//! Shared application-side publish buffer.
//!
//! The buffer batches PHP-to-transport boundary crossings: `Pool::publish`
//! enqueues accepted publications and flushes them in batches once a
//! threshold or interval is reached. The threshold/interval auto-flush is
//! **pipelined** (Round D, issue #41): the batch is spawned on the runtime
//! and `publish` returns before confirmations resolve, while every
//! non-confirmed outcome is surfaced to PHP through the pending-error queue
//! (`drainErrors` / the next publish/flush/pop/stats operation). Explicit
//! flush paths (`flush_all`, teardown) stay synchronous with full-deadline
//! semantics and quiesce outstanding pipelined drains first, so their
//! documented flush-barrier contracts are unchanged.
//!
//! The interval deadline is armed by the first publication of a batch and
//! enforced by a background timer task, so a batch is flushed once its
//! oldest publication is older than the interval even when the process
//! never publishes, pops, or flushes again (a lone FPM publish reaches the
//! broker within the interval instead of sitting in process memory).
//! Consumers additionally hold a clone of this buffer and drain it before
//! waiting for deliveries: this keeps the pop-visible guarantee synchronous
//! even when the configured interval is large.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;

use ext_php_rs::prelude::PhpResult;
use tokio::task::JoinHandle;

use rabbit_rs_core::client::{ClientError, ClientErrorKind, ClientPool};
use rabbit_rs_core::pool::ConnectionHandle;
use rabbit_rs_core::publisher::{PublishOutcome, PublishRequest};

use crate::classes::exception::{backpressure_exception, client_exception, rabbit_exception};
use crate::conversion::NativePublish;

/// Buffer threshold: flush when this many messages are buffered.
pub(crate) const BUFFER_THRESHOLD: usize = 64;
/// Maximum number of buffered publish requests before flushing is forced.
pub(crate) const PUBLISH_BUFFER_MAX_MESSAGES: usize = 4096;
/// Maximum cumulative buffered payload bytes before flushing is forced.
pub(crate) const PUBLISH_BUFFER_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Fixed wall-clock budget for the destructor flush (audit F-18): a stalled
/// broker must not hold process teardown hostage for up to the per-message
/// timeout (30 s default, 24 h ceiling). Explicit `flush()` keeps the
/// caller's full-deadline semantics. The budget is ONE shared deadline across
/// the whole destructor flush: outstanding pipelined drains are quiesced and
/// the still-buffered batch is flushed inside it, not one budget each.
/// `block_on` scheduling overhead around the deadline is not part of the
/// bound.
pub(crate) const TEARDOWN_FLUSH_BUDGET: Duration = Duration::from_millis(500);
/// Cap on concurrent spawned drains: when the cap is hit the flushing
/// `publish()` blocks briefly and then reports backpressure, so the drain
/// pipeline cannot pile up unbounded tasks (Round D Phase 2).
const MAX_CONCURRENT_DRAINS: usize = 8;
/// Cap on pending error records awaiting PHP surfacing. On overflow the
/// oldest record is evicted and counted in `dropped_error_records_total`
/// (surfaced by `stats()`), so records are never lost silently.
const MAX_PENDING_ERRORS: usize = 4096;

/// One non-confirmed publish outcome awaiting PHP surfacing.
///
/// The pipelined drain cannot raise from `publish()` (it already returned),
/// so outcomes land here and surface at the next PHP-visible operation —
/// the same pattern as consumer settlement errors after a pop.
#[derive(Clone, Debug)]
pub struct PendingPublishError {
    pub(crate) message_id: String,
    pub(crate) kind: String,
    pub(crate) message: String,
}

/// Buffered publications and their cumulative payload bytes.
///
/// One mutex guards both so a concurrent `take` can never observe the Vec
/// without its byte accounting (or vice versa): `rebuffer` runs on drain
/// threads while `enqueue`/`take` run on the PHP thread, and split updates
/// left a window where `take` subtracted payload bytes the counter had not
/// credited yet — `attempt to subtract with overflow` under debug builds,
/// poisoned mutexes, and a process abort in the Coverage CI job.
#[derive(Default)]
struct Buffered {
    publishes: Vec<NativePublish>,
    bytes: usize,
}

/// Shared publish buffer with batched flush semantics.
pub struct PublishBuffer {
    client: Arc<ClientPool>,
    handle: Arc<ConnectionHandle>,
    buffer: std::sync::Mutex<Buffered>,
    last_flush: std::sync::Mutex<Option<Instant>>,
    /// Publications discarded without confirmed delivery: deadline-expired
    /// drops in `rebuffer`, unattempted batches on a closing client, and
    /// unconfirmed leftovers at teardown (audit F-18).
    dropped_publications: AtomicU64,
    /// Non-confirmed publish outcomes awaiting PHP surfacing (bounded).
    pending_errors: std::sync::Mutex<VecDeque<PendingPublishError>>,
    /// Pending error records evicted by the bounded queue before PHP could
    /// observe them.
    dropped_error_records: AtomicU64,
    /// Spawned pipelined drains, retained so `flush()`/`close()`/teardown
    /// can quiesce them within the teardown budget.
    drain_handles: std::sync::Mutex<Vec<JoinHandle<()>>>,
    /// Spawned deadline timers, abortable while they sleep: `quiesce`
    /// cancels them so an explicit flush keeps full-deadline semantics over
    /// the still-buffered batch (a timer never holds a taken batch).
    timer_handles: std::sync::Mutex<Vec<JoinHandle<()>>>,
    /// Bounded concurrent spawned drains (backpressure when the pipeline
    /// falls behind production).
    drain_permits: Arc<tokio::sync::Semaphore>,
    /// Set once the destructor flush ran: drains completing afterwards
    /// count their publications as dropped instead of re-buffering them
    /// into a buffer nobody will flush again.
    tearing_down: AtomicBool,
    /// Set while a timer task is scheduled to enforce the current batch's
    /// interval deadline. The flag keeps one timer per batch: the deadline
    /// covers every publication enqueued before it fires.
    timer_pending: AtomicBool,
    /// Time-based flush trigger interval. Wired from the validated
    /// configuration (`publisher.flush_interval`, default 1 millisecond —
    /// issue #194); the test surface overrides it so tests can fill the
    /// buffer past the message ceiling without a flush trigger stealing the
    /// publications mid-fill.
    flush_interval: Duration,
    /// Message-count flush trigger. Defaults to [`BUFFER_THRESHOLD`]; the
    /// test surface overrides it (above the message ceiling) so tests can
    /// drive the buffer to the ceiling through the synchronous overflow
    /// path instead of pipelined drains, whose re-buffer/flush cycling is
    /// scheduling-dependent.
    flush_threshold: usize,
}

