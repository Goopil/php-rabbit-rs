use std::{error::Error, fmt, future::Future, sync::Arc, time::Duration};

use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    config::BrokerConfig,
    metrics::{Metrics, MetricsSnapshot},
    recovery::{Clock, ConnectionState, JitterSource, RecoveryPolicy},
    transport::{
        ConsumerChannel, PublisherChannel, Transport, TransportConnection, TransportError,
        TransportEvent, TransportEventStream, TransportResult,
    },
};

const COMMAND_CAPACITY: usize = 32;

/// Upper bound for a single connect attempt and for each per-command
/// transport operation (channel opens, connection close) so a silent network
/// black hole (socket accepted by a proxy, no handshake data ever arriving)
/// cannot block the recovery lifecycle or park the command loop — every
/// queued command (`Close`, `ConnectionLost`, error events) would otherwise
/// stall until heartbeat detection, configurable up to 65535 s. A timed-out
/// operation resolves as a recoverable connection error and the backoff
/// policy schedules the next try.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawns and owns the serialized lifecycle of one broker connection.
pub struct ConnectionActor;

impl ConnectionActor {
    /// Spawns an actor with deterministic dependencies and shared metrics.
    #[must_use]
    pub fn spawn_with_dependencies_and_metrics(
        transport: Arc<dyn Transport>,
        config: BrokerConfig,
        policy: RecoveryPolicy,
        clock: Arc<dyn Clock>,
        jitter: Arc<dyn JitterSource>,
        metrics: Metrics,
    ) -> ConnectionActorHandle {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (states, state_receiver) = watch::channel(ConnectionState::Disconnected);

        tokio::spawn(run_actor(ActorContext {
            transport,
            config,
            policy,
            clock,
            jitter,
            commands: receiver,
            states,
            metrics: metrics.clone(),
        }));

        ConnectionActorHandle {
            commands,
            states: state_receiver,
            metrics,
        }
    }
}

/// Cloneable command handle for a connection actor.
#[derive(Clone)]
pub struct ConnectionActorHandle {
    commands: mpsc::Sender<Command>,
    states: watch::Receiver<ConnectionState>,
    metrics: Metrics,
}

impl ConnectionActorHandle {
    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<ConnectionState> {
        self.states.clone()
    }

    /// Starts the initial connection attempt.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionActorClosed`] if the actor already stopped.
    pub async fn start(&self) -> Result<(), ConnectionActorClosed> {
        self.send(Command::Start).await
    }

    /// Reports loss of the active connection through the actor's command queue.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionActorClosed`] if the actor already stopped.
    pub async fn connection_lost(
        &self,
        error: TransportError,
    ) -> Result<(), ConnectionActorClosed> {
        self.send(Command::ConnectionLost(error)).await
    }

    /// Opens a publisher channel on the active connection.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionActorClosed`] if the actor stopped or
    /// [`TransportError`] if the channel cannot be opened.
    pub async fn open_publisher(&self) -> Result<Box<dyn PublisherChannel>, ConnectionActorClosed> {
        let (completed, completion) = oneshot::channel();
        self.send(Command::OpenPublisher(completed)).await?;
        completion
            .await
            .map_err(|_| ConnectionActorClosed)?
            .map_err(|_| ConnectionActorClosed)
    }

    /// Opens a publisher channel on the active connection, preserving the
    /// typed transport failure.
    ///
    /// Serves the same serialized [`Command::OpenPublisher`] as
    /// [`Self::open_publisher`], but returns the transport error as-is so
    /// admin callers can classify and surface it.
    ///
    /// # Errors
    ///
    /// Returns a [`TransportError`] when the actor stopped or the channel
    /// cannot be opened on the active connection.
    pub async fn open_admin_channel(&self) -> Result<Box<dyn PublisherChannel>, TransportError> {
        let (completed, completion) = oneshot::channel();
        self.send(Command::OpenPublisher(completed))
            .await
            .map_err(|_| TransportError::closed("connection actor is closed"))?;
        completion
            .await
            .map_err(|_| TransportError::closed("connection actor is closed"))?
    }

