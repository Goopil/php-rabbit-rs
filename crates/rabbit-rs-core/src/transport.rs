use std::{collections::BTreeMap, error::Error, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use serde::Deserialize;

use crate::config::BrokerConfig;

pub mod lapin;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;

pub type TransportResult<T> = Result<T, TransportError>;
pub type Headers = BTreeMap<String, HeaderValue>;
pub type PublishHeaders = Headers;

/// A PHP-compatible value that can be represented in an AMQP field table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeaderValue {
    Void,
    Boolean(bool),
    Integer(i64),
    Double(HeaderFloat),
    Binary(Bytes),
    Array(Vec<Self>),
    Table(PublishHeaders),
}

/// A finite AMQP double stored with deterministic equality semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderFloat(u64);

impl HeaderFloat {
    /// Creates a value when the input is finite.
    #[must_use]
    pub fn new(value: f64) -> Option<Self> {
        value.is_finite().then(|| Self(value.to_bits()))
    }

    /// Returns the represented floating-point value.
    #[must_use]
    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

impl From<Bytes> for HeaderValue {
    fn from(value: Bytes) -> Self {
        Self::Binary(value)
    }
}

/// Stability-oriented classification used by connection recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    Authentication,
    Configuration,
    Connection,
    Protocol,
    Closed,
}

/// Transport failure without exposing a concrete AMQP client error type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError {
    kind: TransportErrorKind,
    message: String,
}

impl TransportError {
    #[must_use]
    pub fn authentication(message: impl Into<String>) -> Self {
        Self::new(TransportErrorKind::Authentication, message)
    }

    /// Configuration failure that retries cannot fix (e.g. unreadable TLS file).
    #[must_use]
    pub fn config(message: impl Into<String>) -> Self {
        Self::new(TransportErrorKind::Configuration, message)
    }

    #[must_use]
    pub fn connection(message: impl Into<String>) -> Self {
        Self::new(TransportErrorKind::Connection, message)
    }