impl PublishBuffer {
    pub fn new(
        client: Arc<ClientPool>,
        handle: Arc<ConnectionHandle>,
        flush_interval: Duration,
    ) -> Self {
        Self {
            client,
            handle,
            buffer: std::sync::Mutex::new(Buffered::default()),
            last_flush: std::sync::Mutex::new(None),
            dropped_publications: AtomicU64::new(0),
            pending_errors: std::sync::Mutex::new(VecDeque::new()),
            dropped_error_records: AtomicU64::new(0),
            drain_handles: std::sync::Mutex::new(Vec::new()),
            timer_handles: std::sync::Mutex::new(Vec::new()),
            drain_permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DRAINS)),
            tearing_down: AtomicBool::new(false),
            timer_pending: AtomicBool::new(false),
            flush_interval,
            flush_threshold: BUFFER_THRESHOLD,
        }
    }

    /// Overrides the message-count flush trigger (test surface only: a
    /// threshold above the message ceiling disables the pipelined auto-flush
    /// so the synchronous overflow path drives the ceiling refusal
    /// deterministically).
    #[cfg(feature = "extension-tests")]
    pub(crate) fn with_flush_threshold(mut self, threshold: usize) -> Self {
        self.flush_threshold = threshold;
        self
    }

    /// Returns the number of publications discarded without confirmed
    /// delivery (deadline-expired, closing-client, or teardown drops).
    pub fn dropped_publications(&self) -> u64 {
        self.dropped_publications.load(Ordering::Relaxed)
    }

    /// Returns the number of pending error records evicted before PHP could
    /// observe them.
    pub fn dropped_error_records(&self) -> u64 {
        self.dropped_error_records.load(Ordering::Relaxed)
    }

    /// Records a non-confirmed publish outcome for PHP surfacing.
    ///
    /// The queue is bounded: past [`MAX_PENDING_ERRORS`] the oldest record
    /// is evicted and counted, so a drain storm can never grow memory
    /// unbounded and the loss remains observable via `stats()`.
    fn record_error(&self, error: PendingPublishError) {
        let mut pending = self
            .pending_errors
            .lock()
            .expect("pending publish errors mutex poisoned");
        if pending.len() >= MAX_PENDING_ERRORS {
            pending.pop_front();
            self.dropped_error_records.fetch_add(1, Ordering::Relaxed);
        }
        pending.push_back(error);
    }

    /// Drains and returns every pending publish error record.
    ///
    /// # Panics
    ///
    /// Panics if the guarded internal mutex was poisoned by a panicked drain.
    pub fn take_errors(&self) -> Vec<PendingPublishError> {
        self.pending_errors
            .lock()
            .expect("pending publish errors mutex poisoned")
            .drain(..)
            .collect()
    }

    /// Returns whether the buffer cannot accept `payload_bytes` more bytes.
    ///
    /// # Panics
    ///
    /// Panics if the guarded internal mutex was poisoned by a panicked drain.
    pub fn would_overflow(&self, payload_bytes: usize) -> bool {
        let buffered = self.buffer.lock().expect("publish buffer mutex poisoned");
        buffered.publishes.len() >= PUBLISH_BUFFER_MAX_MESSAGES
            || buffered.bytes + payload_bytes > PUBLISH_BUFFER_MAX_BYTES
    }

    /// Buffers one accepted publication.
    ///
    /// The first publication of a batch arms the interval deadline so a
    /// batch is time-flushed even when it never reaches the size threshold
    /// (issue #96): the deadline is enforced by [`Self::ensure_flush_timer`]
    /// and evaluated by the next publish, whichever comes first.
    ///
    /// Returns whether this publication started a new batch (the buffer was
    /// empty), so the caller can arm the interval timer.
    ///
    /// # Panics
    ///
    /// Panics if the guarded internal mutex was poisoned by a panicked drain.
    pub fn enqueue(&self, publish: NativePublish) -> bool {
        let payload_bytes = publish.request.payload.len();
        let was_empty;
        {
            let mut buffered = self.buffer.lock().expect("publish buffer mutex poisoned");
            was_empty = buffered.publishes.is_empty();
            buffered.publishes.push(publish);
            buffered.bytes += payload_bytes;
        }
        if was_empty {
            *self.last_flush.lock().expect("last_flush mutex poisoned") = Some(Instant::now());
        }
        was_empty
    }

    /// Returns whether the buffer reached a flush trigger.
    ///
    /// The interval clock is armed by the first publication of each batch
    /// (an enqueue into an empty buffer) and reset by every flush, so the
    /// deadline measures how long the oldest buffered publication has been
    /// waiting: a batch is flushed once it is older than the interval even
    /// if it never reaches the size threshold.
    ///
    /// # Panics
    ///
    /// Panics if the guarded internal mutex was poisoned by a panicked drain.
    pub fn should_flush(&self) -> bool {
        self.buffered_len() >= self.flush_threshold
            || self
                .last_flush
                .lock()
                .expect("last_flush mutex poisoned")
                .is_some_and(|instant| instant.elapsed() >= self.flush_interval)
    }

    /// Returns the number of buffered publications.
    ///
    /// # Panics
    ///
    /// Panics if the guarded internal mutex was poisoned by a panicked drain.
    pub fn buffered_len(&self) -> usize {
        self.buffer
            .lock()
            .expect("publish buffer mutex poisoned")
            .publishes
            .len()
    }

    /// Returns the cumulative buffered payload bytes.
    ///
    /// # Panics
    ///
    /// Panics if the guarded internal mutex was poisoned by a panicked drain.
    pub fn buffered_bytes(&self) -> usize {
        self.buffer
            .lock()
            .expect("publish buffer mutex poisoned")
            .bytes
    }

    /// Returns whether the buffer holds at least one publication.
    fn is_empty(&self) -> bool {
        self.buffered_len() == 0
    }

    /// Drains the buffer, keeping the byte counter in sync.
    fn take(&self) -> Vec<NativePublish> {
        let mut buffered = self.buffer.lock().expect("publish buffer mutex poisoned");
        buffered.bytes = 0;
        std::mem::take(&mut buffered.publishes)
    }

    /// Re-buffers publications whose flush failed, keeping the byte counter
    /// in sync. Publications whose deadline already expired are dropped
    /// (counted in `dropped_publications`): they can never succeed and would
    /// poison every subsequent flush.
    /// Re-buffered publications may exceed the buffer ceiling: they were
    /// already accepted (a `message_id` was returned) and are never dropped
    /// while they can still be delivered.
    fn rebuffer(&self, publishes: Vec<NativePublish>) {
        let now = tokio::time::Instant::now();
        let total = publishes.len();
        let retriable: Vec<NativePublish> = publishes
            .into_iter()
            .filter(|publish| publish.request.deadline > now)
            .collect();
        let expired = total - retriable.len();
        if expired > 0 {
            self.dropped_publications.fetch_add(
                u64::try_from(expired).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        let bytes = Self::payload_bytes(&retriable);
        let mut buffered = self.buffer.lock().expect("publish buffer mutex poisoned");
        buffered.publishes.extend(retriable);
        buffered.bytes += bytes;
    }

    /// Disposes of a failed flush's publications: re-buffered while the pool
    /// lives and the buffer can still be flushed, counted as dropped once
    /// teardown started or the pool closed (nobody will flush them again —
    /// audit F-18 semantics).
    fn rebuffer_or_drop(&self, publishes: Vec<NativePublish>) {
        if self.tearing_down.load(Ordering::Acquire) || self.client.is_closed() {
            self.dropped_publications.fetch_add(
                u64::try_from(publishes.len()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            return;
        }
        self.rebuffer(publishes);
    }

    /// Waits for every spawned drain to complete, bounded by the fixed
    /// teardown budget as an overall deadline. Drains that miss the budget
    /// keep running detached on the process-local runtime: they still
    /// process their outcomes and re-buffer (or count as dropped once
    /// teardown started), so no publication is lost silently.
    ///
    /// Called before every synchronous flush so re-buffered publications
    /// are visible to it, and by the explicit `flush()`/`close()`/destructor
    /// paths.
    ///
    /// # Panics
    ///
    /// Panics if the drain-handle mutex was poisoned by a panicked drain.
    pub fn quiesce(&self) {
        self.quiesce_within(tokio::time::Instant::now() + TEARDOWN_FLUSH_BUDGET);
    }

    /// [`Self::quiesce`] bounded by a caller-provided deadline: the
    /// destructor shares one deadline between this wait and the final batch
    /// flush instead of composing two sequential budgets (audit F-18).
    ///
    /// # Panics
    ///
    /// Panics if the drain-handle mutex was poisoned by a panicked drain.
    fn quiesce_within(&self, deadline: tokio::time::Instant) {
        let timers: Vec<JoinHandle<()>> = std::mem::take(
            &mut *self
                .timer_handles
                .lock()
                .expect("timer handles mutex poisoned"),
        );
        for timer in timers {
            timer.abort();
        }
        // An aborted timer never took the batch: the caller's synchronous
        // flush owns it. The reset lets the next batch arm a fresh timer.
        self.timer_pending.store(false, Ordering::Release);
        let handles: Vec<JoinHandle<()>> = std::mem::take(
            &mut *self
                .drain_handles
                .lock()
                .expect("drain handles mutex poisoned"),
        );
        if handles.is_empty() {
            return;
        }
        self.handle.runtime().block_on(async move {
            for handle in handles {
                let _ = tokio::time::timeout_at(deadline, handle).await;
            }
        });
    }

    /// Sends one batch through the client synchronously, re-buffering on
    /// failure. Kept for the explicit full-deadline flush paths.
    fn flush_batch(&self, publishes: Vec<NativePublish>) -> PhpResult<()> {
        if publishes.is_empty() {
            return Ok(());
        }

        // Keep the original requests so a failed flush can re-buffer them.
        let requests = Self::drain_requests(&publishes);

        match self
            .handle
            .runtime()
            .block_on(self.client.publish_batch(requests))
        {
            Ok(outcomes) => {
                // Every outcome is inspected before anything is raised so a
                // failure never short-circuits the buffer decisions. With the
                // current `publish_batch` contract this arm only ever sees
                // `Confirmed` and `Returned` outcomes: per-message failures
                // such as backpressure or timeout are folded into the
                // batch-level `Err` below, which re-buffers every request.
                let mut first_error = None;
                for outcome in outcomes {
                    if let Err(error) = publish_message_id(outcome) {
                        // `Returned` is the only outcome that resolves to an
                        // error here. An unroutable message is definitive:
                        // re-buffering it would loop forever, so the error is
                        // recorded instead and raised once every outcome has
                        // been processed.
                        first_error.get_or_insert(error);
                    }
                }
                first_error.map_or(Ok(()), Err)
            }
            Err(error) => {
                // `publish_batch` discards per-message results after the first
                // terminal failure, so every request of this flush is
                // un-attempted or of unknown state. Conservatively re-buffer
                // the retriable ones, oldest first, so the next flush retries
                // them; duplicates are permitted and identifiable via
                // `message_id`. A closing pool must not re-buffer: those
                // publications can never be sent again, so they are counted
                // as dropped instead of vanishing silently (audit F-18).
                if matches!(error.kind(), ClientErrorKind::Closed) {
                    self.dropped_publications.fetch_add(
                        u64::try_from(publishes.len()).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                } else {
                    self.rebuffer_or_drop(publishes);
                }
                client_exception(&error)
            }
        }
    }

    /// Flushes the buffer by spawning the batch on the runtime (pipelined).
    ///
    /// Called by the `publish()` auto-flush triggers. The PHP thread returns
    /// immediately; the spawned drain awaits the batch outcomes and records
    /// every non-confirmed outcome in the pending-error queue for PHP to
    /// surface at the next operation. Backpressure when the drain falls
    /// behind: the caller briefly waits for a drain slot, and past the
    /// budget the publications are re-buffered and a `BackpressureException`
    /// is raised.
    fn flush_pipelined(self: &Arc<Self>, publishes: Vec<NativePublish>) -> PhpResult<()> {
        if publishes.is_empty() {
            return Ok(());
        }

        // Bounded pile-up: block briefly for a drain slot. Steady state at
        // the measured ceiling holds ~1-3 concurrent drains, so the cap
        // never binds in normal operation. The timeout is created inside
        // the async block so the timer registers on the runtime the
        // `block_on` enters.
        let Ok(Ok(permit)) = self.handle.runtime().block_on(async {
            tokio::time::timeout(
                TEARDOWN_FLUSH_BUDGET,
                self.drain_permits.clone().acquire_owned(),
            )
            .await
        }) else {
            self.rebuffer_or_drop(publishes);
            // The re-buffered batch must still be flushed within the flush
            // interval (issue #218): arm the timer so a saturated pipeline
            // retries it even if PHP never publishes again. Idempotent: a
            // timer already covering the batch makes this a no-op.
            self.ensure_flush_timer();
            return backpressure_exception(
                "publish drain pipeline is saturated; retry after flush",
            );
        };

        // Keep the original publications so a failed drain can re-buffer a
        // conservative superset (the same contract as the sync flush).
        let requests = Self::drain_requests(&publishes);

        let buffer = Arc::clone(self);
        let task = self.handle.runtime().spawn(async move {
            // The permit is held for the drain's whole life so concurrent
            // spawned drains stay bounded.
            let _permit = permit;
            buffer.run_drain(publishes, requests).await;
        });
        {
            // Push and prune under one lock: completed drains are pruned on
            // every push so a publish-only process cannot accumulate one
            // finished handle per flush cycle. `is_finished` stays false for
            // aborted-but-running drains, so quiesce's abort-safety
            // semantics are unchanged.
            let mut handles = self
                .drain_handles
                .lock()
                .expect("drain handles mutex poisoned");
            handles.push(task);
            handles.retain(|handle| !handle.is_finished());
        }
        Ok(())
    }

    /// Drains the buffer and spawns the batch on the runtime.
    ///
    /// # Errors
    ///
    /// Raises the backpressure exception when no drain slot frees up within
    /// the teardown budget: the batch is re-buffered (or counted as dropped
    /// once teardown started), so nothing is lost.
    pub fn flush_triggered(self: &Arc<Self>) -> PhpResult<()> {
        let publishes = self.take();
        self.flush_pipelined(publishes)
    }

    /// Arms the interval deadline for a freshly started batch.
    ///
    /// Without this, the deadline is evaluated only by the next `publish()`
    /// and a process that stops publishing holds the batch in memory until
    /// a pop, an explicit flush, or close (issue #96 regression report:
    /// lone FPM publishes invisible for 15 s+). The timer enforces the
    /// documented `flush_interval` contract: the batch is flushed once its
    /// oldest publication is older than the interval, even with no further
    /// PHP operation.
    ///
    /// # Panics
    ///
    /// Panics if the timer-handle mutex was poisoned by a panicked drain.
    pub fn ensure_flush_timer(self: &Arc<Self>) {
        if self.timer_pending.swap(true, Ordering::AcqRel) {
            // A timer already covers the current batch: its deadline is the
            // oldest publication's, so the whole batch flushes on time.
            return;
        }
        let buffer = Arc::clone(self);
        let task = self.handle.runtime().spawn(async move {
            buffer.run_flush_timer().await;
        });
        {
            // Bounded tracking: a finished (fired or aborted) timer handle is
            // pruned when the next timer is armed, so repeated arm/fire
            // cycles cannot accumulate handles.
            let mut handles = self
                .timer_handles
                .lock()
                .expect("timer handles mutex poisoned");
            handles.push(task);
            handles.retain(|handle| !handle.is_finished());
        }
    }

    /// Enforces the armed batch deadline (runs on the runtime).
    ///
    /// The timer never drains inline: it hands the batch off to a spawned
    /// drain registered with the other pipelined drains, so aborting a
    /// sleeping timer during [`Self::quiesce`] can never cancel a batch
    /// that was already taken. An explicit flush therefore keeps
    /// full-deadline semantics: quiesce aborts the sleeping timer, the
    /// batch stays buffered, and the synchronous flush owns it.
    async fn run_flush_timer(self: Arc<Self>) {
        tokio::time::sleep(self.flush_interval).await;
        self.timer_pending.store(false, Ordering::Release);
        if !self.should_flush() {
            return;
        }
        let buffer = Arc::clone(&self);
        let task = self.handle.runtime().spawn(async move {
            buffer.run_timer_drain().await;
        });
        {
            // Same bounded tracking as `flush_pipelined`: prune completed
            // handles on push so a long-lived process with a short interval
            // cannot accumulate finished timer-initiated drains.
            let mut handles = self
                .drain_handles
                .lock()
                .expect("drain handles mutex poisoned");
            handles.push(task);
            handles.retain(|handle| !handle.is_finished());
        }
    }

    /// Timer-initiated pipelined drain (runs on the runtime). Mirrors
    /// `flush_pipelined` without its synchronous permit wait: this task
    /// already runs on the runtime, where `block_on` would panic. On
    /// saturation the batch is re-buffered with a fresh flush timer armed and
    /// the backpressure surfaces at the next operation, like every other
    /// non-confirmed outcome.
    async fn run_timer_drain(self: Arc<Self>) {
        let publishes = self.take();
        if publishes.is_empty() {
            return;
        }
        let requests = Self::drain_requests(&publishes);
        let permit = tokio::time::timeout(
            TEARDOWN_FLUSH_BUDGET,
            self.drain_permits.clone().acquire_owned(),
        )
        .await;
        let Ok(Ok(permit)) = permit else {
            let message_id = publishes
                .first()
                .map(|publish| publish.request.properties.message_id.as_ref().to_owned())
                .unwrap_or_default();
            self.rebuffer_or_drop(publishes);
            // Same interval contract as the synchronous saturation path: the
            // re-buffered batch keeps a flush timer armed (idempotent — the
            // timer that spawned this drain already cleared `timer_pending`).
            self.ensure_flush_timer();
            self.record_error(PendingPublishError {
                message_id,
                kind: "Backpressure".to_owned(),
                message: "publish drain pipeline is saturated; retry after flush".to_owned(),
            });
            return;
        };
        let _permit = permit;
        self.run_drain(publishes, requests).await;
    }

    /// Processes one spawned batch's outcomes (runs on the runtime).
    ///
    /// Contract (at-least-once): confirmed publications are released;
    /// returned publications are recorded for PHP surfacing and never
    /// re-buffered (unroutable is definitive); a batch-level failure
    /// re-buffers the whole batch (conservative superset — duplicates are
    /// permitted and identifiable via `message_id`) or counts it as dropped
    /// on a closing pool/teardown; the failure is recorded for PHP
    /// surfacing either way.
    async fn run_drain(
        &self,
        publishes: Vec<NativePublish>,
        requests: Vec<(Arc<str>, PublishRequest)>,
    ) {
        let message_id = publishes
            .first()
            .map(|publish| publish.request.properties.message_id.as_ref().to_owned())
            .unwrap_or_default();
        match self.client.publish_batch(requests).await {
            Ok(outcomes) => {
                for outcome in outcomes {
                    if let PublishOutcome::Returned { message_id, reply } = outcome {
                        self.record_error(PendingPublishError {
                            message_id: message_id.as_ref().to_owned(),
                            kind: "Returned".to_owned(),
                            message: format!(
                                "message {message_id} was returned as unroutable (AMQP {})",
                                reply.code
                            ),
                        });
                    }
                }
            }
            Err(error) => {
                if matches!(error.kind(), ClientErrorKind::Closed) {
                    self.dropped_publications.fetch_add(
                        u64::try_from(publishes.len()).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                } else {
                    self.rebuffer_or_drop(publishes);
                }
                self.record_error(PendingPublishError {
                    message_id,
                    kind: client_error_kind(&error).to_owned(),
                    message: error.to_string(),
                });
            }
        }
    }

    /// Flushes buffered publications under the fixed teardown budget.
    ///
    /// Used by the destructor path only: unlike `flush_all`, failures are
    /// never re-buffered — the process is going away, so anything the budget
    /// could not confirm is counted in `dropped_publications` and released.
    /// `tearing_down` is set before quiescing so every drain completing from
    /// here on counts its publications as dropped instead of re-buffering
    /// them into a buffer whose only remaining flush is the one below
    /// (setting the flag after quiesce left a microsecond window where a
    /// drain re-buffered after the batch was taken, uncounted). Quiesce and
    /// the batch flush share one [`TEARDOWN_FLUSH_BUDGET`] deadline.
    pub(crate) fn flush_teardown(&self) {
        self.tearing_down.store(true, Ordering::Release);
        let deadline = tokio::time::Instant::now() + TEARDOWN_FLUSH_BUDGET;
        self.quiesce_within(deadline);
        if self.is_empty() {
            return;
        }
        let publishes = self.take();
        let requests = Self::drain_requests(&publishes);
        let attempted = self.handle.runtime().block_on(async {
            tokio::time::timeout_at(deadline, self.client.publish_batch(requests)).await
        });
        match attempted {
            Ok(Ok(outcomes)) => {
                // Every outcome is inspected: `Confirmed` deliveries need no
                // accounting, but a `Returned` outcome is not a confirmation —
                // it is a definitive unroutable disposition, recorded for PHP
                // surfacing exactly like the pipelined drains record it
                // instead of being treated as delivered.
                for outcome in outcomes {
                    if let PublishOutcome::Returned { message_id, reply } = outcome {
                        self.record_error(PendingPublishError {
                            message_id: message_id.as_ref().to_owned(),
                            kind: "Returned".to_owned(),
                            message: format!(
                                "message {message_id} was returned as unroutable (AMQP {})",
                                reply.code
                            ),
                        });
                    }
                }
            }
            // Wall-clock timeout or batch-level failure: the outcomes are of
            // unknown state, so every publication of the batch is counted as
            // dropped exactly once (audit F-18). Confirmations that land
            // after the budget stay unobservable by design — the client
            // metrics still count them.
            Err(_) | Ok(Err(_)) => {
                self.dropped_publications.fetch_add(
                    u64::try_from(publishes.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
        }
    }

    /// Flushes every buffered publication synchronously (full-deadline
    /// semantics). Outstanding pipelined drains are quiesced first, bounded
    /// by the fixed teardown budget, so their re-buffered publications are
    /// visible to this drain.
    pub(crate) fn flush_all(&self) -> PhpResult<()> {
        self.quiesce();
        *self.last_flush.lock().expect("last_flush mutex poisoned") = Some(Instant::now());
        let publishes = self.take();
        self.flush_batch(publishes)
    }

    /// Flushes the buffer when it holds publications; a no-op otherwise.
    ///
    /// Consumers call this before waiting for deliveries so publications
    /// accepted earlier are visible to the broker before the consumer blocks.
    /// Outstanding pipelined drains are quiesced either way: publications
    /// already handed to a spawned drain must reach the broker before the
    /// pop blocks.
    pub(crate) fn flush_nonempty(&self) -> PhpResult<()> {
        if self.is_empty() {
            self.quiesce();
            return Ok(());
        }
        self.flush_all()
    }

    /// Clones the batch into the wire-level request list a failed flush
    /// re-buffers (shared by the sync, pipelined, and teardown drains).
    ///
    /// The broker key is an `Arc<str>` bump and `PublishRequest` clones are
    /// zero-copy (payload `Bytes`, `Arc`-held properties), so this is
    /// allocation-free for the common single-headerless case (issue #261).
    fn drain_requests(publishes: &[NativePublish]) -> Vec<(Arc<str>, PublishRequest)> {
        publishes
            .iter()
            .map(|publish| (publish.broker.clone(), publish.request.clone()))
            .collect()
    }

    /// Total payload bytes of the given buffered publications.
    fn payload_bytes(publishes: &[NativePublish]) -> usize {
        publishes
            .iter()
            .map(|publish| publish.request.payload.len())
            .sum()
    }
}

/// Maps a batch-level failure to the pending-error kind that PHP maps to an
/// exception class (mirroring `client_exception`).
fn client_error_kind(error: &ClientError) -> &'static str {
    match error.kind() {
        ClientErrorKind::Backpressure => "Backpressure",
        ClientErrorKind::Transport => "Transport",
        ClientErrorKind::Closed => "Closed",
        ClientErrorKind::Configuration => "Configuration",
        ClientErrorKind::Publish | ClientErrorKind::Consumer => "Publish",
    }
}

pub(crate) fn publish_message_id(outcome: PublishOutcome) -> PhpResult<String> {
    match outcome {
        PublishOutcome::Confirmed { message_id } => Ok(message_id.as_ref().to_owned()),
        PublishOutcome::Returned { message_id, reply } => rabbit_exception(format!(
            "message {message_id} was returned as unroutable (AMQP {})",
            reply.code
        )),
    }
}

/// Focused unit tests for the buffer-internal invariants the state-machine
/// harness cannot reach: the bounded handle-tracking vecs, the saturation-path
/// timer re-arm, and teardown ordering/accounting. Time runs real — the
/// buffer's `Handle::block_on` facade requires a multi-thread runtime, where
/// paused Tokio time is unsupported (see the state-machine harness) — so
/// waits are bounded real-time polls against generous ceilings, never bare
/// sleeps.
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use zend_link_stubs as _;

    use rabbit_rs_core::config::{
        BrokerConfig, Config, Credentials, DelayConfig, Endpoint, PublisherConfigSection,
        TlsConfig, TopologyMode,
    };
    use rabbit_rs_core::publisher::{Destination, MessageProperties};
    use rabbit_rs_core::runtime::{PidProvider, RuntimeFactory, RuntimeRegistry};
    use rabbit_rs_core::transport::mock::{MockTransport, TransportOperation};
    use rabbit_rs_core::transport::{PublishConfirmation, ReturnedMessage, TransportError};

    use super::*;

    /// Timer-inert interval for tests that must not depend on the background
    /// timer (oversized, like the state-machine harness's interval).
    const NO_TIMER: Duration = Duration::from_secs(3600);
    /// Short interval proving the saturation path re-arms the flush timer.
    const SHORT_INTERVAL: Duration = Duration::from_millis(10);
    /// Validity window of a healthy publication: a case finishes in
    /// milliseconds, so healthy deadlines never expire mid-case.
    const HEALTHY_DEADLINE: Duration = Duration::from_secs(10);
    /// How long a released gate stays comfortably inside quiesce's 500 ms
    /// wait, so a mid-quiesce drain completion is deterministic.
    const QUIESCE_RELEASE_DELAY: Duration = Duration::from_millis(20);
    const PAYLOAD: &[u8] = b"job";
    const BROKER: &str = "main";

    struct FixedPid;

    impl PidProvider for FixedPid {
        fn current_pid(&self) -> u32 {
            424_242
        }
    }

    /// The production runtime shape: a single-worker multi-thread runtime.
    struct BackgroundRuntimeFactory;

    impl RuntimeFactory for BackgroundRuntimeFactory {
        fn create(&self) -> std::io::Result<tokio::runtime::Runtime> {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
        }
    }

    /// One buffer wired to a real client pool over the scriptable mock
    /// transport (same shape as the state-machine harness).
    struct Fixture {
        transport: Arc<MockTransport>,
        handle: Arc<ConnectionHandle>,
        buffer: Arc<PublishBuffer>,
        /// Owns the runtime the handle borrows; declared last so it drops
        /// after every other field.
        _registry: RuntimeRegistry,
    }

    impl Fixture {
        fn new(flush_interval: Duration) -> Self {
            let transport = Arc::new(MockTransport::default());
            let registry = RuntimeRegistry::with_dependencies(
                Arc::new(FixedPid),
                Arc::new(BackgroundRuntimeFactory),
            );
            let config: Arc<rabbit_rs_core::config::ValidatedConfig> = Config {
                brokers: vec![BrokerConfig {
                    name: BROKER.to_owned(),
                    hosts: vec![Endpoint::new("rabbit.local", 5672)],
                    vhost: "/".to_owned(),
                    credentials: Credentials::new("guest", "secret"),
                    tls: TlsConfig::disabled(),
                    heartbeat: Duration::from_secs(30),
                }],
                workers: Vec::new(),
                topology_mode: TopologyMode::External,
                routes: std::collections::BTreeMap::new(),
                delay: DelayConfig::default(),
                dead_letter: None,
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
                consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
                queue_type: rabbit_rs_core::transport::QueueKind::Quorum,
                queue_durable: true,
            }
            .validate()
            .expect("valid config")
            .into();
            let handle = registry
                .acquire(rabbit_rs_core::pool::ConnectionKey::from_config(&config))
                .expect("connection handle");
            let client = Arc::new(ClientPool::new(
                Arc::clone(&config),
                Arc::clone(&transport) as _,
            ));
            let buffer = Arc::new(PublishBuffer::new(
                client,
                Arc::clone(&handle),
                flush_interval,
            ));
            Fixture {
                transport,
                handle,
                buffer,
                _registry: registry,
            }
        }

        /// One healthy publication carrying the given message id.
        fn publish(id: u32) -> NativePublish {
            NativePublish {
                broker: BROKER.into(),
                request: PublishRequest::new(
                    Destination::new("jobs", "orders"),
                    Bytes::from_static(PAYLOAD),
                    MessageProperties::new(format!("{id}")),
                    tokio::time::Instant::now() + HEALTHY_DEADLINE,
                ),
            }
        }

        /// Message ids of every publish observed on the wire, in send order.
        fn wire_ids(&self) -> Vec<String> {
            self.transport
                .operations()
                .into_iter()
                .filter_map(|operation| match operation {
                    TransportOperation::Publish(request) => request
                        .properties
                        .message_id
                        .as_ref()
                        .map(ToOwned::to_owned),
                    _ => None,
                })
                .collect()
        }

        fn wire_count(&self, id: &str) -> usize {
            self.wire_ids()
                .iter()
                .filter(|sent| sent.as_str() == id)
                .count()
        }

        fn drain_handle_count(&self) -> usize {
            self.buffer
                .drain_handles
                .lock()
                .expect("drain handles mutex poisoned")
                .len()
        }
    }

    /// Bounded real-time wait for `condition`. The buffer's `block_on` facade
    /// requires a multi-thread runtime, where paused Tokio time is unsupported
    /// (see the state-machine harness), so time runs real: the poll loop
    /// sleeps on the runtime in 2 ms ticks until the condition holds or the
    /// timeout elapses. Returns whether the condition was observed — callers
    /// assert.
    fn wait_for(fixture: &Fixture, timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        fixture.handle.runtime().block_on(async {
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                if condition() {
                    return true;
                }
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    }

    /// Sustained pipelined flushing without any `quiesce()` must not
    /// accumulate one finished `JoinHandle` per flush cycle: the tracking vec
    /// is pruned on every push and stays bounded by the concurrent-drain cap.
    #[test]
    fn pipelined_flush_cycles_prune_completed_drain_handles() {
        let fixture = Fixture::new(NO_TIMER);
        for id in 0..10_000u32 {
            fixture
                .transport
                .push_confirmation(Ok(PublishConfirmation::Ack(None)));
            let publish = Fixture::publish(id);
            fixture.buffer.enqueue(publish);
            fixture.buffer.flush_triggered().expect("pipelined flush");
        }
        // No quiesce, no explicit flush: only the per-push pruning bounds the
        // vec. Today every completed handle is retained until a quiesce.
        let handles = fixture.drain_handle_count();
        assert!(
            handles <= 64,
            "completed drain handles must be pruned on push; {handles} accumulated"
        );
        assert_eq!(fixture.buffer.dropped_publications(), 0);
    }

    /// A permit-timeout re-buffer must leave the batch with an armed flush
    /// timer: the re-buffered publication is retried within the flush
    /// interval even though PHP never publishes again (issue #218 contract).
    #[test]
    fn saturation_rebuffer_rearms_the_flush_timer() {
        let fixture = Fixture::new(SHORT_INTERVAL);
        // Saturate the drain pipeline: every permit held by a drain parked on
        // a controlled confirmation that stays unresolved (healthy deadline,
        // so no expiry).
        let drain_cap = u32::try_from(MAX_CONCURRENT_DRAINS).expect("small const");
        let mut controllers = Vec::new();
        for id in 0..drain_cap {
            controllers.push(fixture.transport.push_controlled_confirmation());
            let publish = Fixture::publish(id);
            fixture.buffer.enqueue(publish);
            fixture.buffer.flush_triggered().expect("drain spawned");
        }
        // One publication more: the flushing publish times out waiting for a
        // drain slot, re-buffers the batch and raises backpressure.
        fixture.buffer.enqueue(Fixture::publish(8));
        let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fixture.buffer.flush_triggered()
        }));
        assert!(raised.is_err(), "a saturated flush must raise backpressure");
        assert_eq!(fixture.buffer.buffered_len(), 1);
        // The re-buffered batch must not wait for the next publish: a timer
        // covers it (the oversized interval in the other tests keeps the
        // timer inert, so the saturation path is the only arming here).
        assert!(
            fixture.buffer.timer_pending.load(Ordering::Acquire),
            "a re-buffered batch must have a flush timer armed"
        );
        // Free the pipeline and script the retry's confirmation: the armed
        // timer must flush the re-buffered publication within the interval.
        for controller in &controllers {
            controller.resolve(Ok(PublishConfirmation::Ack(None)));
        }
        fixture
            .transport
            .push_confirmation(Ok(PublishConfirmation::Ack(None)));
        let flushed = wait_for(&fixture, Duration::from_secs(5), || {
            fixture.wire_ids().len() == MAX_CONCURRENT_DRAINS + 1
        });
        assert!(
            flushed,
            "the re-buffered batch must flush within the flush interval (wire: {:?})",
            fixture.wire_ids()
        );
        assert_eq!(fixture.buffer.buffered_len(), 0);
        assert_eq!(fixture.buffer.dropped_publications(), 0);
        assert!(fixture.buffer.take_errors().is_empty());
    }

    /// The teardown flush's `Ok` batch must not treat `Returned` outcomes as
    /// confirmed deliveries: they are definitive unroutable dispositions,
    /// recorded for PHP surfacing exactly like pipelined drains record them.
    #[test]
    fn teardown_reports_returned_publications_distinctly() {
        let fixture = Fixture::new(NO_TIMER);
        fixture.buffer.enqueue(Fixture::publish(0));
        fixture.buffer.enqueue(Fixture::publish(1));
        fixture
            .transport
            .push_confirmation(Ok(PublishConfirmation::Ack(Some(ReturnedMessage {
                reply_code: 312,
                reply_text: "NO_ROUTE".to_owned(),
                exchange: "jobs".to_owned(),
                routing_key: "orders".to_owned(),
                payload: Bytes::from_static(PAYLOAD),
            }))));
        fixture
            .transport
            .push_confirmation(Ok(PublishConfirmation::Ack(None)));
        fixture.buffer.flush_teardown();
        assert_eq!(fixture.buffer.dropped_publications(), 0);
        assert_eq!(fixture.buffer.buffered_len(), 0);
        let errors = fixture.buffer.take_errors();
        assert_eq!(
            errors.len(),
            1,
            "the returned publication must be recorded, not treated as confirmed"
        );
        assert_eq!(errors[0].message_id, "0");
        assert_eq!(errors[0].kind, "Returned");
    }

    /// `tearing_down` is set before quiesce: a drain failing during the
    /// quiesce window must count its batch as dropped, not re-buffer it into
    /// the teardown flush. The pre-fix ordering re-sent the already-failed
    /// batch on the wire after its terminal resolution.
    #[test]
    fn teardown_sets_tearing_down_before_quiescing_drains() {
        let fixture = Fixture::new(NO_TIMER);
        let gate = fixture.transport.push_publish_gate();
        fixture.buffer.enqueue(Fixture::publish(0));
        fixture.buffer.flush_triggered().expect("drain spawned");
        // The drain is parked on the gated wire write; the second publication
        // stays buffered for the teardown batch flush.
        fixture.buffer.enqueue(Fixture::publish(1));
        // Release the gated send while teardown's quiesce is still awaiting
        // the drain: it then fails (scripted protocol error) mid-quiesce.
        let transport = Arc::clone(&fixture.transport);
        fixture.handle.runtime().spawn(async move {
            tokio::time::sleep(QUIESCE_RELEASE_DELAY).await;
            let _released = gate.release();
            transport.push_confirmation(Err(TransportError::protocol(
                "simulated non-recoverable publish failure",
            )));
        });
        fixture.buffer.flush_teardown();
        // Message 0 reached the wire exactly once (the gated send); the
        // failed drain's batch was counted as dropped, never re-buffered and
        // re-sent by the teardown flush. Message 1 went out once too.
        assert_eq!(
            fixture.wire_count("0"),
            1,
            "a drain failing under teardown must not re-buffer its batch (wire: {:?})",
            fixture.wire_ids()
        );
        assert_eq!(fixture.buffer.dropped_publications(), 2);
        assert_eq!(fixture.buffer.buffered_len(), 0);
    }

    /// The destructor composes quiesce and the final batch flush inside ONE
    /// teardown budget: a drain parked past the budget must not buy the batch
    /// flush a second full budget (audit F-18's single fixed shutdown
    /// ceiling).
    #[test]
    fn teardown_bounds_quiesce_and_batch_flush_in_one_budget() {
        let fixture = Fixture::new(NO_TIMER);
        // Park a drain forever on a gated wire write: quiesce spends its
        // whole wait on it.
        let _gate = fixture.transport.push_publish_gate();
        fixture.buffer.enqueue(Fixture::publish(0));
        fixture.buffer.flush_triggered().expect("drain spawned");
        // One buffered publication for the teardown batch flush, parked on a
        // pending confirmation so the batch flush consumes its whole budget.
        fixture.buffer.enqueue(Fixture::publish(1));
        fixture.transport.push_pending_confirmation();
        let started = std::time::Instant::now();
        fixture.buffer.flush_teardown();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(750),
            "teardown must bound quiesce + batch flush in one shared 500 ms budget; took {elapsed:?}"
        );
        // The buffered publication could not be confirmed within the
        // remaining budget: counted as dropped exactly once.
        assert_eq!(fixture.buffer.dropped_publications(), 1);
    }
}
