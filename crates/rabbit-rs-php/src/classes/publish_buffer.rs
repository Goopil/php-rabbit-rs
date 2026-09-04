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
//! Because the flush triggers only run on publish calls, a publication can
//! otherwise remain buffered while the process stops publishing — a consumer
//! created afterwards would starve waiting for messages that only exist in
//! process memory. Consumers therefore hold a clone of this buffer and drain
//! it before waiting for deliveries, so anything accepted before a pop is
//! visible to that pop.

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
/// Maximum time to wait before flushing the buffer.
pub(crate) const BUFFER_FLUSH_INTERVAL: Duration = Duration::from_millis(1);
/// Maximum number of buffered publish requests before flushing is forced.
pub(crate) const PUBLISH_BUFFER_MAX_MESSAGES: usize = 4096;
/// Maximum cumulative buffered payload bytes before flushing is forced.
pub(crate) const PUBLISH_BUFFER_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Fixed wall-clock budget for the destructor flush (audit F-18): a stalled
/// broker must not hold process teardown hostage for up to the per-message
/// timeout (30 s default, 24 h ceiling). Explicit `flush()` keeps the
/// caller's full-deadline semantics.
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
pub(crate) struct PendingPublishError {
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
pub(crate) struct PublishBuffer {
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
    /// Bounded concurrent spawned drains (backpressure when the pipeline
    /// falls behind production).
    drain_permits: Arc<tokio::sync::Semaphore>,
    /// Set once the destructor flush ran: drains completing afterwards
    /// count their publications as dropped instead of re-buffering them
    /// into a buffer nobody will flush again.
    tearing_down: AtomicBool,
}

impl PublishBuffer {
    pub(crate) fn new(client: Arc<ClientPool>, handle: Arc<ConnectionHandle>) -> Self {
        Self {
            client,
            handle,
            buffer: std::sync::Mutex::new(Buffered::default()),
            last_flush: std::sync::Mutex::new(None),
            dropped_publications: AtomicU64::new(0),
            pending_errors: std::sync::Mutex::new(VecDeque::new()),
            dropped_error_records: AtomicU64::new(0),
            drain_handles: std::sync::Mutex::new(Vec::new()),
            drain_permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DRAINS)),
            tearing_down: AtomicBool::new(false),
        }
    }

    /// Returns the number of publications discarded without confirmed
    /// delivery (deadline-expired, closing-client, or teardown drops).
    pub(crate) fn dropped_publications(&self) -> u64 {
        self.dropped_publications.load(Ordering::Relaxed)
    }

    /// Returns the number of pending error records evicted before PHP could
    /// observe them.
    pub(crate) fn dropped_error_records(&self) -> u64 {
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
    pub(crate) fn take_errors(&self) -> Vec<PendingPublishError> {
        self.pending_errors
            .lock()
            .expect("pending publish errors mutex poisoned")
            .drain(..)
            .collect()
    }

    /// Returns whether the buffer cannot accept `payload_bytes` more bytes.
    pub(crate) fn would_overflow(&self, payload_bytes: usize) -> bool {
        let buffered = self.buffer.lock().expect("publish buffer mutex poisoned");
        buffered.publishes.len() >= PUBLISH_BUFFER_MAX_MESSAGES
            || buffered.bytes + payload_bytes > PUBLISH_BUFFER_MAX_BYTES
    }

    /// Buffers one accepted publication.
    ///
    /// The first publication of a batch arms the interval deadline so a
    /// batch is time-flushed even when it never reaches the size threshold
    /// (issue #96): a fresh pool's first publish would otherwise sit in the
    /// buffer until the threshold, a drain, or an explicit flush.
    pub(crate) fn enqueue(&self, publish: NativePublish) {
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
    }

    /// Returns whether the buffer reached a flush trigger.
    ///
    /// The interval clock is armed by the first publication of each batch
    /// (an enqueue into an empty buffer) and reset by every flush, so the
    /// deadline measures how long the oldest buffered publication has been
    /// waiting: a batch is flushed once it is older than the interval even
    /// if it never reaches the size threshold.
    pub(crate) fn should_flush(&self) -> bool {
        self.buffered_len() >= BUFFER_THRESHOLD
            || self
                .last_flush
                .lock()
                .expect("last_flush mutex poisoned")
                .is_some_and(|instant| instant.elapsed() >= BUFFER_FLUSH_INTERVAL)
    }

    /// Returns the number of buffered publications.
    pub(crate) fn buffered_len(&self) -> usize {
        self.buffer
            .lock()
            .expect("publish buffer mutex poisoned")
            .publishes
            .len()
    }

    /// Returns the cumulative buffered payload bytes.
    pub(crate) fn buffered_bytes(&self) -> usize {
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
    pub(crate) fn quiesce(&self) {
        let handles: Vec<JoinHandle<()>> = std::mem::take(
            &mut *self
                .drain_handles
                .lock()
                .expect("drain handles mutex poisoned"),
        );
        if handles.is_empty() {
            return;
        }
        let deadline = tokio::time::Instant::now() + TEARDOWN_FLUSH_BUDGET;
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
        let requests: Vec<(String, PublishRequest)> = publishes
            .iter()
            .map(|publish| (publish.broker.clone(), publish.request.clone()))
            .collect();

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
            return backpressure_exception(
                "publish drain pipeline is saturated; retry after flush",
            );
        };

        // Keep the original publications so a failed drain can re-buffer a
        // conservative superset (the same contract as the sync flush).
        let requests: Vec<(String, PublishRequest)> = publishes
            .iter()
            .map(|publish| (publish.broker.clone(), publish.request.clone()))
            .collect();

        let buffer = Arc::clone(self);
        let task = self.handle.runtime().spawn(async move {
            // The permit is held for the drain's whole life so concurrent
            // spawned drains stay bounded.
            let _permit = permit;
            buffer.run_drain(publishes, requests).await;
        });
        self.drain_handles
            .lock()
            .expect("drain handles mutex poisoned")
            .push(task);
        Ok(())
    }

    /// Drains the buffer and spawns the batch on the runtime.
    pub(crate) fn flush_triggered(self: &Arc<Self>) -> PhpResult<()> {
        let publishes = self.take();
        self.flush_pipelined(publishes)
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
        requests: Vec<(String, PublishRequest)>,
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
    /// Outstanding pipelined drains are quiesced first (within the same
    /// budget); a drain completing after this point counts its publications
    /// as dropped instead of re-buffering them (`tearing_down`).
    pub(crate) fn flush_teardown(&self) {
        self.quiesce();
        self.tearing_down.store(true, Ordering::Release);
        if self.is_empty() {
            return;
        }
        let publishes = self.take();
        let requests: Vec<(String, PublishRequest)> = publishes
            .iter()
            .map(|publish| (publish.broker.clone(), publish.request.clone()))
            .collect();
        let confirmed = self
            .handle
            .runtime()
            .block_on(async {
                tokio::time::timeout(TEARDOWN_FLUSH_BUDGET, self.client.publish_batch(requests))
                    .await
            })
            .is_ok_and(|result| result.is_ok());
        if !confirmed {
            self.dropped_publications.fetch_add(
                u64::try_from(publishes.len()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
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