    /// Opens a consumer channel on the active connection.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionActorClosed`] if the actor stopped or
    /// [`TransportError`] if the channel cannot be opened.
    pub async fn open_consumer(&self) -> Result<Box<dyn ConsumerChannel>, ConnectionActorClosed> {
        let (completed, completion) = oneshot::channel();
        self.send(Command::OpenConsumer(completed)).await?;
        completion
            .await
            .map_err(|_| ConnectionActorClosed)?
            .map_err(|_| ConnectionActorClosed)
    }

    /// Interrupts any active backoff and waits for graceful actor shutdown.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionActorClosed`] if shutdown cannot be delivered or observed.
    pub async fn close(&self) -> Result<(), ConnectionActorClosed> {
        let (completed, completion) = oneshot::channel();
        self.send(Command::Close(completed)).await?;
        completion.await.map_err(|_| ConnectionActorClosed)
    }

    async fn send(&self, command: Command) -> Result<(), ConnectionActorClosed> {
        self.commands
            .send(command)
            .await
            .map_err(|_| ConnectionActorClosed)
    }
}

/// Indicates that a command could not reach a live connection actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionActorClosed;

impl fmt::Display for ConnectionActorClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("connection actor is closed")
    }
}

impl Error for ConnectionActorClosed {}

enum Command {
    Start,
    ConnectionLost(TransportError),
    OpenPublisher(oneshot::Sender<Result<Box<dyn PublisherChannel>, TransportError>>),
    OpenConsumer(oneshot::Sender<Result<Box<dyn ConsumerChannel>, TransportError>>),
    Close(oneshot::Sender<()>),
}

enum Phase {
    Disconnected,
    Connecting {
        previous_failures: u32,
    },
    Ready,
    Recovering {
        failures: u32,
        error: TransportError,
    },
    FailedPermanent,
}

struct ActorContext {
    transport: Arc<dyn Transport>,
    config: BrokerConfig,
    policy: RecoveryPolicy,
    clock: Arc<dyn Clock>,
    jitter: Arc<dyn JitterSource>,
    commands: mpsc::Receiver<Command>,
    states: watch::Sender<ConnectionState>,
    metrics: Metrics,
}

async fn run_actor(mut context: ActorContext) {
    let mut phase = Phase::Disconnected;
    let mut connection: Option<Box<dyn TransportConnection>> = None;
    // Liveness source of the active connection, created the moment the
    // connection is established so no error can be missed between connect
    // and the Ready loop.
    let mut events: Option<Box<dyn TransportEventStream>> = None;
    let mut generation = 0_u64;

    loop {
        let next = match phase {
            Phase::Disconnected => handle_disconnected(&mut context, &mut connection).await,
            Phase::Connecting { previous_failures } => {
                handle_connecting(
                    &mut context,
                    &mut connection,
                    &mut events,
                    &mut generation,
                    previous_failures,
                )
                .await
            }
            Phase::Ready => handle_ready(&mut context, &mut connection, &mut events).await,
            Phase::Recovering { failures, error } => {
                handle_recovering(&mut context, &mut connection, failures, &error).await
            }
            Phase::FailedPermanent => handle_permanent_failure(&mut context, &mut connection).await,
        };

        let Some(next) = next else {
            return;
        };
        phase = next;
    }
}

async fn handle_disconnected(
    context: &mut ActorContext,
    connection: &mut Option<Box<dyn TransportConnection>>,
) -> Option<Phase> {
    match context.commands.recv().await {
        Some(Command::Start) => Some(Phase::Connecting {
            previous_failures: 0,
        }),
        Some(Command::ConnectionLost(_)) => Some(Phase::Disconnected),
        Some(Command::OpenPublisher(completed)) => {
            let _ = completed.send(Err(TransportError::closed("connection is not ready")));
            Some(Phase::Disconnected)
        }
        Some(Command::OpenConsumer(completed)) => {
            let _ = completed.send(Err(TransportError::closed("connection is not ready")));
            Some(Phase::Disconnected)
        }
        Some(Command::Close(completed)) => {
            shutdown(&context.states, connection, completed).await;
            None
        }
        None => {
            close_connection(connection).await;
            None
        }
    }
}

