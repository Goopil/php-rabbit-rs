//! Property-based state machine for the publish buffer (reliability
//! hardening, `docs/plans/2026-09-11-reliability-hardening.md`).
//!
//! Drives [`PublishBuffer`]'s public operations through the real client pool
//! on the scriptable mock transport, and checks after every generated
//! transition that the at-least-once accounting holds against a reference
//! model:
//!
//! - every accepted publication ends up in exactly one terminal bucket
//!   (confirmed, returned, or counted once in `dropped_publications`) or is
//!   still buffered — never lost silently;
//! - buffered counts and bytes track the model exactly, oldest-first;
//! - the pending-error queue surfaces exactly the records the model expects
//!   (pipelined drains record, synchronous paths raise instead);
//! - the actor-level metrics (confirmations, returns) match the scripted
//!   broker responses exactly, so re-buffers and drops stay distinguishable
//!   from confirmed deliveries;
//! - teardown and a closed pool convert every unconfirmed publication into
//!   an exactly-once drop instead of vanishing.
//!
//! Batch failures are scripted through non-recoverable confirmation errors
//! (`TransportError::protocol`), which resolve the waiter as a per-message
//! failure and fold into a batch-level error whose results are discarded and
//! re-buffered (or dropped) conservatively. Time runs real — the buffer's
//! `Handle::block_on` facade requires a multi-thread runtime, where paused
//! time is unsupported — so healthy deadlines never expire inside a case and
//! the deadline dimension is exercised through a pre-expired enqueue variant
//! instead: its waiter deadline is already past when the actor polls it, so
//! it deterministically resolves as a per-message timeout (the folded error
//! kind of the earliest failing waiter decides the surfaced record), while
//! its confirmation is still consumed by the mock on send. The confirmation
//! budget itself (`now + confirm_timeout`) keeps its dedicated deterministic
//! coverage elsewhere.
//!
//! The buffer ceiling (`PUBLISH_BUFFER_MAX_MESSAGES`) is intentionally out of
//! reach for generated sequences (tens of enqueues vs a 4096 ceiling); the
//! synchronous overflow path has dedicated deterministic coverage elsewhere.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use proptest::prelude::*;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};
// Forces the stub crate into the harness link so the dynamic loader finds the
// Zend symbols the ext-php-rs machinery references (see zend-link-stubs).
use zend_link_stubs as _;

use rabbit_rs_core::client::ClientPool;
use rabbit_rs_core::config::{
    BrokerConfig, Config, Credentials, DelayConfig, Endpoint, PublisherConfigSection, TlsConfig,
    TopologyMode,
};
use rabbit_rs_core::metrics::MetricsSnapshot;
use rabbit_rs_core::pool::ConnectionHandle;
use rabbit_rs_core::publisher::{Destination, MessageProperties, PublishRequest};
use rabbit_rs_core::runtime::{PidProvider, RuntimeRegistry};
use rabbit_rs_core::transport::mock::{MockTransport, TransportOperation};
use rabbit_rs_core::transport::{PublishConfirmation, ReturnedMessage, TransportError};

use crate::classes::publish_buffer::PublishBuffer;
use crate::conversion::NativePublish;
use ext_php_rs::prelude::PhpResult;

/// Runs one synchronous flush, swallowing its panic. Synchronous flush
/// failures raise PHP exceptions, and constructing an exception needs the
/// Zend engine (`PhpException::from_class` retrieves the registered class
/// entry), which the Rust test harness lacks. The buffer already re-buffered
/// or dropped everything before raising, so the panic carries no state loss;
/// the raised exception's content is model-verified instead of asserted.
fn swallow_raise(flush: impl FnOnce() -> PhpResult<()>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(flush));
}

