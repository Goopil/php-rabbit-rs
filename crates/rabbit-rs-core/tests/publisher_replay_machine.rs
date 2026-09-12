//! Property-based state machine for publisher replay across recovery
//! (reliability hardening, `docs/plans/2026-09-11-reliability-hardening.md`).
//!
//! Drives the `RecoveryCoordinator`-managed publisher through generated
//! sequences of publications, connection losses, recoveries, controlled time
//! advances, and a terminal close on the scriptable mock transport. After
//! every transition the at-least-once replay invariants are checked against
//! a reference model:
//!
//! - every accepted publication resolves exactly once with the outcome the
//!   model predicts (confirmed, returned, or a typed failure) — never
//!   silently, never twice;
//! - the wire carries exactly the model's sends in order, replays included,
//!   and every send of one publication (first send and replays alike) carries
//!   the same message id and the same original-deadline probe header — a
//!   replay is a duplicate of the original request, not a rebuild;
//! - confirm-mode selection happens exactly once per connection generation;
//! - the actor metrics mirror the scripted broker responses:
//!   `publishes_total`, `confirmations_total`, `returns_total`,
//!   `reconnects_total` (one per completed recovery), and
//!   `publication_retries_total` (one per deadline re-arm), while
//!   `backpressure_total`, `recovery_failures_total`, and the consumer
//!   counters stay at zero.
//!
//! Every transition stays valid under proptest's value shrinking: a send
//! with no scripted confirmation consumes the mock's default
//! (`NotRequested` → terminal `Unconfirmed`), the model mirrors the mock's
//! script FIFO (leftovers persist across transitions), and transitions that
//! make no sense in the shrunk-landed phase are no-ops on both sides.
//!
//! Time runs paused on a per-case current-thread runtime: confirmation
//! timeouts fire only across explicit `AdvanceTime` transitions, and the
//! recovery backoff is absorbed by a poll-advance loop bounded far past the
//! policy's delay for two scripted connect failures. The model never tracks
//! the backoff, because it cannot matter: parks inherited from the wire keep
//! more than 25 s of deadline margin (a confirmation is dropped by the 5 s
//! confirm timeout long before the 30 s publication budget runs out), and
//! zero-budget parks (accepted pre-expired during a suspension) fail
//! deterministically at the first flush without a send — the jittered
//! backoff (≤ ~300 ms here) can never cross those margins, so replay
//! outcomes stay exact.
//!
//! Two paths are deliberately out of reach for generated sequences, each
//! with dedicated deterministic coverage elsewhere:
//!
//! - closing while a confirmation cannot resolve: quiesce would park on
//!   paused time forever, so `Close` is only generated (and only acts) when
//!   every publication is already terminal or nothing can be attempted;
//! - a park whose *positive* budget expires during a suspension (the
//!   re-arm-then-fail path): unreachable for the same margin reason, covered
//!   by the recovery tests' re-arm assertions.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use proptest::prelude::*;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

mod common;

use common::broker;
use rabbit_rs_core::config::{
    Config, ConsumerConfigSection, DelayConfig, PublisherConfigSection, SafetyMode, TopologyMode,
};
use rabbit_rs_core::metrics::Metrics;
use rabbit_rs_core::pool::recovery_coordinator::{
    RecoveryCoordinator, RecoveryCoordinatorConfig, RecoveryCoordinatorHandle,
};
use rabbit_rs_core::publisher::{
    Destination, MessageProperties, PublishError, PublishErrorKind, PublishOutcome, PublishRequest,
    PublisherConfig, PublisherHandle,
};
use rabbit_rs_core::recovery::{ConnectionState, RecoveryPolicy};
use rabbit_rs_core::transport::mock::{
    MockConfirmationController, MockTransport, TransportOperation,
};
use rabbit_rs_core::transport::{
    HeaderValue, PublishConfirmation, QueueKind, ReturnedMessage, Transport, TransportError,
};

/// Publication budget assigned at acceptance, in model milliseconds. Far
/// beyond anything a case advances: a confirmation is dropped by the 5 s
/// confirm timeout first, so parks inherited from the wire keep more than
/// 25 s of margin over the recovery backoff.
const HEALTHY_BUDGET_MS: u64 = 30_000;
/// Publisher confirm timeout, mirrored by the model's confirmation deadline.
const CONFIRM_TIMEOUT_MS: u64 = 5_000;
/// Publisher buffer capacity. Generated sequences (≤ 40 transitions) never
/// approach it, so backpressure stays structurally impossible.
const BUFFER_CAPACITY: usize = 64;
const PAYLOAD: &[u8] = b"payload";
/// Probe header stamped at acceptance. The transport-level publish record
/// carries no deadline field, so request identity across replays (same id
/// AND same original deadline) is verified through this passthrough marker.
const DEADLINE_PROBE_HEADER: &str = "x-probe-deadline";
/// Paused-time slice and window of the recovery poll-advance loop. The
/// policy's delay for two scripted connect failures (100 + 200 ms, jittered
/// to at most their full value) fits in a fraction of the window.
const ADVANCE_SLICE_MS: u64 = 25;
const RECOVERY_WINDOW_POLLS: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ready,
    Suspended,
    FailedPermanent,
    Closed,
}