    #[must_use]
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(TransportErrorKind::Protocol, message)
    }

    #[must_use]
    pub fn closed(message: impl Into<String>) -> Self {
        Self::new(TransportErrorKind::Closed, message)
    }

    #[must_use]
    pub const fn kind(&self) -> TransportErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(
            self.kind,
            TransportErrorKind::Connection | TransportErrorKind::Closed
        )
    }

    fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for TransportError {}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum QueueKind {
    Classic,
    #[default]
    Quorum,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSpec {
    pub name: String,
    pub durable: bool,
    pub exclusive: bool,
    pub auto_delete: bool,
    pub kind: QueueKind,
    pub dead_letter_exchange: Option<String>,
    pub dead_letter_routing_key: Option<String>,
    pub message_ttl: Option<Duration>,
    pub expires: Option<Duration>,
    pub delivery_limit: Option<u32>,
    pub arguments: Headers,
}

impl QueueSpec {
    #[must_use]
    pub fn quorum(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            durable: true,
            exclusive: false,
            auto_delete: false,
            kind: QueueKind::Quorum,
            dead_letter_exchange: None,
            dead_letter_routing_key: None,
            message_ttl: None,
            expires: None,
            delivery_limit: None,
            arguments: Headers::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ExchangeKind {
    #[default]
    Direct,
    Fanout,
    Topic,
    Headers,
    /// `x-delayed-message` exchange backed by the `RabbitMQ` delayed-message plugin.
    ///
    /// The inner kind specifies the underlying exchange type used by the plugin
    /// (direct, topic, etc.) and is emitted as the `x-delayed-type` argument.
    Delayed(Box<ExchangeKind>),
}

impl ExchangeKind {
    /// Returns the AMQP exchange type string used when declaring this exchange.
    #[must_use]
    pub fn amqp_type_name(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Fanout => "fanout",
            Self::Topic => "topic",
            Self::Headers => "headers",
            Self::Delayed(_) => "x-delayed-message",
        }
    }

    /// Returns the underlying exchange type for delayed exchanges, or `self` otherwise.
    #[must_use]
    pub fn underlying(&self) -> &Self {
        match self {
            Self::Delayed(inner) => inner,
            other => other,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeSpec {
    pub name: String,
    pub kind: ExchangeKind,
    pub durable: bool,
    pub auto_delete: bool,
    pub internal: bool,
    pub arguments: Headers,
}

impl ExchangeSpec {
    #[must_use]
    pub fn new(name: impl Into<String>, kind: ExchangeKind) -> Self {
        Self {
            name: name.into(),
            kind,
            durable: true,
            auto_delete: false,
            internal: false,
            arguments: Headers::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingSpec {
    pub queue: String,
    pub exchange: String,
    pub routing_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishProperties {
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub message_id: Option<String>,
    pub delay_ms: Option<u64>,
    pub headers: PublishHeaders,
    pub persistent: bool,
}

impl Default for PublishProperties {
    fn default() -> Self {
        Self {
            content_type: None,
            correlation_id: None,
            message_id: None,
            delay_ms: None,
            headers: PublishHeaders::new(),
            persistent: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishRequest {
    pub exchange: Arc<str>,
    pub routing_key: Arc<str>,
    pub payload: Bytes,
    pub mandatory: bool,
    pub properties: PublishProperties,
}

impl PublishRequest {
    #[must_use]
    pub fn new(
        exchange: impl Into<Arc<str>>,
        routing_key: impl Into<Arc<str>>,
        payload: impl Into<Bytes>,
    ) -> Self {
        Self {
            exchange: exchange.into(),
            routing_key: routing_key.into(),
            payload: payload.into(),
            mandatory: true,
            properties: PublishProperties::default(),
        }
    }

    #[must_use]
    pub const fn mandatory(mut self, mandatory: bool) -> Self {
        self.mandatory = mandatory;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReturnedMessage {
    pub reply_code: u16,
    pub reply_text: String,
    pub exchange: String,
    pub routing_key: String,
    pub payload: Bytes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishConfirmation {
    Ack(Option<ReturnedMessage>),
    Nack(Option<ReturnedMessage>),
    NotRequested,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerRequest {
    pub queue: String,
    pub consumer_tag: String,
    pub exclusive: bool,
    pub no_ack: bool,
}

impl ConsumerRequest {
    #[must_use]
    pub fn new(queue: impl Into<String>, consumer_tag: impl Into<String>) -> Self {
        Self {
            queue: queue.into(),
            consumer_tag: consumer_tag.into(),
            exclusive: false,
            no_ack: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub delivery_tag: u64,
    pub exchange: String,
    pub routing_key: String,
    pub redelivered: bool,
    pub message_id: Option<String>,
    pub correlation_id: Option<String>,
    pub headers: Arc<Headers>,
    pub payload: Bytes,
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// # Errors
    ///
    /// Returns a classified error when no broker endpoint can be connected.
    async fn connect(&self, config: &BrokerConfig)
    -> TransportResult<Box<dyn TransportConnection>>;
}

#[async_trait]
pub trait TransportConnection: Send + Sync {
    /// # Errors
    ///
    /// Returns an error when the broker cannot allocate a publisher channel.
    async fn open_publisher(&self) -> TransportResult<Box<dyn PublisherChannel>>;

    /// # Errors
    ///
    /// Returns an error when the broker cannot allocate a consumer channel.
    async fn open_consumer(&self) -> TransportResult<Box<dyn ConsumerChannel>>;

    /// # Errors
    ///
    /// Returns an error when graceful connection shutdown fails.
    async fn close(&self) -> TransportResult<()>;
}

#[async_trait]
pub trait TopologyChannel: Send + Sync {
    /// # Errors
    ///
    /// Returns an error when the exchange cannot be declared.
    async fn declare_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()>;

    /// # Errors
    ///
    /// Returns an error when the exchange is absent or incompatible.
    async fn verify_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()>;

    /// # Errors
    ///
    /// Returns an error when the queue cannot be declared.
    async fn declare_queue(&self, spec: &QueueSpec) -> TransportResult<()>;

    /// # Errors
    ///
    /// Returns an error when the queue is absent or incompatible.
    async fn verify_queue(&self, spec: &QueueSpec) -> TransportResult<()>;

    /// # Errors
    ///
    /// Returns an error when the queue binding cannot be declared.
    async fn bind_queue(&self, spec: &BindingSpec) -> TransportResult<()>;

    /// Returns the number of pending messages in the queue.
    ///
    /// Uses a passive `queue.declare` so the queue is not created if absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue does not exist or the broker rejects the request.
    async fn queue_size(&self, queue: &str) -> TransportResult<u32>;

    /// Removes all messages from the queue.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue does not exist or the broker rejects the purge.
    async fn purge_queue(&self, queue: &str) -> TransportResult<()>;

    /// # Errors
    ///
    /// Returns an error when graceful channel shutdown fails.
    async fn close(&self) -> TransportResult<()>;
}

#[async_trait]
pub trait PublisherChannel: TopologyChannel {
    /// # Errors
    ///
    /// Returns an error when confirm mode cannot be enabled.
    async fn enable_confirms(&self) -> TransportResult<()>;

    /// Enqueues a publish and returns a separately awaitable confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error when the publish cannot be written to the channel.
    async fn publish(&self, request: PublishRequest) -> TransportResult<Box<dyn PublishReceipt>>;

    /// Sends a batch of publishes, returning one receipt per request in order.
    ///
    /// The default implementation calls [`publish`](Self::publish) sequentially.
    /// Implementations may override this to pipeline frames and reduce per-message
    /// async overhead.
    ///
    /// # Errors
    ///
    /// Returns an error when any publish cannot be written to the channel.
    async fn publish_batch(
        &self,
        requests: Vec<PublishRequest>,
    ) -> TransportResult<Vec<Box<dyn PublishReceipt>>> {
        let mut receipts = Vec::with_capacity(requests.len());
        for request in requests {
            receipts.push(self.publish(request).await?);
        }
        Ok(receipts)
    }
}

#[async_trait]
pub trait PublishReceipt: Send {
    /// # Errors
    ///
    /// Returns an error when the broker confirmation cannot be obtained.
    async fn wait(self: Box<Self>) -> TransportResult<PublishConfirmation>;
}

#[async_trait]
pub trait ConsumerChannel: TopologyChannel {
    /// # Errors
    ///
    /// Returns an error when `QoS` cannot be applied to the channel.
    async fn set_qos(&self, prefetch: u16) -> TransportResult<()>;

    /// # Errors
    ///
    /// Returns an error when the consumer cannot be registered.
    async fn consume(&self, request: ConsumerRequest) -> TransportResult<Box<dyn DeliveryStream>>;

    /// # Errors
    ///
    /// Returns an error when the acknowledgement cannot be sent.
    async fn ack(&self, delivery_tag: u64, multiple: bool) -> TransportResult<()>;

    /// # Errors
    ///
    /// Returns an error when the rejection cannot be sent.
    async fn reject(&self, delivery_tag: u64, requeue: bool) -> TransportResult<()>;
}

#[async_trait]
pub trait DeliveryStream: Send {
    async fn next(&mut self) -> Option<TransportResult<Delivery>>;
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use bytes::Bytes;

    use super::{
        ConsumerRequest, Delivery, PublishConfirmation, PublishRequest, QueueSpec, Transport,
        TransportError, TransportErrorKind,
        lapin::LapinTransport,
        mock::{MockTransport, TransportOperation},
    };
    use crate::config::{BrokerConfig, Credentials, Endpoint, TlsConfig};

    fn broker() -> BrokerConfig {
        BrokerConfig {
            name: "primary".to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "guest"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    #[test]
    fn publish_requests_are_mandatory_by_default() {
        assert!(PublishRequest::new("jobs", "default", b"payload".to_vec()).mandatory);
    }

    #[test]
    fn publish_request_accepts_arc_str() {
        let req = PublishRequest::new(
            Arc::<str>::from("test_exchange"),
            Arc::<str>::from("test.key"),
            Bytes::from_static(b"payload"),
        );
        assert_eq!(req.exchange.as_ref(), "test_exchange");
        assert_eq!(req.routing_key.as_ref(), "test.key");
    }

    #[tokio::test]
    async fn mock_transport_records_a_pipelined_publish() {
        let transport = MockTransport::default();
        transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));

        let connection = transport.connect(&broker()).await.expect("connection");
        let publisher = connection.open_publisher().await.expect("publisher");
        publisher
            .declare_queue(&QueueSpec::quorum("jobs"))
            .await
            .expect("queue declaration");
        publisher
            .enable_confirms()
            .await
            .expect("publisher confirms");
        let receipt = publisher
            .publish(PublishRequest::new("", "jobs", b"payload".to_vec()).mandatory(true))
            .await
            .expect("publish");

        assert_eq!(
            transport.operations(),
            vec![
                TransportOperation::Connect {
                    broker: "primary".to_owned(),
                },
                TransportOperation::OpenPublisher,
                TransportOperation::DeclareQueue(QueueSpec::quorum("jobs")),
                TransportOperation::EnableConfirms,
                TransportOperation::Publish(
                    PublishRequest::new("", "jobs", b"payload".to_vec()).mandatory(true)
                ),
            ]
        );
        assert_eq!(
            receipt.wait().await.expect("confirmation"),
            PublishConfirmation::Ack(None)
        );
    }

    #[tokio::test]
    async fn mock_transport_scripts_connection_failures() {
        let transport = MockTransport::default();
        transport.push_connect_result(Err(TransportError::authentication("access refused")));

        let result = transport.connect(&broker()).await;
        let Err(error) = result else {
            panic!("the scripted connection should fail");
        };

        assert_eq!(error.kind(), TransportErrorKind::Authentication);
        assert!(!error.is_recoverable());
    }

    #[tokio::test]
    async fn mock_consumer_supports_qos_delivery_ack_and_reject() {
        let transport = MockTransport::default();
        transport.push_delivery(Ok(Delivery {
            delivery_tag: 42,
            exchange: "events".to_owned(),
            routing_key: "jobs".to_owned(),
            redelivered: false,
            message_id: None,
            correlation_id: None,
            headers: Arc::new(super::Headers::new()),
            payload: Bytes::from_static(b"job"),
        }));

        let connection = transport.connect(&broker()).await.expect("connection");
        let consumer = connection.open_consumer().await.expect("consumer");
        consumer.set_qos(64).await.expect("qos");
        let mut deliveries = consumer
            .consume(ConsumerRequest::new("jobs", "worker-1"))
            .await
            .expect("consumer stream");
        let delivery = deliveries
            .next()
            .await
            .expect("scripted delivery")
            .expect("valid delivery");
        consumer
            .ack(delivery.delivery_tag, false)
            .await
            .expect("ack");
        consumer
            .reject(delivery.delivery_tag + 1, true)
            .await
            .expect("reject");

        assert!(
            transport
                .operations()
                .contains(&TransportOperation::Qos { prefetch: 64 })
        );
        assert!(transport.operations().contains(&TransportOperation::Ack {
            delivery_tag: 42,
            multiple: false,
        }));
        assert!(
            transport
                .operations()
                .contains(&TransportOperation::Reject {
                    delivery_tag: 43,
                    requeue: true,
                })
        );
    }

    #[test]
    fn lapin_adapter_implements_the_transport_boundary() {
        fn assert_transport<T: Transport>() {}

        assert_transport::<LapinTransport>();
    }
}