/// Flush interval driving the background timer. Oversized so the timer can
/// never fire during a test case (its `should_flush` guard compares the real
/// wall clock against the interval): timer flushing is deterministic
/// coverage elsewhere, and the state machine stays free of clock races.
const FLUSH_INTERVAL: Duration = Duration::from_hours(1);
/// Validity window of a healthy publication. Generous against CI stalls: a
/// case finishes in milliseconds, so healthy deadlines never expire and the
/// model's expired/healthy classification stays stable.
const HEALTHY_DEADLINE: Duration = Duration::from_secs(10);
/// How far in the past a pre-expired publication's deadline sits. Only the
/// failed-batch re-buffer deadline filter reads it.
const EXPIRED_MARGIN: Duration = Duration::from_millis(100);
/// Fixed payload size: byte ceilings never bind and buffered bytes stay a
/// pure multiple of the count.
const PAYLOAD: &[u8] = b"job";
const BROKER: &str = "main";

/// One buffered publication as the model tracks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BufferedMsg {
    id: u32,
    /// Whether the publication's deadline already passed at enqueue time.
    /// Only failed-batch re-buffering consults it: expired publications are
    /// dropped exactly once instead of being re-buffered.
    expired: bool,
}

/// How a scripted batch resolves at the broker; only the first send differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchScript {
    /// Every send is acknowledged.
    AllAck,
    /// The first send is returned as unroutable, the rest acknowledged.
    FirstReturn,
    /// The first send fails with a non-recoverable error: the actor resolves
    /// it as a per-message failure and `publish_batch` folds the batch into
    /// a batch-level error whose results are discarded and re-buffered (or
    /// dropped) conservatively.
    FirstProtocolErr,
}

#[derive(Clone, Debug)]
enum Transition {
    /// Buffers one publication; `expired` pre-dates its deadline.
    Enqueue { expired: bool },
    /// Pipelined auto-flush (`flush_triggered`): the batch drains on a
    /// spawned task that records outcomes in the pending-error queue.
    PipelinedFlush(BatchScript),
    /// Explicit synchronous flush (`flush_all`): raises instead of recording.
    ExplicitFlush(BatchScript),
    /// Pop-path synchronous flush (`flush_nonempty`): raises like
    /// [`Transition::ExplicitFlush`], no-op when the buffer is empty.
    PopFlush(BatchScript),
    /// Destructor flush: quiesces drains, never records, never re-buffers —
    /// anything the batch did not confirm is counted as dropped.
    Teardown(BatchScript),
    /// Closes the pool: later flushes fail with `Closed` and drop.
    CloseClient,
}

/// Which flush path ran: only the pipelined drain records pending errors —
/// synchronous paths raise to the caller and the destructor stays silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlushSurface {
    Pipelined,
    Sync,
    Teardown,
}

/// Per-send actor-level outcome (the batch result is folded separately).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Confirmed,
    Returned,
    /// A per-message failure folded into the batch-level error. `Timeout`:
    /// the publication's deadline passed before its confirmation resolved
    /// (the actor's `timeout_at` deadline check wins over the instant mock
    /// receipt). `Protocol`: the scripted non-recoverable confirmation
    /// error.
    Failed(FailedCause),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailedCause {
    Timeout,
    Protocol,
}

/// The reference model. Cumulative observables mirror the SUT's counters;
/// `errors` is a per-transition delta because the SUT's pending-error queue
/// is drained by every invariant check.
#[derive(Clone, Debug, Default)]
struct Model {
    next_id: u32,
    /// Live buffer contents, oldest first.
    buffered: VecDeque<BufferedMsg>,
    /// Publications observed on the wire, in send order (replays included).
    sent_ids: Vec<u32>,
    /// Ack/Nack confirmations resolved by the actor (the pool's
    /// `confirmations_total` metric): scripted acknowledgements count even
    /// when a batch-level failure discards their outcomes.
    ack_resolutions: u64,
    /// Broker returns (`returns_total`), counted at resolution regardless of
    /// the batch outcome.
    returns_count: u64,
    /// Publications counted once in `dropped_publications`.
    dropped_ids: Vec<u32>,
    /// Terminal resolutions, for the model's own no-silent-loss partition.
    terminal_acked: Vec<u32>,
    terminal_returned: Vec<u32>,
    /// Every accepted publication id, for the partition check.
    enqueued_ids: Vec<u32>,
    torn_down: bool,
    client_closed: bool,
    /// Pending-error records surfaced by this transition only (the SUT's
    /// queue is drained after every transition).
    errors: Vec<(u32, &'static str)>,
    /// The batch this transition's flush sent (empty for non-flush
    /// transitions). The framework hands the post-transition model state to
    /// the SUT, so this is how the harness learns which publications the
    /// flush attempts and which of them are healthy sends.
    last_batch: Vec<BufferedMsg>,
}

struct PublishBufferMachine;

impl ReferenceStateMachine for PublishBufferMachine {
    type State = Model;
    type Transition = Transition;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(Model::default()).boxed()
    }

