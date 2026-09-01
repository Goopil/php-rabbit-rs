//! Shared application-side publish buffer.
//!
//! The buffer batches PHP-to-transport boundary crossings: `Pool::publish`
//! enqueues accepted publications and flushes them in batches once a
//! threshold or interval is reached. Because the flush triggers only run on
//! publish calls, a publication can otherwise remain buffered while the
//! process stops publishing — a consumer created afterwards would starve
//! waiting for messages that only exist in process memory. Consumers
//! therefore hold a clone of this buffer and drain it before waiting for
//! deliveries, so anything accepted before a pop is visible to that pop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;

use ext_php_rs::prelude::PhpResult;

use rabbit_rs_core::client::{ClientErrorKind, ClientPool};
use rabbit_rs_core::pool::ConnectionHandle;
use rabbit_rs_core::publisher::{PublishOutcome, PublishRequest};

use crate::classes::exception::{client_exception, rabbit_exception};
use crate::conversion::NativePublish;

/// Buffer threshold: flush when this many messages are buffered.
pub(crate) const BUFFER_THRESHOLD: usize = 64;
/// Maximum time to wait before flushing the buffer.
pub(crate) const BUFFER_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);
/// Maximum number of buffered publish requests before flushing is forced.
pub(crate) const PUBLISH_BUFFER_MAX_MESSAGES: usize = 4096;
/// Maximum cumulative buffered payload bytes before flushing is forced.
pub(crate) const PUBLISH_BUFFER_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Fixed wall-clock budget for the destructor flush (audit F-18): a stalled
/// broker must not hold process teardown hostage for up to the per-message
/// timeout (30 s default, 24 h ceiling). Explicit `flush()` keeps the
/// caller's full-deadline semantics.
pub(crate) const TEARDOWN_FLUSH_BUDGET: Duration = Duration::from_millis(500);

/// Shared publish buffer with batched flush semantics.
pub(crate) struct PublishBuffer {
    client: Arc<ClientPool>,
    handle: Arc<ConnectionHandle>,
    buffer: std::sync::Mutex<Vec<NativePublish>>,
    buffered_bytes: std::sync::Mutex<usize>,
    last_flush: std::sync::Mutex<Option<Instant>>,
    /// Publications discarded without confirmed delivery: deadline-expired
    /// drops in `rebuffer`, unattempted batches on a closing client, and
    /// unconfirmed leftovers at teardown (audit F-18).
    dropped_publications: AtomicU64,
}

impl PublishBuffer {
    pub(crate) fn new(client: Arc<ClientPool>, handle: Arc<ConnectionHandle>) -> Self {
        Self {
            client,
            handle,
            buffer: std::sync::Mutex::new(Vec::with_capacity(BUFFER_THRESHOLD)),
            buffered_bytes: std::sync::Mutex::new(0),
            last_flush: std::sync::Mutex::new(None),
            dropped_publications: AtomicU64::new(0),
        }
    }

    /// Returns the number of publications discarded without confirmed
    /// delivery (deadline-expired, closing-client, or teardown drops).
    pub(crate) fn dropped_publications(&self) -> u64 {
        self.dropped_publications.load(Ordering::Relaxed)
    }

    /// Returns whether the buffer cannot accept `payload_bytes` more bytes.
    pub(crate) fn would_overflow(&self, payload_bytes: usize) -> bool {
        self.buffered_len() >= PUBLISH_BUFFER_MAX_MESSAGES
            || self.buffered_bytes() + payload_bytes > PUBLISH_BUFFER_MAX_BYTES
    }

    /// Buffers one accepted publication.
    ///
    /// The first publication of a batch arms the interval deadline so a
    /// batch is time-flushed even when it never reaches the size threshold
    /// (issue #96): a fresh pool's first publish would otherwise sit in the
    /// buffer until the threshold, a drain, or an explicit flush.
    pub(crate) fn enqueue(&self, publish: NativePublish) {
        let payload_bytes = publish.request.payload.len();
        if self.buffered_len() == 0 {
            *self.last_flush.lock().expect("last_flush mutex poisoned") = Some(Instant::now());
        }
        let mut buffer = self.buffer.lock().expect("publish buffer mutex poisoned");
        buffer.push(publish);
        drop(buffer);
        *self
            .buffered_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned") += payload_bytes;
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
            .len()
    }

    /// Returns the cumulative buffered payload bytes.
    pub(crate) fn buffered_bytes(&self) -> usize {
        *self
            .buffered_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned")
    }

    /// Returns whether the buffer holds at least one publication.
    fn is_empty(&self) -> bool {
        self.buffered_len() == 0
    }

    /// Drains the buffer, keeping the byte counter in sync.
    fn take(&self) -> Vec<NativePublish> {
        let mut buffer = self.buffer.lock().expect("publish buffer mutex poisoned");
        let publishes = std::mem::take(&mut *buffer);
        drop(buffer);
        *self
            .buffered_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned") -= Self::payload_bytes(&publishes);
        publishes
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
        let mut buffer = self.buffer.lock().expect("publish buffer mutex poisoned");
        buffer.extend(retriable);
        drop(buffer);
        *self
            .buffered_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned") += bytes;
    }

    /// Sends one batch through the client, re-buffering on failure.
    fn flush_batch(&self, publishes: Vec<NativePublish>) -> PhpResult<()> {
        if publishes.is_empty() {
            return Ok(());
        }

        // Keep the original requests so a failed flush can re-buffer them.
        let requests: Vec<(String, PublishRequest)> = publishes
            .iter()
            .map(|publish| (publish.broker.clone(), publish.request.clone()))
            .collect();

        let batch = self
            .handle
            .runtime()
            .block_on(self.client.publish_batch(requests));

        match batch {
            Ok(outcomes) => {
                // Every outcome is inspected before anything is raised so a
                // failure never short-circuits the buffer decisions. With the
                // current `publish_batch` contract this arm only ever sees
                // `Confirmed`, `Ambiguous`, and `Returned` outcomes: per-message
                // failures such as backpressure or timeout are folded into the
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
                    self.rebuffer(publishes);
                }
                client_exception(&error)
            }
        }
    }

    /// Flushes buffered publications under the fixed teardown budget.
    ///
    /// Used by the destructor path only: unlike `flush_all`, failures are
    /// never re-buffered — the process is going away, so anything the budget
    /// could not confirm is counted in `dropped_publications` and released.
    pub(crate) fn flush_teardown(&self) {
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

    /// Flushes every buffered publication.
    pub(crate) fn flush_all(&self) -> PhpResult<()> {
        *self.last_flush.lock().expect("last_flush mutex poisoned") = Some(Instant::now());
        let publishes = self.take();
        self.flush_batch(publishes)
    }

    /// Flushes the buffer when it holds publications; a no-op otherwise.
    ///
    /// Consumers call this before waiting for deliveries so publications
    /// accepted earlier are visible to the broker before the consumer blocks.
    pub(crate) fn flush_nonempty(&self) -> PhpResult<()> {
        if self.is_empty() {
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

#[allow(
    clippy::match_same_arms,
    reason = "Confirmed and Ambiguous are semantically distinct outcomes that both return the message_id"
)]
pub(crate) fn publish_message_id(outcome: PublishOutcome) -> PhpResult<String> {
    match outcome {
        PublishOutcome::Confirmed { message_id } => Ok(message_id.as_ref().to_owned()),
        PublishOutcome::Returned { message_id, reply } => rabbit_exception(format!(
            "message {message_id} was returned as unroutable (AMQP {})",
            reply.code
        )),
        PublishOutcome::Ambiguous { message_id } => Ok(message_id.as_ref().to_owned()),
    }
}