async fn handle_connecting(
    context: &mut ActorContext,
    connection: &mut Option<Box<dyn TransportConnection>>,
    events: &mut Option<Box<dyn TransportEventStream>>,
    generation: &mut u64,
    previous_failures: u32,
) -> Option<Phase> {
    context.states.send_replace(ConnectionState::Connecting {
        attempt: previous_failures.saturating_add(1),
    });
    tokio::task::yield_now().await;

    let connect = context.transport.connect(&context.config);
    tokio::pin!(connect);
    let result = loop {
        tokio::select! {
            result = tokio::time::timeout(CONNECT_TIMEOUT, &mut connect) => {
                break result
                    .unwrap_or_else(|_| Err(TransportError::connection(
                        "connect attempt timed out",
                    )));
            }
            command = context.commands.recv() => match command {
                Some(Command::Close(completed)) => {
                    shutdown(&context.states, connection, completed).await;
                    return None;
                }
                None => {
                    close_connection(connection).await;
                    return None;
                }
                Some(Command::OpenPublisher(completed)) => {
                    let _ = completed.send(Err(TransportError::closed("connection is not ready")));
                }
                Some(Command::OpenConsumer(completed)) => {
                    let _ = completed.send(Err(TransportError::closed("connection is not ready")));
                }
                Some(Command::ConnectionLost(new_error)) if !new_error.is_recoverable() => {
                    publish_permanent_failure(&context.states, &new_error);
                    return Some(Phase::FailedPermanent);
                }
                Some(Command::Start | Command::ConnectionLost(_)) => {}
            }
        }
    };

    match result {
        Ok(new_connection) => {
            *events = Some(new_connection.event_stream());
            *connection = Some(new_connection);
            *generation = generation.saturating_add(1);
            if *generation > 1 {
                context.metrics.record_reconnect();
            }
            crate::log::info(
                "connection_actor",
                format!(
                    "broker '{}' connected (generation {})",
                    context.config.name, *generation
                ),
            );
            context.states.send_replace(ConnectionState::Ready {
                generation: *generation,
            });
            Some(Phase::Ready)
        }
        Err(error) if error.is_recoverable() => {
            crate::log::warn(
                "connection_actor",
                format!(
                    "broker '{}' connect attempt {} failed: {error}",
                    context.config.name,
                    previous_failures.saturating_add(1)
                ),
            );
            Some(Phase::Recovering {
                failures: previous_failures.saturating_add(1),
                error,
            })
        }
        Err(error) => {
            crate::log::error(
                "connection_actor",
                format!(
                    "broker '{}' failed permanently: {error}",
                    context.config.name
                ),
            );
            publish_permanent_failure(&context.states, &error);
            Some(Phase::FailedPermanent)
        }
    }
}

