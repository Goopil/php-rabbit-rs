use std::{error::Error, fmt, sync::Arc};

use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    config::BrokerConfig,
    recovery::{Clock, ConnectionState, EqualJitter, JitterSource, RecoveryPolicy, TokioClock},
    transport::{Transport, TransportConnection, TransportError},
};

const COMMAND_CAPACITY: usize = 32;

/// Spawns and owns the serialized lifecycle of one broker connection.
pub struct ConnectionActor;

impl ConnectionActor {
    /// Spawns an actor with production clock and equal-jitter defaults.
    #[must_use]
    pub fn spawn(
        transport: Arc<dyn Transport>,
        config: BrokerConfig,
        policy: RecoveryPolicy,
    ) -> ConnectionActorHandle {
        Self::spawn_with_dependencies(
            transport,
            config,
            policy,
            Arc::new(TokioClock),
            Arc::new(EqualJitter),
        )
    }

    /// Spawns an actor with deterministic time and jitter dependencies.
    #[must_use]
    pub fn spawn_with_dependencies(
        transport: Arc<dyn Transport>,
        config: BrokerConfig,
        policy: RecoveryPolicy,
        clock: Arc<dyn Clock>,
        jitter: Arc<dyn JitterSource>,
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
        }));

        ConnectionActorHandle {
            commands,
            states: state_receiver,
        }
    }
}

/// Cloneable command handle for a connection actor.
#[derive(Clone)]
pub struct ConnectionActorHandle {
    commands: mpsc::Sender<Command>,
    states: watch::Receiver<ConnectionState>,
}

impl ConnectionActorHandle {
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
}

async fn run_actor(mut context: ActorContext) {
    let mut phase = Phase::Disconnected;
    let mut connection: Option<Box<dyn TransportConnection>> = None;
    let mut generation = 0_u64;

    loop {
        let next = match phase {
            Phase::Disconnected => handle_disconnected(&mut context, &mut connection).await,
            Phase::Connecting { previous_failures } => {
                handle_connecting(
                    &mut context,
                    &mut connection,
                    &mut generation,
                    previous_failures,
                )
                .await
            }
            Phase::Ready => handle_ready(&mut context, &mut connection).await,
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
            result = &mut connect => break result,
            command = context.commands.recv() => match command {
                Some(Command::Close(completed)) => {
                    shutdown(&context.states, connection, completed).await;
                    return None;
                }
                None => {
                    close_connection(connection).await;
                    return None;
                }
                Some(Command::Start | Command::ConnectionLost(_)) => {}
            }
        }
    };

    match result {
        Ok(new_connection) => {
            *connection = Some(new_connection);
            *generation = generation.saturating_add(1);
            context.states.send_replace(ConnectionState::Ready {
                generation: *generation,
            });
            Some(Phase::Ready)
        }
        Err(error) if error.is_recoverable() => Some(Phase::Recovering {
            failures: previous_failures.saturating_add(1),
            error,
        }),
        Err(error) => {
            publish_permanent_failure(&context.states, &error);
            Some(Phase::FailedPermanent)
        }
    }
}

async fn handle_ready(
    context: &mut ActorContext,
    connection: &mut Option<Box<dyn TransportConnection>>,
) -> Option<Phase> {
    match context.commands.recv().await {
        Some(Command::ConnectionLost(error)) => {
            close_connection(connection).await;
            if error.is_recoverable() {
                Some(Phase::Recovering { failures: 1, error })
            } else {
                publish_permanent_failure(&context.states, &error);
                Some(Phase::FailedPermanent)
            }
        }
        Some(Command::Start) => Some(Phase::Ready),
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
    if let Some(connection) = connection.take() {
        let _ = connection.close().await;
    }
}