    fn transitions(_state: &Self::State) -> BoxedStrategy<Self::Transition> {
        prop_oneof![
            // Enqueue dominates: batches, lone publications, pre-expired ones.
            12 => prop::bool::ANY.prop_map(|expired| Transition::Enqueue { expired }),
            5 => script_strategy().prop_map(Transition::PipelinedFlush),
            4 => script_strategy().prop_map(Transition::ExplicitFlush),
            3 => script_strategy().prop_map(Transition::PopFlush),
            2 => script_strategy().prop_map(Transition::Teardown),
            1 => Just(Transition::CloseClient),
        ]
        .boxed()
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        // The SUT's pending-error queue is drained by every invariant check,
        // so the model tracks only the records this transition produces, and
        // only a flush carries a batch.
        state.errors.clear();
        state.last_batch.clear();
        match transition {
            Transition::Enqueue { expired } => {
                let msg = BufferedMsg {
                    id: state.next_id,
                    expired: *expired,
                };
                state.next_id += 1;
                state.buffered.push_back(msg);
                state.enqueued_ids.push(msg.id);
            }
            Transition::PipelinedFlush(script) => {
                Self::apply_flush(&mut state, *script, FlushSurface::Pipelined);
            }
            Transition::ExplicitFlush(script) | Transition::PopFlush(script) => {
                Self::apply_flush(&mut state, *script, FlushSurface::Sync);
            }
            Transition::Teardown(script) => Self::apply_teardown(&mut state, *script),
            Transition::CloseClient => state.client_closed = true,
        }
        state
    }
}

impl PublishBufferMachine {
    /// Applies one flush of the whole buffer. A closed pool sends nothing:
    /// every buffered publication is dropped exactly once, surfaced as a
    /// `Closed` record by the pipelined drain and raised (not recorded) by
    /// the synchronous paths.
    fn apply_flush(state: &mut Model, script: BatchScript, surface: FlushSurface) {
        if state.buffered.is_empty() {
            // No batch is taken and no confirmations are consumed.
            return;
        }
        let batch: Vec<BufferedMsg> = state.buffered.drain(..).collect();
        state.last_batch = batch.clone();
        if state.client_closed {
            state.dropped_ids.extend(batch.iter().map(|msg| msg.id));
            if surface == FlushSurface::Pipelined {
                state.errors.push((batch[0].id, "Closed"));
            }
            return;
        }
        Self::run_batch(state, &batch, script, surface);
    }

    /// Applies the destructor flush: sends happen, failures are never
    /// re-buffered, and outcomes are never surfaced.
    fn apply_teardown(state: &mut Model, script: BatchScript) {
        state.torn_down = true;
        if state.buffered.is_empty() {
            return;
        }
        let batch: Vec<BufferedMsg> = state.buffered.drain(..).collect();
        state.last_batch = batch.clone();
        if state.client_closed {
            state.dropped_ids.extend(batch.iter().map(|msg| msg.id));
            return;
        }
        Self::run_batch(state, &batch, script, FlushSurface::Teardown);
    }