/// A scripted confirmation response; the mock consumes one per wire send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    AckNone,
    AckReturned,
    NackNone,
    ErrProtocol,
    ErrConnection,
    Pending,
    Controlled,
}

/// The model's terminal prediction for one publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Predicted {
    Confirmed,
    Returned,
    Err(PublishErrorKind),
}

#[derive(Clone, Copy, Debug)]
struct Pub {
    id: u32,
    /// Budget assigned at acceptance: the re-arm amount if the deadline
    /// expires while parked.
    timeout_ms: u64,
    /// Current deadline in model milliseconds (re-arming mutates it).
    deadline_ms: u64,
    /// Send sequence; 0 marks a publication never sent.
    seq: u64,
    state: PubState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PubState {
    /// Sent, confirmation outstanding (`Pending`/`Controlled` only — every
    /// other script resolves at send time in the model).
    Awaiting {
        script: Script,
        conf_deadline_ms: u64,
    },
    /// Parked in the replay queue across a suspension.
    Parked { retried: bool },
}

/// The reference model. Total over every shrunk transition value: transitions
/// that make no sense in the current phase are no-ops, and a send with no
/// scripted confirmation resolves as the mock's default (`Unconfirmed`).
#[derive(Clone, Debug)]
struct Model {
    phase: Phase,
    /// Connection generation: 1 after the initial connection, +1 per
    /// completed recovery.
    generation: u64,
    /// Whether a recoverable connection loss has been reported to the
    /// connection actor without a completed recovery since — the condition
    /// under which an `AdvanceTime` fires the armed backoff and reconnects.
    connection_down: bool,
    /// Model milliseconds since the case started. Only `AdvanceTime` moves
    /// it — the recovery backoff is absorbed by the poll-advance loop and
    /// never crosses a park's margin (see the module docs).
    now_ms: u64,
    next_id: u32,
    next_seq: u64,
    /// Live (non-terminal) publications. In `Suspended` this is the replay
    /// queue, ordered by send sequence with never-sent publications first —
    /// exactly the actor's stable sort by sequence.
    active: Vec<Pub>,
    /// Mirror of the mock transport's confirmation FIFO: pushed per scripted
    /// send, popped per wire send, leftovers persist across transitions.
    scripted_fifo: VecDeque<Script>,
    /// Wire sends in order: (message id, original acceptance deadline).
    sends: Vec<(u32, u64)>,
    /// Terminal resolutions in resolution order.
    resolved: Vec<(u32, Predicted)>,
    accepts: u64,
    ack_resolutions: u64,
    returns_count: u64,
    retries: u64,
    controlled_outstanding: bool,
}

impl Model {
    fn initial() -> Self {
        Self {
            phase: Phase::Ready,
            generation: 1,
            connection_down: false,
            now_ms: 0,
            next_id: 0,
            next_seq: 0,
            active: Vec::new(),
            scripted_fifo: VecDeque::new(),
            sends: Vec::new(),
            resolved: Vec::new(),
            accepts: 0,
            ack_resolutions: 0,
            returns_count: 0,
            retries: 0,
            controlled_outstanding: false,
        }
    }

    /// Publications that reach the wire on the next replay flush: parks that
    /// survive the expire pass without being terminally failed pre-wire.
    fn flush_send_count(&self) -> usize {
        self.active
            .iter()
            .filter(|publication| match publication.state {
                PubState::Parked { retried } => {
                    if publication.deadline_ms > self.now_ms {
                        true
                    } else {
                        // Expired while parked: re-armed at most once with
                        // the remaining budget; a zero budget expires again
                        // at the pre-wire guard and sends nothing.
                        !retried && publication.timeout_ms > 0
                    }
                }
                PubState::Awaiting { .. } => false,
            })
            .count()
    }

    /// Suspends the generation: confirmations in flight are dropped, every
    /// live publication parks, and the merged replay queue is stable-sorted
    /// by send sequence (never-sent publications first). Idempotent.
    fn suspend(&mut self) {
        for publication in &mut self.active {
            if matches!(publication.state, PubState::Awaiting { .. }) {
                publication.state = PubState::Parked { retried: false };
            }
        }
        self.active.sort_by_key(|publication| publication.seq);
        self.phase = Phase::Suspended;
        self.controlled_outstanding = false;
    }

