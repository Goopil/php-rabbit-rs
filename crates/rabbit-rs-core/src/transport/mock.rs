use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;
use tokio::sync::oneshot;

use super::{
    BindingSpec, ConsumerChannel, ConsumerRequest, Delivery, DeliveryStream, ExchangeSpec,
    PublishConfirmation, PublishReceipt, PublishRequest, PublisherChannel, QueueSpec,
    TopologyChannel, Transport, TransportConnection, TransportError, TransportResult,
};
use crate::config::BrokerConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportOperation {
    Connect { broker: String },
    OpenPublisher,
    OpenConsumer,
    DeclareExchange(ExchangeSpec),
    VerifyExchange(ExchangeSpec),
    DeclareQueue(QueueSpec),
    VerifyQueue(QueueSpec),
    BindQueue(BindingSpec),
    QueueSize { queue: String, result: u32 },
    PurgeQueue { queue: String },
    EnableConfirms,
    Publish(PublishRequest),
    PublishBatch(Vec<PublishRequest>),
    Qos { prefetch: u16 },
    Consume(ConsumerRequest),
    Ack { delivery_tag: u64, multiple: bool },
    Reject { delivery_tag: u64, requeue: bool },
    CloseChannel,
    CloseConnection,
}

#[derive(Default)]
struct MockState {
    operations: Vec<TransportOperation>,
    connect_results: VecDeque<TransportResult<()>>,
    confirmations: VecDeque<MockConfirmation>,
    deliveries: VecDeque<TransportResult<Delivery>>,
    keep_delivery_stream_open: bool,
    operation_results: VecDeque<TransportResult<()>>,
    consumer_results: VecDeque<TransportResult<()>>,
    queue_sizes: VecDeque<TransportResult<u32>>,
    connect_gates: VecDeque<MockOperationGateWait>,
    open_publisher_gates: VecDeque<MockOperationGateWait>,
    open_consumer_gates: VecDeque<MockOperationGateWait>,
    close_connection_gates: VecDeque<MockOperationGateWait>,
    close_channel_gates: VecDeque<MockOperationGateWait>,
    ack_gates: VecDeque<MockOperationGateWait>,
    delivery_gates: VecDeque<MockOperationGateWait>,
    publish_gates: VecDeque<MockPublishGateWait>,
}

#[derive(Clone, Default)]
pub struct MockTransport {
    state: Arc<Mutex<MockState>>,
}

impl MockTransport {
    pub fn push_connect_result(&self, result: TransportResult<()>) {
        self.state().connect_results.push_back(result);
    }

    pub fn push_confirmation(&self, result: TransportResult<PublishConfirmation>) {
        self.state()
            .confirmations
            .push_back(MockConfirmation::Ready(result));
    }

    pub fn push_pending_confirmation(&self) {
        self.state()
            .confirmations
            .push_back(MockConfirmation::Pending);
    }

    #[must_use]
    pub fn push_controlled_confirmation(&self) -> MockConfirmationController {
        let (sender, receiver) = oneshot::channel();
        self.state()
            .confirmations
            .push_back(MockConfirmation::Controlled(receiver));
        MockConfirmationController {
            sender: Arc::new(Mutex::new(Some(sender))),
        }
    }

    pub fn push_delivery(&self, delivery: TransportResult<Delivery>) {
        self.state().deliveries.push_back(delivery);
    }

    pub fn keep_delivery_stream_open(&self) {
        self.state().keep_delivery_stream_open = true;
    }

    pub fn push_operation_result(&self, result: TransportResult<()>) {
        self.state().operation_results.push_back(result);
    }

    pub fn push_consumer_result(&self, result: TransportResult<()>) {
        self.state().consumer_results.push_back(result);
    }

    pub fn push_queue_size(&self, result: TransportResult<u32>) {
        self.state().queue_sizes.push_back(result);
    }

    #[must_use]
    pub fn push_connect_gate(&self) -> MockOperationGate {
        let (wait, gate) = operation_gate();
        self.state().connect_gates.push_back(wait);
        gate
    }

    #[must_use]
    pub fn push_open_publisher_gate(&self) -> MockOperationGate {
        let (wait, gate) = operation_gate();
        self.state().open_publisher_gates.push_back(wait);
        gate
    }

    #[must_use]
    pub fn push_open_consumer_gate(&self) -> MockOperationGate {
        let (wait, gate) = operation_gate();
        self.state().open_consumer_gates.push_back(wait);
        gate
    }