    /// Sends one open-client batch against the model. Pre-expired
    /// publications are rejected by the actor at mailbox-processing time —
    /// before any wire write — so they are never sent, never confirmed, and
    /// consume no scripted confirmation; the scripted special outcome applies
    /// to the first healthy send.
    fn run_batch(
        state: &mut Model,
        batch: &[BufferedMsg],
        script: BatchScript,
        surface: FlushSurface,
    ) {
        let mut send_position = 0usize;
        let outcomes: Vec<Outcome> = batch
            .iter()
            .map(|msg| {
                if msg.expired {
                    return Outcome::Failed(FailedCause::Timeout);
                }
                let outcome = match (script, send_position) {
                    (BatchScript::FirstProtocolErr, 0) => Outcome::Failed(FailedCause::Protocol),
                    (BatchScript::FirstReturn, 0) => Outcome::Returned,
                    _ => Outcome::Confirmed,
                };
                send_position += 1;
                outcome
            })
            .collect();
        // Only healthy publications reach the wire, in batch order.
        state.sent_ids.extend(
            batch
                .iter()
                .zip(outcomes.iter())
                .filter(|(msg, _)| !msg.expired)
                .map(|(msg, _)| msg.id),
        );
        for outcome in &outcomes {
            match outcome {
                Outcome::Confirmed => state.ack_resolutions += 1,
                Outcome::Returned => {
                    state.ack_resolutions += 1;
                    state.returns_count += 1;
                }
                Outcome::Failed(_) => {}
            }
        }

        let first_failure = outcomes
            .iter()
            .position(|outcome| matches!(outcome, Outcome::Failed(_)));
        let Some(failed_at) = first_failure else {
            for (outcome, msg) in outcomes.iter().zip(batch) {
                match outcome {
                    Outcome::Confirmed => state.terminal_acked.push(msg.id),
                    Outcome::Returned => {
                        state.terminal_returned.push(msg.id);
                        if surface == FlushSurface::Pipelined {
                            state.errors.push((msg.id, "Returned"));
                        }
                    }
                    Outcome::Failed(_) => unreachable!("batch has no failure"),
                }
            }
            return;
        };

        // Batch-level failure: the folded error carries the first request's
        // message id and the kind of the earliest failing waiter, and the
        // whole batch is re-buffered or dropped. Only the pipelined drain
        // records the failure; synchronous paths raise and the destructor
        // stays silent.
        if surface == FlushSurface::Pipelined {
            let kind = match outcomes[failed_at] {
                Outcome::Failed(FailedCause::Timeout) => "Publish",
                Outcome::Failed(FailedCause::Protocol) => "Transport",
                Outcome::Confirmed | Outcome::Returned => unreachable!("failure position"),
            };
            state.errors.push((batch[0].id, kind));
        }
        Self::dispose_failed_batch(state, batch);
    }

    /// Batch-failure disposition: on a live buffer, expired publications are
    /// dropped exactly once and retriable ones are re-buffered oldest-first
    /// (a conservative superset — re-sent duplicates are permitted and
    /// identifiable via `message_id`); on a closing pool or during teardown
    /// everything is dropped.
    fn dispose_failed_batch(state: &mut Model, batch: &[BufferedMsg]) {
        if state.torn_down || state.client_closed {
            state.dropped_ids.extend(batch.iter().map(|msg| msg.id));
            return;
        }
        for msg in batch {
            if msg.expired {
                state.dropped_ids.push(msg.id);
            } else {
                state.buffered.push_back(*msg);
            }
        }
    }
}

fn script_strategy() -> impl Strategy<Value = BatchScript> {
    prop_oneof![
        6 => Just(BatchScript::AllAck),
        2 => Just(BatchScript::FirstReturn),
        3 => Just(BatchScript::FirstProtocolErr),
    ]
}

/// The concrete harness: a one-worker multi-thread runtime (spawned tasks —
/// coordinators, actors, drains — must be driven in the background for the
/// buffer's `Handle::block_on` facade to work) and a mock-transport pool.
struct Sut {
    transport: Arc<MockTransport>,
    client: Arc<ClientPool>,
    handle: Arc<ConnectionHandle>,
    buffer: Arc<PublishBuffer>,
    /// Owns the runtime the handle borrows; declared last so it drops after
    /// every other field.
    _registry: RuntimeRegistry,
}

struct FixedPid;

impl PidProvider for FixedPid {
    fn current_pid(&self) -> u32 {
        424_242
    }
}