    fn accept(&mut self, expired: bool, script: Option<Script>) {
        if self.phase == Phase::Closed {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.accepts += 1;
        match self.phase {
            Phase::Ready => {
                if expired {
                    // Pre-wire guard: rejected before any send, without a
                    // retry count and without consuming a confirmation.
                    self.resolved
                        .push((id, Predicted::Err(PublishErrorKind::Timeout)));
                    return;
                }
                if let Some(script) = script {
                    self.scripted_fifo.push_back(script);
                }
                let publication = Pub {
                    id,
                    timeout_ms: HEALTHY_BUDGET_MS,
                    deadline_ms: self.now_ms + HEALTHY_BUDGET_MS,
                    seq: 0,
                    state: PubState::Parked { retried: false },
                };
                self.wire_send(publication);
            }
            Phase::Suspended => {
                if expired {
                    // Zero remaining budget: the suspension deadline watcher
                    // fires immediately for the already-past deadline, re-arms
                    // once (counted), and the immediate second tick terminates
                    // the publication — no flush and no time advance needed.
                    self.retries += 1;
                    self.resolved
                        .push((id, Predicted::Err(PublishErrorKind::Timeout)));
                    return;
                }
                self.active.push(Pub {
                    id,
                    timeout_ms: HEALTHY_BUDGET_MS,
                    deadline_ms: self.now_ms + HEALTHY_BUDGET_MS,
                    seq: 0,
                    state: PubState::Parked { retried: false },
                });
            }
            Phase::FailedPermanent => {
                self.resolved
                    .push((id, Predicted::Err(PublishErrorKind::Transport)));
            }
            Phase::Closed => unreachable!("handled above"),
        }
    }

    /// One wire send of `publication`, consuming the script FIFO head (the
    /// mock's default `NotRequested` when the FIFO runs dry).
    fn wire_send(&mut self, mut publication: Pub) {
        self.next_seq += 1;
        publication.seq = self.next_seq;
        self.sends.push((publication.id, publication.deadline_ms));
        match self.scripted_fifo.pop_front() {
            Some(script @ (Script::Pending | Script::Controlled)) => {
                let conf_deadline_ms =
                    (self.now_ms + CONFIRM_TIMEOUT_MS).min(publication.deadline_ms);
                publication.state = PubState::Awaiting {
                    script,
                    conf_deadline_ms,
                };
                self.controlled_outstanding |= script == Script::Controlled;
                self.active.push(publication);
            }
            Some(Script::ErrConnection) => {
                // A recoverable failure on the wire re-queues the publication
                // and suspends the generation.
                publication.state = PubState::Parked { retried: false };
                self.active.push(publication);
                self.suspend();
            }
            Some(script) => self.resolve_immediately(publication.id, script),
            None => {
                self.resolved.push((
                    publication.id,
                    Predicted::Err(PublishErrorKind::Unconfirmed),
                ));
            }
        }
    }

    fn resolve_immediately(&mut self, id: u32, script: Script) {
        let predicted = match script {
            Script::AckNone => {
                self.ack_resolutions += 1;
                Predicted::Confirmed
            }
            Script::AckReturned => {
                self.ack_resolutions += 1;
                self.returns_count += 1;
                Predicted::Returned
            }
            Script::NackNone => {
                self.ack_resolutions += 1;
                Predicted::Err(PublishErrorKind::Nack)
            }
            Script::ErrProtocol => Predicted::Err(PublishErrorKind::Transport),
            Script::ErrConnection | Script::Pending | Script::Controlled => {
                unreachable!("non-terminal scripts are handled by wire_send")
            }
        };
        self.resolved.push((id, predicted));
    }
}

/// Applies one transition to the model. Shared by the framework's reference
/// state machine and the harness's pre-state mirror, so both always agree.
fn apply_transition(mut state: Model, transition: &Transition) -> Model {
    match transition {
        Transition::Publish { expired, script } => state.accept(*expired, *script),
        Transition::LoseConnection { permanent } => match state.phase {
            Phase::Ready | Phase::Suspended => {
                if *permanent {
                    let drained = std::mem::take(&mut state.active);
                    for publication in drained {
                        state
                            .resolved
                            .push((publication.id, Predicted::Err(PublishErrorKind::Transport)));
                    }
                    state.phase = Phase::FailedPermanent;
                    state.connection_down = false;
                    state.controlled_outstanding = false;
                } else {
                    state.suspend();
                    state.connection_down = true;
                }
            }
            Phase::FailedPermanent | Phase::Closed => {}
        },
        Transition::Recover { scripts, .. } => match state.phase {
            Phase::Ready | Phase::Suspended => {
                state.scripted_fifo.extend(scripts.iter().copied());
                state.connection_down = true;
                // The reported loss reaches the publisher actor (idempotent
                // when already suspended); the recovery completes within the
                // poll-advance window and flushes the replay.
                state.suspend();
                complete_recovery(&mut state);
            }
            Phase::FailedPermanent | Phase::Closed => {}
        },
        Transition::AdvanceTime { millis } => {
            state.now_ms += *millis;
            if state.phase == Phase::Suspended && state.connection_down {
                // The armed backoff fires, the mock's default Ok reconnects,
                // and the replay flushes with whatever the FIFO holds.
                complete_recovery(&mut state);
            }
            let now = state.now_ms;
            let mut timed_out = Vec::new();
            state.active.retain(|publication| match publication.state {
                PubState::Awaiting {
                    conf_deadline_ms, ..
                } if conf_deadline_ms <= now => {
                    timed_out.push((publication.id, Predicted::Err(PublishErrorKind::Timeout)));
                    false
                }
                _ => true,
            });
            state.resolved.extend(timed_out);
        }
        Transition::ResolveControlled(script) => {
            if let Some(position) = state
                .active
                .iter()
                .position(|publication| {
                    matches!(
                        publication.state,
                        PubState::Awaiting {
                            script: Script::Controlled,
                            ..
                        }
                    )
                })
                .filter(|_| state.controlled_outstanding)
            {
                let publication = state.active.remove(position);
                state.resolve_immediately(publication.id, *script);
                state.controlled_outstanding = false;
            }
        }
        Transition::Close => match state.phase {
            Phase::Closed => {}
            Phase::Ready if !state.active.is_empty() => {
                // Closing with an unresolvable confirmation would park
                // quiesce on paused time; the strategy never generates it
                // and shrinking cannot make it act.
            }
            Phase::Suspended => {
                let drained = std::mem::take(&mut state.active);
                for publication in drained {
                    state
                        .resolved
                        .push((publication.id, Predicted::Err(PublishErrorKind::Closed)));
                }
                state.phase = Phase::Closed;
            }
            Phase::Ready | Phase::FailedPermanent => {
                state.active.clear();
                state.phase = Phase::Closed;
            }
        },
    }
    state
}

/// Completes an in-flight recovery: expire pass over the replay queue, then
/// the ordered flush. Used by `Recover` and by `AdvanceTime` while the
/// connection is down.
fn complete_recovery(state: &mut Model) {
    let now = state.now_ms;
    // Expire pass: a park expired while parked is re-armed at most once with
    // its remaining budget; a zero budget expires again at the pre-wire
    // guard and terminally fails without a send or a second retry count.
    for publication in &mut state.active {
        if let PubState::Parked { retried } = &mut publication.state
            && publication.deadline_ms <= now
            && !*retried
        {
            *retried = true;
            publication.deadline_ms = now + publication.timeout_ms;
            state.retries += 1;
        }
    }
    let drained = std::mem::take(&mut state.active);
    for publication in drained {
        if publication.deadline_ms <= now {
            state
                .resolved
                .push((publication.id, Predicted::Err(PublishErrorKind::Timeout)));
        } else {
            state.wire_send(publication);
        }
    }
    state.generation += 1;
    state.phase = Phase::Ready;
    state.connection_down = false;
}

#[derive(Clone, Debug)]
enum Transition {
    /// Publishes one message. `script` is present exactly when the phase is
    /// `Ready` (the send consumes the script FIFO); shrinking may strip it,
    /// which makes the send resolve as the mock's default `Unconfirmed`.
    Publish {
        expired: bool,
        script: Option<Script>,
    },
    /// Reports a connection loss: a recoverable one parks everything, a
    /// permanent one fails everything terminally.
    LoseConnection { permanent: bool },
    /// Reports a loss and reconnects after `connect_failures` refused
    /// attempts, then flushes the replay queue; `scripts` are pushed onto
    /// the confirmation FIFO for the flush sends.
    Recover {
        connect_failures: u32,
        scripts: Vec<Script>,
    },
    /// Advances paused time, firing confirmation timeouts — and, while the
    /// connection is down, the armed recovery backoff.
    AdvanceTime { millis: u64 },
    /// Resolves the at most one outstanding controlled confirmation.
    ResolveControlled(Script),
    /// Stops the coordinator; parked publications resolve `Closed`.
    Close,
}

struct PublisherReplayMachine;

impl ReferenceStateMachine for PublisherReplayMachine {
    type State = Model;
    type Transition = Transition;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(Model::initial()).boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        match state.phase {
            // A controlled confirmation is outstanding: closing would park
            // quiesce on paused time, so only resolving it retires it.
            Phase::Ready if state.controlled_outstanding => prop_oneof![
                12 => (any::<bool>(), direct_script(false)).prop_map(
                    |(expired, script)| Transition::Publish { expired, script: Some(script) },
                ),
                3 => any::<bool>().prop_map(|permanent| Transition::LoseConnection { permanent }),
                3 => (100u64..=10_000u64).prop_map(|millis| Transition::AdvanceTime { millis }),
                2 => controlled_script().prop_map(Transition::ResolveControlled),
            ]
            .boxed(),
            Phase::Ready if state.active.is_empty() => prop_oneof![
                12 => (any::<bool>(), direct_script(true)).prop_map(
                    |(expired, script)| Transition::Publish { expired, script: Some(script) },
                ),
                3 => any::<bool>().prop_map(|permanent| Transition::LoseConnection { permanent }),
                3 => (100u64..=10_000u64).prop_map(|millis| Transition::AdvanceTime { millis }),
                1 => Just(Transition::Close),
            ]
            .boxed(),
            Phase::Ready => prop_oneof![
                // A pending confirmation is outstanding: closing would park
                // quiesce on paused time, so no `Close` here.
                12 => (any::<bool>(), direct_script(true)).prop_map(
                    |(expired, script)| Transition::Publish { expired, script: Some(script) },
                ),
                3 => any::<bool>().prop_map(|permanent| Transition::LoseConnection { permanent }),
                3 => (100u64..=10_000u64).prop_map(|millis| Transition::AdvanceTime { millis }),
            ]
            .boxed(),
            Phase::Suspended => prop_oneof![
                5 => any::<bool>().prop_map(|expired| Transition::Publish { expired, script: None }),
                8 => (
                    0u32..=2u32,
                    proptest::collection::vec(
                        flush_script(),
                        state.flush_send_count()..=state.flush_send_count(),
                    ),
                )
                    .prop_map(|(connect_failures, scripts)| Transition::Recover {
                        connect_failures,
                        scripts,
                    }),
                1 => Just(Transition::Close),
            ]
            .boxed(),
            Phase::FailedPermanent => prop_oneof![
                2 => Just(Transition::Publish {
                    expired: false,
                    script: None,
                }),
                1 => Just(Transition::Close),
            ]
            .boxed(),
            Phase::Closed => Just(Transition::Close).boxed(),
        }
    }