    #[must_use]
    pub fn push_close_connection_gate(&self) -> MockOperationGate {
        let (wait, gate) = operation_gate();
        self.state().close_connection_gates.push_back(wait);
        gate
    }

    /// Pushes a gate that makes the next `PublisherChannel::close()` or
    /// `ConsumerChannel::close()` call pending until the returned gate is
    /// released.
    ///
    /// This simulates a broker or transport that stalls during channel
    /// shutdown, allowing tests to verify that close deadlines are enforced.
    #[must_use]
    pub fn push_close_channel_gate(&self) -> MockOperationGate {
        let (wait, gate) = operation_gate();
        self.state().close_channel_gates.push_back(wait);
        gate
    }

    #[must_use]
    pub fn push_ack_gate(&self) -> MockOperationGate {
        let (wait, gate) = operation_gate();
        self.state().ack_gates.push_back(wait);
        gate
    }

    #[must_use]
    pub fn push_delivery_gate(&self) -> MockOperationGate {
        let (wait, gate) = operation_gate();
        self.state().delivery_gates.push_back(wait);
        gate
    }

    /// Pushes a publish gate that makes the next `publish()` call pending
    /// until the returned [`MockPublishGate`] is released.
    ///
    /// This simulates Lapin's buffer-full scenario where `basic_publish`
    /// does not complete on the first poll, exercising the slow-path
    /// (`FuturesUnordered`) in the publisher actor.
    #[must_use]
    pub fn push_publish_gate(&self) -> MockPublishGate {
        let (wait, gate) = publish_gate();
        self.state().publish_gates.push_back(wait);
        gate
    }

    #[must_use]
    pub fn operations(&self) -> Vec<TransportOperation> {
        self.state().operations.clone()
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct MockOperationGateWait {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

#[derive(Clone)]
pub struct MockOperationGate {
    entered: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    release: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl MockOperationGate {
    pub async fn wait_entered(&self) {
        let Some(receiver) = self
            .entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let _ = receiver.await;
    }

    #[must_use]
    pub fn release(&self) -> bool {
        self.release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some_and(|sender| sender.send(()).is_ok())
    }
}

fn operation_gate() -> (MockOperationGateWait, MockOperationGate) {
    let (entered_sender, entered_receiver) = oneshot::channel();
    let (release_sender, release_receiver) = oneshot::channel();
    (
        MockOperationGateWait {
            entered: entered_sender,
            release: release_receiver,
        },
        MockOperationGate {
            entered: Arc::new(Mutex::new(Some(entered_receiver))),
            release: Arc::new(Mutex::new(Some(release_sender))),
        },
    )
}

async fn wait_for_gate(gate: Option<MockOperationGateWait>) {
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
}

struct MockPublishGateWait {
    release: oneshot::Receiver<()>,
}

#[derive(Clone)]
pub struct MockPublishGate {
    release: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl MockPublishGate {
    /// Releases the gated publish, allowing it to complete.
    pub fn release(&self) -> bool {
        self.release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some_and(|sender| sender.send(()).is_ok())
    }
}

fn publish_gate() -> (MockPublishGateWait, MockPublishGate) {
    let (release_sender, release_receiver) = oneshot::channel();
    (
        MockPublishGateWait {
            release: release_receiver,
        },
        MockPublishGate {
            release: Arc::new(Mutex::new(Some(release_sender))),
        },
    )
}

async fn wait_for_publish_gate(gate: Option<MockPublishGateWait>) {
    if let Some(gate) = gate {
        let _ = gate.release.await;
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn connect(
        &self,
        config: &BrokerConfig,
    ) -> TransportResult<Box<dyn TransportConnection>> {
        let (gate, result) = {
            let mut state = self.state();
            state.operations.push(TransportOperation::Connect {
                broker: config.name.clone(),
            });
            (
                state.connect_gates.pop_front(),
                state.connect_results.pop_front().unwrap_or(Ok(())),
            )
        };
        wait_for_gate(gate).await;
        result?;

        Ok(Box::new(MockConnection {
            state: self.state.clone(),
        }))
    }
}

struct MockConnection {
    state: Arc<Mutex<MockState>>,
}

impl MockConnection {
    fn state(&self) -> MutexGuard<'_, MockState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl TransportConnection for MockConnection {
    async fn open_publisher(&self) -> TransportResult<Box<dyn PublisherChannel>> {
        let gate = {
            let mut state = self.state();
            state.operations.push(TransportOperation::OpenPublisher);
            state.open_publisher_gates.pop_front()
        };
        wait_for_gate(gate).await;
        Ok(Box::new(MockPublisherChannel {
            state: self.state.clone(),
        }))
    }

    async fn open_consumer(&self) -> TransportResult<Box<dyn ConsumerChannel>> {
        let gate = {
            let mut state = self.state();
            state.operations.push(TransportOperation::OpenConsumer);
            state.open_consumer_gates.pop_front()
        };
        wait_for_gate(gate).await;
        Ok(Box::new(MockConsumerChannel {
            state: self.state.clone(),
        }))
    }

    async fn close(&self) -> TransportResult<()> {
        let gate = {
            let mut state = self.state();
            state.operations.push(TransportOperation::CloseConnection);
            state.close_connection_gates.pop_front()
        };
        wait_for_gate(gate).await;
        Ok(())
    }
}

struct MockPublisherChannel {
    state: Arc<Mutex<MockState>>,
}

impl MockPublisherChannel {
    fn record(&self, operation: TransportOperation) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .operations
            .push(operation);
    }

    fn record_topology(&self, operation: TransportOperation) -> TransportResult<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.operations.push(operation);
        state.operation_results.pop_front().unwrap_or(Ok(()))
    }
}

#[async_trait]
impl TopologyChannel for MockPublisherChannel {
    async fn declare_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::DeclareExchange(spec.clone()))
    }

    async fn verify_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::VerifyExchange(spec.clone()))
    }