/// The production runtime shape: a single-worker multi-thread runtime.
struct BackgroundRuntimeFactory;

impl rabbit_rs_core::runtime::RuntimeFactory for BackgroundRuntimeFactory {
    fn create(&self) -> std::io::Result<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
    }
}

fn validated_config() -> Arc<rabbit_rs_core::config::ValidatedConfig> {
    Config {
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
    .into()
}

/// Queues exactly the confirmations the batch's healthy sends will consume,
/// in send order, read from the model's post-transition state (the batch the
/// flush just took). Pre-expired publications are rejected by the actor
/// before any wire write and must not be scripted. A closed client fails
/// before sending and must not be scripted either (leftovers would poison
/// the next batch's FIFO alignment).
fn script_confirmations(sut: &Sut, model: &Model, script: BatchScript) {
    if model.client_closed {
        return;
    }
    let mut send_position = 0usize;
    for msg in &model.last_batch {
        if msg.expired {
            continue;
        }
        let confirmation = match (script, send_position) {
            (BatchScript::FirstProtocolErr, 0) => Err(TransportError::protocol(
                "simulated non-recoverable publish failure",
            )),
            (BatchScript::FirstReturn, 0) => Ok(PublishConfirmation::Ack(Some(ReturnedMessage {
                reply_code: 312,
                reply_text: "NO_ROUTE".to_owned(),
                exchange: "jobs".to_owned(),
                routing_key: "orders".to_owned(),
                payload: Bytes::from_static(PAYLOAD),
            }))),
            _ => Ok(PublishConfirmation::Ack(None)),
        };
        sut.transport.push_confirmation(confirmation);
        send_position += 1;
    }
}

fn wire_send_ids(sut: &Sut) -> Vec<u32> {
    sut.transport
        .operations()
        .into_iter()
        .filter_map(|operation| match operation {
            TransportOperation::Publish(request) => request
                .properties
                .message_id
                .as_deref()
                .and_then(|id| id.parse::<u32>().ok()),
            _ => None,
        })
        .collect()
}

impl StateMachineTest for PublishBufferMachine {
    type SystemUnderTest = Sut;
    type Reference = Self;

    fn init_test(_ref_state: &Model) -> Self::SystemUnderTest {
        let transport = Arc::new(MockTransport::default());
        // Connect, channel opens, and confirm-mode selection default to Ok on
        // the mock; only confirmations are scripted per batch.
        let registry = RuntimeRegistry::with_dependencies(
            Arc::new(FixedPid),
            Arc::new(BackgroundRuntimeFactory),
        );
        let config = validated_config();
        let handle = registry
            .acquire(rabbit_rs_core::pool::ConnectionKey::from_config(&config))
            .expect("connection handle");
        let client = Arc::new(ClientPool::new(
            Arc::clone(&config),
            Arc::clone(&transport) as _,
        ));
        let buffer = Arc::new(PublishBuffer::new(
            Arc::clone(&client),
            Arc::clone(&handle),
            FLUSH_INTERVAL,
        ));
        Sut {
            transport,
            client,
            handle,
            buffer,
            _registry: registry,
        }
    }

    fn apply(
        state: Self::SystemUnderTest,
        ref_state: &Model,
        transition: Transition,
    ) -> Self::SystemUnderTest {
        match transition {
            Transition::Enqueue { expired } => {
                let now = tokio::time::Instant::now();
                let deadline = if expired {
                    now - EXPIRED_MARGIN
                } else {
                    now + HEALTHY_DEADLINE
                };
                // The framework hands the post-transition model state here;
                // the id of this enqueue is its last accepted publication.
                let id = *ref_state
                    .enqueued_ids
                    .last()
                    .expect("enqueue recorded in the model");
                let publish = NativePublish {
                    broker: BROKER.to_owned(),
                    request: PublishRequest::new(
                        Destination::new("jobs", "orders"),
                        Bytes::from_static(PAYLOAD),
                        MessageProperties::new(format!("{id}")),
                        deadline,
                    ),
                };
                if state.buffer.enqueue(publish) {
                    // The pool arms the interval timer for a fresh batch.
                    state.buffer.ensure_flush_timer();
                }
            }
            Transition::PipelinedFlush(script) => {
                script_confirmations(&state, ref_state, script);
                state.buffer.flush_triggered().expect("pipelined flush");
                // Deterministic drain barrier: quiesce awaits every spawned
                // drain to completion (scripted confirmations resolve
                // immediately), so its outcome records are observable.
                state.buffer.quiesce();
            }
            Transition::ExplicitFlush(script) => {
                script_confirmations(&state, ref_state, script);
                swallow_raise(|| state.buffer.flush_all());
            }
            Transition::PopFlush(script) => {
                script_confirmations(&state, ref_state, script);
                swallow_raise(|| state.buffer.flush_nonempty());
            }
            Transition::Teardown(script) => {
                script_confirmations(&state, ref_state, script);
                state.buffer.flush_teardown();
            }
            Transition::CloseClient => {
                let _ = state.handle.runtime().block_on(state.client.close());
            }
        }
        state
    }

    fn check_invariants(state: &Self::SystemUnderTest, ref_state: &Model) {
        // Model self-check: every publication resolves into exactly one
        // terminal bucket or is still buffered, and drops happen at most once.
        let resolved = ref_state
            .buffered
            .iter()
            .map(|msg| msg.id)
            .chain(ref_state.terminal_acked.iter().copied())
            .chain(ref_state.terminal_returned.iter().copied())
            .chain(ref_state.dropped_ids.iter().copied())
            .collect::<Vec<_>>();
        let mut enqueued = ref_state.enqueued_ids.clone();
        enqueued.sort_unstable();
        let mut sorted_resolved = resolved.clone();
        sorted_resolved.sort_unstable();
        assert_eq!(
            sorted_resolved, enqueued,
            "model lost track of a publication"
        );
        let mut unique_drops = ref_state.dropped_ids.clone();
        unique_drops.sort_unstable();
        unique_drops.dedup();
        assert_eq!(
            unique_drops.len(),
            ref_state.dropped_ids.len(),
            "model dropped a publication twice"
        );

        // Buffered counts track the model exactly.
        assert_eq!(
            state.buffer.buffered_len(),
            ref_state.buffered.len(),
            "buffered count diverged from the model"
        );
        assert_eq!(
            state.buffer.buffered_bytes(),
            ref_state.buffered.len() * PAYLOAD.len(),
            "buffered bytes diverged from the model"
        );

        // Drops are counted exactly once per publication.
        assert_eq!(
            state.buffer.dropped_publications(),
            ref_state.dropped_ids.len() as u64,
            "drop accounting diverged"
        );

        // Surfaced error records match the model: order, ids, kinds.
        let errors = state
            .buffer
            .take_errors()
            .into_iter()
            .map(|error| {
                (
                    error.message_id.parse::<u32>().expect("numeric id"),
                    error.kind,
                )
            })
            .collect::<Vec<(u32, String)>>();
        let model_errors = ref_state
            .errors
            .iter()
            .map(|(id, kind)| (*id, (*kind).to_owned()))
            .collect::<Vec<(u32, String)>>();
        assert_eq!(errors, model_errors, "surfaced records diverged");

        // Actor-level metrics match the scripted broker responses exactly:
        // every acknowledgement was consumed by a send, every return was
        // resolved as unroutable, and neither turned into a silent drop.
        let metrics: MetricsSnapshot = state.client.metrics_snapshot();
        assert_eq!(
            metrics.confirmations_total, ref_state.ack_resolutions,
            "confirmation metric diverged from the scripted broker responses"
        );
        assert_eq!(
            metrics.returns_total, ref_state.returns_count,
            "return metric diverged from the scripted broker responses"
        );

        // The wire carries exactly the sends the model made, in order —
        // re-batched duplicates included, no phantom sends.
        assert_eq!(
            wire_send_ids(state),
            ref_state.sent_ids,
            "wire send log diverged"
        );
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn publish_buffer_state_machine(sequential 1..40 => PublishBufferMachine);
}