async fn handle_ready(
    context: &mut ActorContext,
    connection: &mut Option<Box<dyn TransportConnection>>,
    events: &mut Option<Box<dyn TransportEventStream>>,
) -> Option<Phase> {
    // The liveness source lives for the whole Ready phase; every exit of
    // this loop is a phase change where the connection dies or is closed, so
    // the taken stream is simply dropped with the local binding.
    let mut events = events.take();
    loop {
        tokio::select! {
            // The transport itself reports the connection is dying (socket
            // reset, heartbeat failure): route it exactly like a reported
            // `Command::ConnectionLost`.
            event = next_transport_event(&mut events) => {
                match event {
                    TransportEvent::Error(error) => {
                        // A dead connection is never blocked: its successor
                        // starts unblocked. The episode counter survives.
                        context.metrics.clear_connection_blocked();
                        close_connection(connection).await;
                        return Some(route_loss(&context.states, error));
                    }
                    TransportEvent::Blocked(reason) => {
                        context.metrics.record_connection_blocked();
                        crate::log::warn(
                            "connection_actor",
                            format!(
                                "broker '{}' blocked: {}",
                                context.config.name,
                                truncate_reason(&reason),
                            ),
                        );
                    }
                    TransportEvent::Unblocked => {
                        context.metrics.clear_connection_blocked();
                        crate::log::info(
                            "connection_actor",
                            format!("broker '{}' unblocked", context.config.name),
                        );
                    }
                }
            }
            command = context.commands.recv() => {
                match command {
                    Some(Command::ConnectionLost(error)) => {
                        context.metrics.clear_connection_blocked();
                        close_connection(connection).await;
                        return Some(route_loss(&context.states, error));
                    }
                    Some(Command::OpenPublisher(completed)) => {
                        let outcome = match connection.as_deref() {
                            Some(conn) => bounded_channel_operation(conn.open_publisher()).await,
                            None => Ok(Err(TransportError::closed("connection is not ready"))),
                        };
                        match outcome {
                            Ok(result) => {
                                let _ = completed.send(result);
                            }
                            Err(error) => {
                                // The open outlived its budget: the connection
                                // can no longer make progress. Reply to the
                                // caller, then treat it exactly like a reported
                                // loss so queued commands are serviced again
                                // instead of parking behind the wedged call.
                                let _ = completed.send(Err(error.clone()));
                                context.metrics.clear_connection_blocked();
                                close_connection(connection).await;
                                return Some(route_loss(&context.states, error));
                            }
                        }
                    }
                    Some(Command::OpenConsumer(completed)) => {
                        let outcome = match connection.as_deref() {
                            Some(conn) => bounded_channel_operation(conn.open_consumer()).await,
                            None => Ok(Err(TransportError::closed("connection is not ready"))),
                        };
                        match outcome {
                            Ok(result) => {
                                let _ = completed.send(result);
                            }
                            Err(error) => {
                                let _ = completed.send(Err(error.clone()));
                                context.metrics.clear_connection_blocked();
                                close_connection(connection).await;
                                return Some(route_loss(&context.states, error));
                            }
                        }
                    }
                    Some(Command::Start) => {}
                    Some(Command::Close(completed)) => {
                        context.metrics.clear_connection_blocked();
                        shutdown(&context.states, connection, completed).await;
                        return None;
                    }
                    None => {
                        context.metrics.clear_connection_blocked();
                        close_connection(connection).await;
                        return None;
                    }
                }
            }
        }
    }
}

/// Waits for the next event of the active connection. Pends forever when
/// there is no connection, leaving commands as the only wake-up source.
async fn next_transport_event(
    events: &mut Option<Box<dyn TransportEventStream>>,
) -> TransportEvent {
    match events {
        Some(stream) => stream.next().await.unwrap_or_else(|| {
            TransportEvent::Error(TransportError::connection("transport event stream ended"))
        }),
        None => std::future::pending().await,
    }
}

/// Routes a connection loss to recovery or permanent failure based on the
/// error's recoverability.
fn route_loss(states: &watch::Sender<ConnectionState>, error: TransportError) -> Phase {
    if error.is_recoverable() {
        Phase::Recovering { failures: 1, error }
    } else {
        publish_permanent_failure(states, &error);
        Phase::FailedPermanent
    }
}