    async fn declare_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::DeclareQueue(spec.clone()))
    }

    async fn verify_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::VerifyQueue(spec.clone()))
    }

    async fn bind_queue(&self, spec: &BindingSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::BindQueue(spec.clone()))
    }

    async fn queue_size(&self, queue: &str) -> TransportResult<u32> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.operations.push(TransportOperation::QueueSize {
            queue: queue.to_owned(),
            result: 0,
        });
        state.queue_sizes.pop_front().unwrap_or(Ok(0))
    }

    async fn purge_queue(&self, queue: &str) -> TransportResult<()> {
        self.record_topology(TransportOperation::PurgeQueue {
            queue: queue.to_owned(),
        })
    }

    async fn close(&self) -> TransportResult<()> {
        let gate = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.operations.push(TransportOperation::CloseChannel);
            state.close_channel_gates.pop_front()
        };
        wait_for_gate(gate).await;
        Ok(())
    }
}

#[async_trait]
impl PublisherChannel for MockPublisherChannel {
    async fn enable_confirms(&self) -> TransportResult<()> {
        self.record(TransportOperation::EnableConfirms);
        Ok(())
    }

    async fn publish(&self, request: PublishRequest) -> TransportResult<Box<dyn PublishReceipt>> {
        let gate = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.publish_gates.pop_front()
        };
        wait_for_publish_gate(gate).await;

        self.record(TransportOperation::Publish(request));
        let result = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .confirmations
            .pop_front()
            .unwrap_or(MockConfirmation::Ready(Ok(
                PublishConfirmation::NotRequested,
            )));

        Ok(Box::new(MockPublishReceipt {
            confirmation: Some(result),
        }))
    }

    async fn publish_batch(
        &self,
        requests: Vec<PublishRequest>,
    ) -> TransportResult<Vec<Box<dyn PublishReceipt>>> {
        let count = requests.len();
        self.record(TransportOperation::PublishBatch(requests));
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut receipts = Vec::with_capacity(count);
        for _ in 0..count {
            let result = state
                .confirmations
                .pop_front()
                .unwrap_or(MockConfirmation::Ready(Ok(
                    PublishConfirmation::NotRequested,
                )));
            receipts.push(Box::new(MockPublishReceipt {
                confirmation: Some(result),
            }) as Box<dyn PublishReceipt>);
        }
        Ok(receipts)
    }
}

enum MockConfirmation {
    Ready(TransportResult<PublishConfirmation>),
    Pending,
    Controlled(oneshot::Receiver<TransportResult<PublishConfirmation>>),
}

#[derive(Clone)]
pub struct MockConfirmationController {
    sender: Arc<Mutex<Option<oneshot::Sender<TransportResult<PublishConfirmation>>>>>,
}

