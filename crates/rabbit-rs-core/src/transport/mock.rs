use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;

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
    EnableConfirms,
    Publish(PublishRequest),
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
    operation_results: VecDeque<TransportResult<()>>,
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

    pub fn push_delivery(&self, delivery: TransportResult<Delivery>) {
        self.state().deliveries.push_back(delivery);
    }

    pub fn push_operation_result(&self, result: TransportResult<()>) {
        self.state().operation_results.push_back(result);
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

#[async_trait]
impl Transport for MockTransport {
    async fn connect(
        &self,
        config: &BrokerConfig,
    ) -> TransportResult<Box<dyn TransportConnection>> {
        let mut state = self.state();
        state.operations.push(TransportOperation::Connect {
            broker: config.name.clone(),
        });
        state.connect_results.pop_front().unwrap_or(Ok(()))?;

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
        self.state()
            .operations
            .push(TransportOperation::OpenPublisher);
        Ok(Box::new(MockPublisherChannel {
            state: self.state.clone(),
        }))
    }

    async fn open_consumer(&self) -> TransportResult<Box<dyn ConsumerChannel>> {
        self.state()
            .operations
            .push(TransportOperation::OpenConsumer);
        Ok(Box::new(MockConsumerChannel {
            state: self.state.clone(),
        }))
    }

    async fn close(&self) -> TransportResult<()> {
        self.state()
            .operations
            .push(TransportOperation::CloseConnection);
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

    async fn close(&self) -> TransportResult<()> {
        self.record(TransportOperation::CloseChannel);
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
}

enum MockConfirmation {
    Ready(TransportResult<PublishConfirmation>),
    Pending,
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
        }
    }
}

struct MockConsumerChannel {
    state: Arc<Mutex<MockState>>,
}

impl MockConsumerChannel {
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

    async fn close(&self) -> TransportResult<()> {
        self.record(TransportOperation::CloseChannel);
        Ok(())
    }
}

#[async_trait]
impl ConsumerChannel for MockConsumerChannel {
    async fn set_qos(&self, prefetch: u16) -> TransportResult<()> {
        self.record(TransportOperation::Qos { prefetch });
        Ok(())
    }

    async fn consume(&self, request: ConsumerRequest) -> TransportResult<Box<dyn DeliveryStream>> {
        self.record(TransportOperation::Consume(request));
        Ok(Box::new(MockDeliveryStream {
            state: self.state.clone(),
        }))
    }

    async fn ack(&self, delivery_tag: u64, multiple: bool) -> TransportResult<()> {
        self.record(TransportOperation::Ack {
            delivery_tag,
            multiple,
        });
        Ok(())
    }

    async fn reject(&self, delivery_tag: u64, requeue: bool) -> TransportResult<()> {
        self.record(TransportOperation::Reject {
            delivery_tag,
            requeue,
        });
        Ok(())
    }
}

struct MockDeliveryStream {
    state: Arc<Mutex<MockState>>,
}

#[async_trait]
impl DeliveryStream for MockDeliveryStream {
    async fn next(&mut self) -> Option<TransportResult<Delivery>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deliveries
            .pop_front()
    }
}