    fn apply(state: Self::State, transition: &Self::Transition) -> Self::State {
        apply_transition(state, transition)
    }
}

fn direct_script(allow_controlled: bool) -> BoxedStrategy<Script> {
    let base = prop_oneof![
        6 => Just(Script::AckNone),
        2 => Just(Script::AckReturned),
        1 => Just(Script::NackNone),
        1 => Just(Script::ErrProtocol),
        2 => Just(Script::ErrConnection),
        1 => Just(Script::Pending),
    ];
    if allow_controlled {
        prop_oneof![6 => base, 1 => Just(Script::Controlled)].boxed()
    } else {
        base.boxed()
    }
}

fn flush_script() -> impl Strategy<Value = Script> {
    // A replay flush never scripts a recoverable error (the mid-flush
    // suspension it would trigger is exercised by direct accepts) nor a
    // controlled confirmation (direct accepts only, one at a time).
    prop_oneof![
        6 => Just(Script::AckNone),
        2 => Just(Script::AckReturned),
        1 => Just(Script::NackNone),
        1 => Just(Script::ErrProtocol),
        1 => Just(Script::Pending),
    ]
}

fn controlled_script() -> impl Strategy<Value = Script> {
    prop_oneof![
        6 => Just(Script::AckNone),
        2 => Just(Script::AckReturned),
        1 => Just(Script::NackNone),
        1 => Just(Script::ErrProtocol),
    ]
}

/// One observed waiter resolution.
#[derive(Debug)]
struct ObservedResolution {
    id: u32,
    outcome: Result<PublishOutcome, PublishError>,
}

/// The concrete harness: a paused current-thread runtime owning every task
/// (coordinator, connection actor, publisher actor, resolver tasks), a mock
/// transport, and the shared metrics registry.
struct Sut {
    transport: Arc<MockTransport>,
    coordinator: RecoveryCoordinatorHandle,
    publisher: PublisherHandle,
    metrics: Metrics,
    resolved: Arc<Mutex<Vec<ObservedResolution>>>,
    /// Controller for the at most one outstanding controlled confirmation.
    controller: Option<MockConfirmationController>,
    /// The model state before the transition being applied: scripting and
    /// no-op decisions read the phase the transition lands in, not the one
    /// it was generated for (value shrinking can change the latter).
    pre_state: Model,
    /// Owns every task; declared last so it drops after the handles.
    runtime: tokio::runtime::Runtime,
}

fn validated_config() -> Arc<rabbit_rs_core::config::ValidatedConfig> {
    Arc::new(
        Config {
            brokers: vec![broker("primary", "/", "guest")],
            workers: Vec::new(),
            topology_mode: TopologyMode::External,
            routes: BTreeMap::new(),
            delay: DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid config"),
    )
}

fn returned_message() -> ReturnedMessage {
    ReturnedMessage {
        reply_code: 312,
        reply_text: "NO_ROUTE".to_owned(),
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        payload: Bytes::from_static(PAYLOAD),
    }
}

/// Queues the mock response one wire send will consume.
fn push_script(transport: &MockTransport, script: Script) {
    match script {
        Script::AckNone => transport.push_confirmation(Ok(PublishConfirmation::Ack(None))),
        Script::AckReturned => {
            transport.push_confirmation(Ok(PublishConfirmation::Ack(Some(returned_message()))));
        }
        Script::NackNone => transport.push_confirmation(Ok(PublishConfirmation::Nack(None))),
        Script::ErrProtocol => transport.push_confirmation(Err(TransportError::protocol(
            "simulated non-recoverable publish failure",
        ))),
        Script::ErrConnection => transport.push_confirmation(Err(TransportError::connection(
            "simulated recoverable publish failure",
        ))),
        Script::Pending => transport.push_pending_confirmation(),
        Script::Controlled => {
            unreachable!("controlled confirmations use push_controlled_confirmation")
        }
    }
}

/// Wire publish records: (message id, probe-stamped original deadline).
fn wire_sends(transport: &MockTransport) -> Vec<(u32, i64)> {
    transport
        .operations()
        .into_iter()
        .filter_map(|operation| match operation {
            TransportOperation::Publish(request) => {
                let id: u32 = request
                    .properties
                    .message_id
                    .as_deref()
                    .expect("message id on the wire")
                    .parse()
                    .expect("numeric message id");
                let Some(HeaderValue::Integer(deadline)) =
                    request.properties.headers.get(DEADLINE_PROBE_HEADER)
                else {
                    panic!("deadline probe header missing on the wire");
                };
                Some((id, *deadline))
            }
            _ => None,
        })
        .collect()
}

fn wire_send_count(transport: &MockTransport) -> usize {
    transport
        .operations()
        .iter()
        .filter(|operation| matches!(operation, TransportOperation::Publish(_)))
        .count()
}

/// Drives the paused runtime until the SUT has caught up with the model's
/// post-transition expectations, then drains every queued command chain
/// (connection events, suspensions) so the next transition scripts against a
/// quiescent actor.
fn settle(state: &Sut, ref_state: &Model) {
    state.runtime.block_on(async {
        for _ in 0..2_000 {
            if wire_send_count(&state.transport) == ref_state.sends.len()
                && state
                    .resolved
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len()
                    == ref_state.resolved.len()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            wire_send_count(&state.transport),
            ref_state.sends.len(),
            "wire send count diverged from the model"
        );
        assert_eq!(
            state
                .resolved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            ref_state.resolved.len(),
            "resolution count diverged from the model"
        );
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
    });
}

async fn wait_initial_ready(coordinator: &RecoveryCoordinatorHandle) {
    for _ in 0..2_000 {
        if matches!(
            coordinator.state(),
            ConnectionState::Ready { generation: 1 }
        ) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "initial connection did not become Ready (state: {:?})",
        coordinator.state()
    );
}

/// Advances paused time until the coordinator reports the expected
/// generation, absorbing the recovery backoff (and any scripted connect
/// failures) along the way.
async fn wait_ready_generation(coordinator: &RecoveryCoordinatorHandle, generation: u64) {
    for _ in 0..RECOVERY_WINDOW_POLLS {
        if let ConnectionState::Ready { generation: ready } = coordinator.state()
            && ready == generation
        {
            return;
        }
        tokio::time::advance(Duration::from_millis(ADVANCE_SLICE_MS)).await;
        tokio::task::yield_now().await;
    }
    panic!("recovery did not reach Ready {{ {generation} }} within the advanced window");
}

fn predicted_of(outcome: &Result<PublishOutcome, PublishError>) -> Predicted {
    match outcome {
        Ok(PublishOutcome::Confirmed { .. }) => Predicted::Confirmed,
        Ok(PublishOutcome::Returned { .. }) => Predicted::Returned,
        Err(error) => Predicted::Err(error.kind()),
    }
}

/// A pre-expired Ready publish is rejected before any wire write and
/// consumes no confirmation: scripting it would poison the FIFO alignment
/// of the next send.
fn script_for_ready(state: &mut Sut, script: Option<Script>) {
    if let Some(script) = script {
        match script {
            Script::Controlled => {
                state.controller = Some(state.transport.push_controlled_confirmation());
            }
            script => push_script(&state.transport, script),
        }
    }
}

fn spawn_waiter_recorder(state: &mut Sut, id: u32, probe: i64, expired: bool) {
    let resolved = Arc::clone(&state.resolved);
    let publisher = state.publisher.clone();
    state.runtime.block_on(async move {
        let now = tokio::time::Instant::now();
        let deadline = if expired {
            now
        } else {
            now + Duration::from_millis(HEALTHY_BUDGET_MS)
        };
        let mut properties = MessageProperties::new(id.to_string());
        properties.headers.insert(
            DEADLINE_PROBE_HEADER.to_owned(),
            HeaderValue::Integer(probe),
        );
        let request = PublishRequest::new(
            Destination::new("jobs", "high"),
            Bytes::from_static(PAYLOAD),
            properties,
            deadline,
        );
        let waiter = publisher.try_publish(request).expect("publish accepted");
        tokio::spawn(async move {
            let outcome = waiter.wait().await;
            let mut recorded = resolved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            recorded.push(ObservedResolution { id, outcome });
        });
    });
}

fn apply_publish(state: &mut Sut, pre: &Model, expired: bool, script: Option<Script>) {
    // The probe carries the model's acceptance-time deadline; the mock's
    // publish record has no deadline field, so this marker is what makes
    // replay identity observable on the wire.
    let probe = i64::try_from(pre.now_ms + u64::from(!expired) * HEALTHY_BUDGET_MS)
        .expect("deadline fits i64");
    if pre.phase == Phase::Ready && !expired {
        script_for_ready(state, script);
    }
    if pre.phase != Phase::Closed {
        spawn_waiter_recorder(state, pre.next_id, probe, expired);
    }
}

async fn drain_yield_loops() {
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
}

fn apply_lose_connection(state: &mut Sut, pre: &Model, permanent: bool) {
    if matches!(pre.phase, Phase::Ready | Phase::Suspended)
        && state.coordinator.state() != ConnectionState::Closed
    {
        let error = if permanent {
            TransportError::config("simulated permanent failure")
        } else {
            TransportError::connection("simulated connection loss")
        };
        let coordinator = &state.coordinator;
        state.runtime.block_on(async move {
            coordinator
                .connection_lost(error)
                .await
                .expect("connection loss reported");
            drain_yield_loops().await;
        });
    }
}

fn apply_recover(
    state: &mut Sut,
    pre: &Model,
    generation: u64,
    connect_failures: u32,
    scripts: &[Script],
) {
    if matches!(pre.phase, Phase::Ready | Phase::Suspended)
        && state.coordinator.state() != ConnectionState::Closed
    {
        for _ in 0..connect_failures {
            state
                .transport
                .push_connect_result(Err(TransportError::connection("simulated connect refusal")));
        }
        state.transport.push_connect_result(Ok(()));
        for script in scripts {
            push_script(&state.transport, *script);
        }
        let coordinator = &state.coordinator;
        state.runtime.block_on(async move {
            coordinator
                .connection_lost(TransportError::connection("simulated connection loss"))
                .await
                .expect("connection loss reported");
            drain_yield_loops().await;
            wait_ready_generation(coordinator, generation).await;
        });
    }
}

fn apply_resolve_controlled(state: &mut Sut, script: Script) {
    if let Some(controller) = state.controller.take() {
        let result = match script {
            Script::AckNone => Ok(PublishConfirmation::Ack(None)),
            Script::AckReturned => Ok(PublishConfirmation::Ack(Some(returned_message()))),
            Script::NackNone => Ok(PublishConfirmation::Nack(None)),
            Script::ErrProtocol => Err(TransportError::protocol(
                "simulated non-recoverable publish failure",
            )),
            Script::ErrConnection | Script::Pending | Script::Controlled => {
                unreachable!("controlled resolutions are terminal confirmations")
            }
        };
        // A stale controller (its confirmation was parked by a suspension)
        // resolves nothing: its receiver is gone.
        let _ = controller.resolve(result);
    }
}

fn apply_close(state: &mut Sut, pre: &Model) {
    let can_close = match pre.phase {
        Phase::Closed => false,
        Phase::Ready => pre.active.is_empty(),
        Phase::Suspended | Phase::FailedPermanent => true,
    };
    if can_close && state.coordinator.state() != ConnectionState::Closed {
        let coordinator = &state.coordinator;
        state.runtime.block_on(async move {
            coordinator.close().await.expect("coordinator closed");
        });
    }
}

impl StateMachineTest for PublisherReplayMachine {
    type SystemUnderTest = Sut;
    type Reference = Self;

    fn init_test(ref_state: &Model) -> Self::SystemUnderTest {
        let transport = Arc::new(MockTransport::default());
        let metrics = Metrics::default();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .expect("paused current-thread runtime");
        let coordinator = runtime.block_on(async {
            let coordinator = RecoveryCoordinator::spawn(
                &(Arc::clone(&transport) as Arc<dyn Transport>),
                RecoveryCoordinatorConfig {
                    broker: broker("primary", "/", "guest"),
                    policy: RecoveryPolicy::default(),
                    publisher_config: PublisherConfig::with_safety(
                        BUFFER_CAPACITY,
                        Duration::from_millis(CONFIRM_TIMEOUT_MS),
                        SafetyMode::Safe,
                    ),
                    config: validated_config(),
                    metrics: metrics.clone(),
                    requested_profiles: Arc::new(Mutex::new(BTreeMap::new())),
                },
            );
            wait_initial_ready(&coordinator).await;
            coordinator
        });
        let publisher = runtime.block_on(async {
            coordinator
                .wait_for_publisher()
                .await
                .expect("publisher installed after the initial connection")
        });
        Sut {
            transport,
            coordinator,
            publisher,
            metrics,
            resolved: Arc::new(Mutex::new(Vec::new())),
            controller: None,
            pre_state: ref_state.clone(),
            runtime,
        }
    }

    fn apply(
        mut state: Self::SystemUnderTest,
        ref_state: &Model,
        transition: Transition,
    ) -> Self::SystemUnderTest {
        let pre = state.pre_state.clone();
        state.pre_state = apply_transition(pre.clone(), &transition);
        match &transition {
            Transition::Publish { expired, script } => {
                apply_publish(&mut state, &pre, *expired, *script);
            }
            Transition::LoseConnection { permanent } => {
                apply_lose_connection(&mut state, &pre, *permanent);
            }
            Transition::Recover {
                connect_failures,
                scripts,
            } => apply_recover(
                &mut state,
                &pre,
                ref_state.generation,
                *connect_failures,
                scripts,
            ),
            Transition::AdvanceTime { millis } => {
                state.runtime.block_on(async {
                    tokio::time::advance(Duration::from_millis(*millis)).await;
                });
            }
            Transition::ResolveControlled(script) => apply_resolve_controlled(&mut state, *script),
            Transition::Close => apply_close(&mut state, &pre),
        }
        settle(&state, ref_state);
        state
    }

    fn check_invariants(state: &Self::SystemUnderTest, ref_state: &Model) {
        // Model partition: every accepted publication is either terminal
        // (once) or still live — never two buckets, never none.
        let mut buckets: BTreeMap<u32, usize> = BTreeMap::new();
        for (id, _) in &ref_state.resolved {
            *buckets.entry(*id).or_insert(0) += 1;
        }
        for publication in &ref_state.active {
            *buckets.entry(publication.id).or_insert(0) += 1;
        }
        assert_eq!(
            buckets.len(),
            ref_state.next_id as usize,
            "model lost track of an accepted publication"
        );
        assert!(
            buckets.values().all(|count| *count == 1),
            "model double-counted a publication: {buckets:?}"
        );

        // The wire carries exactly the model's sends, in order, and every
        // send of one publication carries the same original-deadline probe
        // marker — replays are duplicates of the original request.
        let wire = wire_sends(&state.transport);
        let model_sends: Vec<(u32, i64)> = ref_state
            .sends
            .iter()
            .map(|(id, deadline)| (*id, i64::try_from(*deadline).expect("deadline fits i64")))
            .collect();
        assert_eq!(wire, model_sends, "wire send log diverged from the model");

        // Resolutions match the model per id, exactly once each.
        let recorded = state
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let observed: BTreeMap<u32, Predicted> = recorded
            .iter()
            .map(|resolution| (resolution.id, predicted_of(&resolution.outcome)))
            .collect();
        let expected: BTreeMap<u32, Predicted> = ref_state.resolved.iter().copied().collect();
        assert_eq!(observed, expected, "resolution outcomes diverged");
        for resolution in recorded.iter() {
            if let Ok(
                PublishOutcome::Confirmed { message_id }
                | PublishOutcome::Returned { message_id, .. },
            ) = &resolution.outcome
            {
                assert_eq!(
                    &**message_id,
                    &resolution.id.to_string(),
                    "outcome echoed a different message id"
                );
            }
        }
        drop(recorded);

        // Confirm-mode selection: exactly once per connection generation.
        let enable_confirms = state
            .transport
            .operations()
            .into_iter()
            .filter(|operation| matches!(operation, TransportOperation::EnableConfirms))
            .count();
        assert_eq!(
            u64::try_from(enable_confirms).expect("small count"),
            ref_state.generation,
            "confirm-mode selection did not happen once per generation"
        );

        // Metrics mirror the scripted broker responses exactly.
        let snapshot = state.metrics.snapshot();
        assert_eq!(
            snapshot.publishes_total, ref_state.accepts,
            "publishes_total diverged"
        );
        assert_eq!(
            snapshot.confirmations_total, ref_state.ack_resolutions,
            "confirmations_total diverged"
        );
        assert_eq!(
            snapshot.returns_total, ref_state.returns_count,
            "returns_total diverged"
        );
        assert_eq!(
            snapshot.publication_retries_total, ref_state.retries,
            "publication_retries_total diverged"
        );
        assert_eq!(
            snapshot.reconnects_total,
            ref_state.generation - 1,
            "reconnects_total diverged"
        );
        assert_eq!(
            snapshot.recovery_failures_total, 0,
            "recovery generations must not fail on the mock"
        );
        assert_eq!(
            snapshot.backpressure_total, 0,
            "backpressure must stay structurally impossible"
        );
        assert_eq!(snapshot.deliveries_total, 0, "no consumers exist here");
        assert_eq!(snapshot.duplicates_total, 0, "no consumers exist here");
        assert_eq!(snapshot.acks_total, 0, "no consumers exist here");
        assert_eq!(snapshot.rejects_total, 0, "no consumers exist here");

        if ref_state.phase == Phase::Closed {
            assert_eq!(state.coordinator.state(), ConnectionState::Closed);
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn publisher_replay_state_machine(sequential 1..40 => PublisherReplayMachine);
}