async fn handle_recovering(
    context: &mut ActorContext,
    connection: &mut Option<Box<dyn TransportConnection>>,
    failures: u32,
    error: &TransportError,
) -> Option<Phase> {
    let retry_in = context
        .jitter
        .apply(context.policy.delay_for_failure(failures));
    context.states.send_replace(ConnectionState::Recovering {
        attempt: failures,
        retry_in,
        reason: error.to_string(),
    });

    let sleep = context.clock.sleep(retry_in);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            () = &mut sleep => return Some(Phase::Connecting {
                previous_failures: failures,
            }),
            command = context.commands.recv() => match command {
                Some(Command::Close(completed)) => {
                    shutdown(&context.states, connection, completed).await;
                    return None;
                }
                Some(Command::ConnectionLost(new_error)) if !new_error.is_recoverable() => {
                    publish_permanent_failure(&context.states, &new_error);
                    return Some(Phase::FailedPermanent);
                }
                Some(Command::OpenPublisher(completed)) => {
                    let _ = completed.send(Err(TransportError::closed("connection is not ready")));
                }
                Some(Command::OpenConsumer(completed)) => {
                    let _ = completed.send(Err(TransportError::closed("connection is not ready")));
                }
                Some(Command::Start | Command::ConnectionLost(_)) => {}
                None => {
                    close_connection(connection).await;
                    return None;
                }
            }
        }
    }
}

async fn handle_permanent_failure(
    context: &mut ActorContext,
    connection: &mut Option<Box<dyn TransportConnection>>,
) -> Option<Phase> {
    match context.commands.recv().await {
        Some(Command::Start) => Some(Phase::Connecting {
            previous_failures: 0,
        }),
        Some(Command::ConnectionLost(_)) => Some(Phase::FailedPermanent),
        Some(Command::OpenPublisher(completed)) => {
            let _ = completed.send(Err(TransportError::closed("connection is not ready")));
            Some(Phase::FailedPermanent)
        }
        Some(Command::OpenConsumer(completed)) => {
            let _ = completed.send(Err(TransportError::closed("connection is not ready")));
            Some(Phase::FailedPermanent)
        }
        Some(Command::Close(completed)) => {
            shutdown(&context.states, connection, completed).await;
            None
        }
        None => {
            close_connection(connection).await;
            None
        }
    }
}

fn publish_permanent_failure(states: &watch::Sender<ConnectionState>, error: &TransportError) {
    states.send_replace(ConnectionState::FailedPermanent {
        kind: error.kind(),
        reason: error.to_string(),
    });
}

async fn shutdown(
    states: &watch::Sender<ConnectionState>,
    connection: &mut Option<Box<dyn TransportConnection>>,
    completed: oneshot::Sender<()>,
) {
    close_connection(connection).await;
    states.send_replace(ConnectionState::Closed);
    let _ = completed.send(());
}

async fn close_connection(connection: &mut Option<Box<dyn TransportConnection>>) {
    let Some(connection) = connection.take() else {
        return;
    };
    // A wedged transport must not stall shutdown or a loss transition past
    // the same budget as any other connection operation: the socket is
    // dropped either way, so the close result is best-effort.
    if tokio::time::timeout(CONNECT_TIMEOUT, connection.close())
        .await
        .is_err()
    {
        crate::log::warn(
            "connection_actor",
            "connection close exceeded its budget; abandoning the connection",
        );
    }
}

/// Bounds one per-command channel operation by [`CONNECT_TIMEOUT`], the same
/// deadline as a connect attempt.
///
/// A channel open that outlives that budget means the connection can no
/// longer make progress; the caller receives a recoverable transport error
/// and the actor treats the connection as lost instead of parking the
/// command loop — with every queued `Close` and `ConnectionLost` behind it —
/// until heartbeat detection, configurable up to 65535 s, happens to notice.
async fn bounded_channel_operation<T>(
    operation: impl Future<Output = TransportResult<T>>,
) -> Result<TransportResult<T>, TransportError> {
    tokio::time::timeout(CONNECT_TIMEOUT, operation)
        .await
        .map_err(|_| TransportError::connection("channel operation timed out"))
}

/// Caps the broker-provided blocked reason in log output. The string is a
/// protocol-provided diagnostic, but it is still external input and must not
/// balloon a log line.
const BLOCKED_REASON_MAX_CHARS: usize = 200;

fn truncate_reason(reason: &str) -> String {
    reason.chars().take(BLOCKED_REASON_MAX_CHARS).collect()
}
