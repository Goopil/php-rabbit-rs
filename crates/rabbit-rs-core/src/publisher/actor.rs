use std::{future, sync::Arc, time::Duration};

use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use tokio::{
    sync::{mpsc, oneshot},
    time,
};

use crate::transport::{
    PublishConfirmation, PublishProperties as TransportProperties,
    PublishRequest as TransportRequest, PublisherChannel, TransportError, TransportResult,
};

use super::{
    PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublishWaiter, PublisherConfig,
    ReturnInfo,
    batcher::Batcher,
    confirms::{ConfirmLedger, PendingConfirmation},
};

pub struct PublisherActor;

impl PublisherActor {
    #[must_use]
    pub fn spawn(channel: Arc<dyn PublisherChannel>, config: PublisherConfig) -> PublisherHandle {
        let (commands, receiver) = mpsc::channel(config.buffer_capacity.max(1));
        tokio::spawn(run_actor(channel, config, receiver));
        PublisherHandle { commands }
    }
}

#[derive(Clone, Debug)]
pub struct PublisherHandle {
    commands: mpsc::Sender<Command>,
}

impl PublisherHandle {
    /// Enqueues a publish without waiting when the bounded command queue is full.
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Backpressure`] for a full queue or
    /// [`PublishErrorKind::Closed`] when the actor has stopped.
    pub fn try_publish(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError> {
        let (completion, receiver) = oneshot::channel();
        let command = Command::Publish(QueuedPublish {
            request,
            completion,
        });

        match self.commands.try_send(command) {
            Ok(()) => Ok(PublishWaiter::new(receiver)),
            Err(mpsc::error::TrySendError::Full(_)) => Err(PublishError::new(
                PublishErrorKind::Backpressure,
                "publisher command buffer is full",
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(PublishError::new(
                PublishErrorKind::Closed,
                "publisher actor is closed",
            )),
        }
    }

    /// Marks all sent but unconfirmed messages ambiguous.
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Closed`] if the actor is no longer running.
    pub async fn connection_lost(&self) -> Result<(), PublishError> {
        let (completed, completion) = oneshot::channel();
        self.commands
            .send(Command::ConnectionLost(completed))
            .await
            .map_err(|_| {
                PublishError::new(PublishErrorKind::Closed, "publisher actor is closed")
            })?;
        completion.await.map_err(|_| {
            PublishError::new(
                PublishErrorKind::Closed,
                "publisher actor stopped while handling connection loss",
            )
        })
    }

    /// Stops the actor after resolving buffered and pending commands safely.
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Closed`] if the actor already stopped.
    pub async fn close(&self) -> Result<(), PublishError> {
        let (completed, completion) = oneshot::channel();
        self.commands
            .send(Command::Close(completed))
            .await
            .map_err(|_| {
                PublishError::new(PublishErrorKind::Closed, "publisher actor is closed")
            })?;
        completion.await.map_err(|_| {
            PublishError::new(
                PublishErrorKind::Closed,
                "publisher actor stopped during shutdown",
            )
        })
    }
}

#[derive(Debug)]
enum Command {
    Publish(QueuedPublish),
    ConnectionLost(oneshot::Sender<()>),
    Close(oneshot::Sender<()>),
}

#[derive(Debug)]
struct QueuedPublish {
    request: PublishRequest,
    completion: oneshot::Sender<Result<PublishOutcome, PublishError>>,
}

enum ConfirmationResult {
    Completed(TransportResult<PublishConfirmation>),
    TimedOut,
}

type ConfirmationFuture = BoxFuture<'static, (u64, ConfirmationResult)>;

async fn run_actor(
    channel: Arc<dyn PublisherChannel>,
    config: PublisherConfig,
    mut commands: mpsc::Receiver<Command>,
) {
    let mut batch = Batcher::new(config.max_messages, config.max_bytes);
    let mut ledger = ConfirmLedger::default();
    let mut confirmations = FuturesUnordered::<ConfirmationFuture>::new();
    let mut sequence = 0_u64;
    let flush_interval = if config.flush_interval.is_zero() {
        Duration::from_nanos(1)
    } else {
        config.flush_interval
    };
    let mut flush_deadline = None;

    if let Err(error) = channel.enable_confirms().await {
        reject_until_closed(&mut commands, error).await;
        return;
    }

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Publish(queued)) => {
                    let payload_len = queued.request.payload.len();
                    if batch.is_empty() {
                        flush_deadline = Some(time::Instant::now() + flush_interval);
                    }
                    if batch.push(queued, payload_len) {
                        flush_batch(
                            &channel,
                            &mut batch,
                            &mut ledger,
                            &mut confirmations,
                            &mut sequence,
                            config.confirm_timeout,
                        ).await;
                        flush_deadline = None;
                    }
                }
                Some(Command::ConnectionLost(completed)) => {
                    fail_buffered(&mut batch);
                    flush_deadline = None;
                    resolve_ambiguous(&mut ledger);
                    confirmations = FuturesUnordered::new();
                    let _ = completed.send(());
                }
                Some(Command::Close(completed)) => {
                    fail_buffered(&mut batch);
                    resolve_ambiguous(&mut ledger);
                    drop(confirmations);
                    let _ = channel.close().await;
                    let _ = completed.send(());
                    return;
                }
                None => {
                    fail_buffered(&mut batch);
                    resolve_ambiguous(&mut ledger);
                    let _ = channel.close().await;
                    return;
                }
            },
            () = wait_for_flush(flush_deadline) => {
                flush_batch(
                    &channel,
                    &mut batch,
                    &mut ledger,
                    &mut confirmations,
                    &mut sequence,
                    config.confirm_timeout,
                ).await;
                flush_deadline = None;
            }
            confirmation = confirmations.next(), if !confirmations.is_empty() => {
                if let Some((confirmed_sequence, result)) = confirmation {
                    resolve_confirmation(&mut ledger, confirmed_sequence, result);
                }
            }
        }
    }
}

async fn wait_for_flush(deadline: Option<time::Instant>) {
    if let Some(deadline) = deadline {
        time::sleep_until(deadline).await;
    } else {
        future::pending::<()>().await;
    }
}

async fn flush_batch(
    channel: &Arc<dyn PublisherChannel>,
    batch: &mut Batcher<QueuedPublish>,
    ledger: &mut ConfirmLedger,
    confirmations: &mut FuturesUnordered<ConfirmationFuture>,
    sequence: &mut u64,
    confirm_timeout: Duration,
) {
    if batch.is_empty() {
        return;
    }

    for queued in batch.take() {
        *sequence = sequence.saturating_add(1);
        let current_sequence = *sequence;
        let message_id = queued.request.properties.message_id.clone();
        let deadline = queued
            .request
            .deadline
            .min(time::Instant::now() + confirm_timeout);
        let transport_request = into_transport_request(queued.request);

        match channel.publish(transport_request).await {
            Ok(receipt) => {
                ledger.insert(
                    current_sequence,
                    PendingConfirmation {
                        message_id,
                        completion: queued.completion,
                    },
                );
                confirmations.push(Box::pin(async move {
                    let result = match time::timeout_at(deadline, receipt.wait()).await {
                        Ok(result) => ConfirmationResult::Completed(result),
                        Err(_) => ConfirmationResult::TimedOut,
                    };
                    (current_sequence, result)
                }));
            }
            Err(error) => {
                let outcome = if error.is_recoverable() {
                    Ok(PublishOutcome::Ambiguous { message_id })
                } else {
                    Err(transport_publish_error(&error))
                };
                let _ = queued.completion.send(outcome);
            }
        }
    }
}

fn into_transport_request(request: PublishRequest) -> TransportRequest {
    TransportRequest {
        exchange: request.destination.exchange,
        routing_key: request.destination.routing_key,
        payload: request.payload,
        mandatory: true,
        properties: TransportProperties {
            content_type: request.properties.content_type,
            correlation_id: request.properties.correlation_id,
            message_id: Some(request.properties.message_id),
            persistent: true,
        },
    }
}

fn resolve_confirmation(ledger: &mut ConfirmLedger, sequence: u64, result: ConfirmationResult) {
    let Some(pending) = ledger.remove(sequence) else {
        return;
    };
    let outcome = match result {
        ConfirmationResult::TimedOut => Err(PublishError::new(
            PublishErrorKind::Timeout,
            "publisher confirmation timed out",
        )),
        ConfirmationResult::Completed(Err(error)) if error.is_recoverable() => {
            Ok(PublishOutcome::Ambiguous {
                message_id: pending.message_id.clone(),
            })
        }
        ConfirmationResult::Completed(Err(error)) => Err(transport_publish_error(&error)),
        ConfirmationResult::Completed(Ok(
            PublishConfirmation::Ack(Some(returned)) | PublishConfirmation::Nack(Some(returned)),
        )) => Ok(PublishOutcome::Returned {
            message_id: pending.message_id.clone(),
            reply: ReturnInfo {
                code: returned.reply_code,
                text: returned.reply_text,
                exchange: returned.exchange,
                routing_key: returned.routing_key,
            },
        }),
        ConfirmationResult::Completed(Ok(PublishConfirmation::Ack(None))) => {
            Ok(PublishOutcome::Confirmed {
                message_id: pending.message_id.clone(),
            })
        }
        ConfirmationResult::Completed(Ok(PublishConfirmation::Nack(None))) => {
            Err(PublishError::new(
                PublishErrorKind::Nack,
                "broker negatively acknowledged the message",
            ))
        }
        ConfirmationResult::Completed(Ok(PublishConfirmation::NotRequested)) => {
            Err(PublishError::new(
                PublishErrorKind::Unconfirmed,
                "publisher confirms were not enabled",
            ))
        }
    };
    let _ = pending.completion.send(outcome);
}

fn fail_buffered(batch: &mut Batcher<QueuedPublish>) {
    for queued in batch.take() {
        let _ = queued.completion.send(Err(PublishError::new(
            PublishErrorKind::Closed,
            "message was not sent before the publisher connection closed",
        )));
    }
}

fn resolve_ambiguous(ledger: &mut ConfirmLedger) {
    for pending in ledger.drain() {
        let _ = pending.completion.send(Ok(PublishOutcome::Ambiguous {
            message_id: pending.message_id,
        }));
    }
}

fn transport_publish_error(error: &TransportError) -> PublishError {
    PublishError::new(PublishErrorKind::Transport, error.to_string())
}

async fn reject_until_closed(commands: &mut mpsc::Receiver<Command>, error: TransportError) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Publish(queued) => {
                let _ = queued.completion.send(Err(transport_publish_error(&error)));
            }
            Command::ConnectionLost(completed) => {
                let _ = completed.send(());
            }
            Command::Close(completed) => {
                let _ = completed.send(());
                return;
            }
        }
    }
}