impl MockConfirmationController {
    pub fn resolve(&self, result: TransportResult<PublishConfirmation>) -> bool {
        self.sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some_and(|sender| sender.send(result).is_ok())
    }
}

struct MockPublishReceipt {
    confirmation: Option<MockConfirmation>,
}

#[async_trait]
impl PublishReceipt for MockPublishReceipt {
    async fn wait(mut self: Box<Self>) -> TransportResult<PublishConfirmation> {
        match self
            .confirmation
            .take()
            .ok_or_else(|| TransportError::closed("confirmation already consumed"))?
        {
            MockConfirmation::Ready(result) => result,
            MockConfirmation::Pending => std::future::pending().await,
            MockConfirmation::Controlled(receiver) => receiver
                .await
                .unwrap_or_else(|_| Err(TransportError::closed("confirmation was cancelled"))),
        }
    }
}

struct MockConsumerChannel {
    state: Arc<Mutex<MockState>>,
}

impl MockConsumerChannel {
    fn record_topology(&self, operation: TransportOperation) -> TransportResult<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.operations.push(operation);
        state.operation_results.pop_front().unwrap_or(Ok(()))
    }

    fn record_consumer(&self, operation: TransportOperation) -> TransportResult<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.operations.push(operation);
        state.consumer_results.pop_front().unwrap_or(Ok(()))
    }
}

#[async_trait]
impl TopologyChannel for MockConsumerChannel {
    async fn declare_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::DeclareExchange(spec.clone()))
    }

    async fn verify_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::VerifyExchange(spec.clone()))
    }

    async fn declare_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::DeclareQueue(spec.clone()))
    }

    async fn verify_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::VerifyQueue(spec.clone()))
    }

    async fn bind_queue(&self, spec: &BindingSpec) -> TransportResult<()> {
        self.record_topology(TransportOperation::BindQueue(spec.clone()))
    }

    async fn queue_size(&self, queue: &str) -> TransportResult<u32> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.operations.push(TransportOperation::QueueSize {
            queue: queue.to_owned(),
            result: 0,
        });
        state.queue_sizes.pop_front().unwrap_or(Ok(0))
    }

    async fn purge_queue(&self, queue: &str) -> TransportResult<()> {
        self.record_topology(TransportOperation::PurgeQueue {
            queue: queue.to_owned(),
        })
    }

    async fn close(&self) -> TransportResult<()> {
        let gate = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.operations.push(TransportOperation::CloseChannel);
            state.close_channel_gates.pop_front()
        };
        wait_for_gate(gate).await;
        Ok(())
    }
}

#[async_trait]
impl ConsumerChannel for MockConsumerChannel {
    async fn set_qos(&self, prefetch: u16) -> TransportResult<()> {
        self.record_consumer(TransportOperation::Qos { prefetch })
    }

    async fn consume(&self, request: ConsumerRequest) -> TransportResult<Box<dyn DeliveryStream>> {
        self.record_consumer(TransportOperation::Consume(request))?;
        Ok(Box::new(MockDeliveryStream {
            state: self.state.clone(),
        }))
    }

    async fn ack(&self, delivery_tag: u64, multiple: bool) -> TransportResult<()> {
        let (gate, result) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.operations.push(TransportOperation::Ack {
                delivery_tag,
                multiple,
            });
            (
                state.ack_gates.pop_front(),
                state.consumer_results.pop_front().unwrap_or(Ok(())),
            )
        };
        wait_for_gate(gate).await;
        result
    }

    async fn reject(&self, delivery_tag: u64, requeue: bool) -> TransportResult<()> {
        self.record_consumer(TransportOperation::Reject {
            delivery_tag,
            requeue,
        })
    }
}

struct MockDeliveryStream {
    state: Arc<Mutex<MockState>>,
}

#[async_trait]
impl DeliveryStream for MockDeliveryStream {
    async fn next(&mut self) -> Option<TransportResult<Delivery>> {
        let (gate, delivery) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                state.delivery_gates.pop_front(),
                state.deliveries.pop_front(),
            )
        };
        wait_for_gate(gate).await;
        if let Some(delivery) = delivery {
            return Some(delivery);
        }
        let keep_open = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keep_delivery_stream_open;
        if keep_open {
            std::future::pending().await
        } else {
            None
        }
    }
}
