use std::{pin::Pin, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use lapin::{
    BasicProperties, Channel, Confirmation, Connection, ConnectionProperties,
    options::{
        BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, BasicQosOptions,
        BasicRejectOptions, ConfirmSelectOptions, ExchangeDeclareOptions, QueueBindOptions,
        QueueDeclareOptions, QueuePurgeOptions,
    },
    tcp::OwnedTLSConfig,
    types::{AMQPValue, FieldArray, FieldTable},
};
use url::Url;

use super::{
    BindingSpec, ConsumerChannel, ConsumerRequest, Delivery, DeliveryStream, ExchangeKind,
    ExchangeSpec, HeaderValue, PublishConfirmation, PublishReceipt, PublishRequest,
    PublisherChannel, QueueKind, QueueSpec, ReturnedMessage, TopologyChannel, Transport,
    TransportConnection, TransportError, TransportResult,
};
use crate::config::{BrokerConfig, Endpoint};

/// Production AMQP 0-9-1 adapter. No Lapin type crosses this module boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct LapinTransport;

#[async_trait]
impl Transport for LapinTransport {
    async fn connect(
        &self,
        config: &BrokerConfig,
    ) -> TransportResult<Box<dyn TransportConnection>> {
        let properties = ConnectionProperties::default()
            .with_connection_name(format!("rabbit-rs:{}", config.name).into());
        let tls_config = build_tls_config(config)?;
        let mut last_error = None;

        for endpoint in config.hosts() {
            let uri = connection_uri(config, endpoint)?;
            let result = if config.tls.is_enabled() {
                Connection::connect_with_config(
                    uri.as_str(),
                    properties.clone(),
                    tls_config.clone(),
                    lapin::runtime::default_runtime()
                        .map_err(|error| TransportError::connection(error.to_string()))?,
                )
                .await
            } else {
                Connection::connect(uri.as_str(), properties.clone()).await
            };
            match result {
                Ok(connection) => return Ok(Box::new(LapinConnection { inner: connection })),
                Err(error) => last_error = Some(map_lapin_error(error)),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            TransportError::connection(format!(
                "broker '{}' has no endpoint to connect",
                config.name
            ))
        }))
    }
}

struct LapinConnection {
    inner: Connection,
}

/// Filters the lapin connection event stream down to connection errors.
///
/// With lapin's default `auto_recover: false`, every recoverable connection
/// failure (including heartbeat timeouts) is emitted as `Event::Error`.
struct LapinErrorStream {
    events: Pin<Box<dyn futures_util::Stream<Item = lapin::Event> + Send>>,
}

#[async_trait]
impl super::TransportErrorStream for LapinErrorStream {
    async fn next(&mut self) -> Option<TransportError> {
        loop {
            let event = self.events.as_mut().next().await?;
            if let lapin::Event::Error(error) = event {
                return Some(map_lapin_error(error));
            }
        }
    }
}

#[async_trait]
impl TransportConnection for LapinConnection {
    fn error_stream(&self) -> Box<dyn super::TransportErrorStream> {
        Box::new(LapinErrorStream {
            events: Box::pin(self.inner.events_listener()),
        })
    }

    async fn open_publisher(&self) -> TransportResult<Box<dyn PublisherChannel>> {
        let channel = self.inner.create_channel().await.map_err(map_lapin_error)?;
        Ok(Box::new(LapinPublisherChannel { inner: channel }))
    }

    async fn open_consumer(&self) -> TransportResult<Box<dyn ConsumerChannel>> {
        let channel = self.inner.create_channel().await.map_err(map_lapin_error)?;
        Ok(Box::new(LapinConsumerChannel { inner: channel }))
    }

    async fn close(&self) -> TransportResult<()> {
        match self.inner.close(200, "OK".into()).await {
            Ok(()) => Ok(()),
            // Closing an already-dead connection is trivially complete: the
            // socket is gone, so there is nothing left to shut down. Pool
            // shutdown must stay graceful after self-recovery replaced the
            // connection the pool still caches.
            Err(error)
                if matches!(
                    error.kind(),
                    lapin::ErrorKind::InvalidConnectionState(
                        lapin::ConnectionState::Error | lapin::ConnectionState::Closed
                    )
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(map_lapin_error(error)),
        }
    }
}

struct LapinPublisherChannel {
    inner: Channel,
}

#[async_trait]
impl TopologyChannel for LapinPublisherChannel {
    async fn declare_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        declare_exchange(&self.inner, spec, false).await
    }

    async fn verify_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        declare_exchange(&self.inner, spec, true).await
    }

    async fn declare_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        declare_queue(&self.inner, spec, false).await
    }

    async fn verify_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        declare_queue(&self.inner, spec, true).await
    }

    async fn bind_queue(&self, spec: &BindingSpec) -> TransportResult<()> {
        bind_queue(&self.inner, spec).await
    }

    async fn queue_size(&self, queue: &str) -> TransportResult<u32> {
        queue_size(&self.inner, queue).await
    }

    async fn purge_queue(&self, queue: &str) -> TransportResult<()> {
        self.inner
            .queue_purge(queue.to_owned().into(), QueuePurgeOptions::default())
            .await
            .map(|_| ())
            .map_err(map_lapin_error)
    }

    async fn close(&self) -> TransportResult<()> {
        close_channel(&self.inner).await
    }
}

#[async_trait]
impl PublisherChannel for LapinPublisherChannel {
    async fn enable_confirms(&self) -> TransportResult<()> {
        self.inner
            .confirm_select(ConfirmSelectOptions::default())
            .await
            .map_err(map_lapin_error)
    }

    async fn publish(&self, request: PublishRequest) -> TransportResult<Box<dyn PublishReceipt>> {
        let properties = publish_properties(&request);
        let exchange = request.exchange;
        let routing_key = request.routing_key;
        let confirmation = self
            .inner
            .basic_publish(
                exchange.as_ref().into(),
                routing_key.as_ref().into(),
                BasicPublishOptions {
                    mandatory: request.mandatory,
                    immediate: false,
                },
                &request.payload,
                properties,
            )
            .await
            .map_err(map_lapin_error)?;

        Ok(Box::new(LapinPublishReceipt {
            inner: confirmation,
        }))
    }
}

struct LapinPublishReceipt {
    inner: lapin::PublisherConfirm,
}

#[async_trait]
impl PublishReceipt for LapinPublishReceipt {
    async fn wait(self: Box<Self>) -> TransportResult<PublishConfirmation> {
        match self.inner.await.map_err(map_lapin_error)? {
            Confirmation::Ack(returned) => {
                Ok(PublishConfirmation::Ack(returned.map(map_returned_message)))
            }
            Confirmation::Nack(returned) => Ok(PublishConfirmation::Nack(
                returned.map(map_returned_message),
            )),
            Confirmation::NotRequested => Ok(PublishConfirmation::NotRequested),
        }
    }
}

struct LapinConsumerChannel {
    inner: Channel,
}

#[async_trait]
impl TopologyChannel for LapinConsumerChannel {
    async fn declare_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        declare_exchange(&self.inner, spec, false).await
    }

    async fn verify_exchange(&self, spec: &ExchangeSpec) -> TransportResult<()> {
        declare_exchange(&self.inner, spec, true).await
    }

    async fn declare_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        declare_queue(&self.inner, spec, false).await
    }

    async fn verify_queue(&self, spec: &QueueSpec) -> TransportResult<()> {
        declare_queue(&self.inner, spec, true).await
    }

    async fn bind_queue(&self, spec: &BindingSpec) -> TransportResult<()> {
        bind_queue(&self.inner, spec).await
    }

    async fn queue_size(&self, queue: &str) -> TransportResult<u32> {
        queue_size(&self.inner, queue).await
    }

    async fn purge_queue(&self, queue: &str) -> TransportResult<()> {
        self.inner
            .queue_purge(queue.to_owned().into(), QueuePurgeOptions::default())
            .await
            .map(|_| ())
            .map_err(map_lapin_error)
    }

    async fn close(&self) -> TransportResult<()> {
        close_channel(&self.inner).await
    }
}

#[async_trait]
impl ConsumerChannel for LapinConsumerChannel {
    async fn set_qos(&self, prefetch: u16) -> TransportResult<()> {
        self.inner
            .basic_qos(prefetch, BasicQosOptions { global: false })
            .await
            .map_err(map_lapin_error)
    }

    async fn consume(&self, request: ConsumerRequest) -> TransportResult<Box<dyn DeliveryStream>> {
        let consumer = self
            .inner
            .basic_consume(
                request.queue.into(),
                request.consumer_tag.into(),
                BasicConsumeOptions {
                    no_local: false,
                    no_ack: request.no_ack,
                    exclusive: request.exclusive,
                    nowait: false,
                },
                FieldTable::default(),
            )
            .await
            .map_err(map_lapin_error)?;

        Ok(Box::new(LapinDeliveryStream { inner: consumer }))
    }

    async fn ack(&self, delivery_tag: u64, multiple: bool) -> TransportResult<()> {
        self.inner
            .basic_ack(delivery_tag, BasicAckOptions { multiple })
            .await
            .map_err(map_lapin_error)
    }

    async fn reject(&self, delivery_tag: u64, requeue: bool) -> TransportResult<()> {
        self.inner
            .basic_reject(delivery_tag, BasicRejectOptions { requeue })
            .await
            .map_err(map_lapin_error)
    }
}

struct LapinDeliveryStream {
    inner: lapin::Consumer,
}

#[async_trait]
impl DeliveryStream for LapinDeliveryStream {
    async fn next(&mut self) -> Option<TransportResult<Delivery>> {
        self.inner.next().await.map(|result| {
            result.map_or_else(
                |error| Err(map_lapin_error(error)),
                |delivery| {
                    Ok(Delivery {
                        delivery_tag: delivery.delivery_tag,
                        exchange: delivery.exchange.to_string(),
                        routing_key: delivery.routing_key.to_string(),
                        redelivered: delivery.redelivered,
                        message_id: delivery
                            .properties
                            .message_id()
                            .as_ref()
                            .map(ToString::to_string),
                        correlation_id: delivery
                            .properties
                            .correlation_id()
                            .as_ref()
                            .map(ToString::to_string),
                        headers: Arc::new(map_headers(delivery.properties.headers().as_ref())),
                        payload: Bytes::from(delivery.data),
                    })
                },
            )
        })
    }
}

fn build_tls_config(config: &BrokerConfig) -> TransportResult<OwnedTLSConfig> {
    let tls = &config.tls;
    let identity = build_tls_identity(tls)?;
    let cert_chain = match tls.ca_cert() {
        Some(path) => Some(std::fs::read_to_string(path).map_err(|error| {
            TransportError::config(format!(
                "tls.ca_cert: cannot read '{}': {error}",
                path.display()
            ))
        })?),
        None => None,
    };

    Ok(OwnedTLSConfig {
        identity,
        cert_chain,
    })
}

fn build_tls_identity(
    tls: &crate::config::TlsConfig,
) -> TransportResult<Option<lapin::tcp::OwnedIdentity>> {
    let (Some(cert_path), Some(key_path)) = (tls.client_cert(), tls.client_key()) else {
        return Ok(None);
    };

    let pem = std::fs::read(cert_path).map_err(|error| {
        TransportError::config(format!(
            "tls.client_cert: cannot read '{}': {error}",
            cert_path.display()
        ))
    })?;
    let key = std::fs::read(key_path).map_err(|error| {
        TransportError::config(format!(
            "tls.client_key: cannot read '{}': {error}",
            key_path.display()
        ))
    })?;

    Ok(Some(lapin::tcp::OwnedIdentity::PKCS8 { pem, key }))
}

/// Builds the AMQP URI for the given broker endpoint.
///
/// The scheme is `amqps` when TLS is enabled, `amqp` otherwise.
///
/// # Errors
///
/// Returns a [`TransportError`] when the URI cannot be constructed (invalid host, port, username, password, or vhost).
pub fn connection_uri(config: &BrokerConfig, endpoint: &Endpoint) -> TransportResult<Url> {
    let scheme = if config.tls.is_enabled() {
        "amqps"
    } else {
        "amqp"
    };
    let mut uri = Url::parse(&format!("{scheme}://localhost"))
        .map_err(|error| TransportError::protocol(error.to_string()))?;
    uri.set_host(Some(endpoint.host()))
        .map_err(|error| TransportError::protocol(error.to_string()))?;
    uri.set_port(Some(endpoint.port()))
        .map_err(|()| TransportError::protocol("invalid broker port"))?;
    uri.set_username(config.credentials.username())
        .map_err(|()| TransportError::protocol("invalid broker username"))?;
    uri.set_password(Some(config.credentials.password()))
        .map_err(|()| TransportError::protocol("invalid broker password"))?;
    uri.path_segments_mut()
        .map_err(|()| TransportError::protocol("broker URI cannot contain a vhost"))?
        .clear()
        .push(&config.vhost);
    uri.query_pairs_mut()
        .append_pair("heartbeat", &config.heartbeat.as_secs().to_string())
        // Negotiate a 1 MB frame size (up from the 128 KB default) so larger
        // payloads can be sent in a single frame, reducing per-frame overhead.
        .append_pair("frame_max", "1048576");

    Ok(uri)
}

async fn declare_exchange(
    channel: &Channel,
    spec: &ExchangeSpec,
    passive: bool,
) -> TransportResult<()> {
    let mut arguments = FieldTable::default();
    if let ExchangeKind::Delayed(underlying) = &spec.kind {
        arguments.insert(
            "x-delayed-type".into(),
            AMQPValue::LongString(underlying.amqp_type_name().into()),
        );
    }
    for (name, value) in &spec.arguments {
        arguments.insert(name.clone().into(), publish_header_value(value));
    }

    let exchange_type = match &spec.kind {
        ExchangeKind::Direct => lapin::ExchangeKind::Direct,
        ExchangeKind::Fanout => lapin::ExchangeKind::Fanout,
        ExchangeKind::Topic => lapin::ExchangeKind::Topic,
        ExchangeKind::Headers => lapin::ExchangeKind::Headers,
        ExchangeKind::Delayed(_) => lapin::ExchangeKind::Custom("x-delayed-message".into()),
    };

    channel
        .exchange_declare(
            spec.name.clone().into(),
            exchange_type,
            ExchangeDeclareOptions {
                passive,
                durable: spec.durable,
                auto_delete: spec.auto_delete,
                internal: spec.internal,
                nowait: false,
            },
            arguments,
        )
        .await
        .map_err(map_lapin_error)
}

async fn declare_queue(channel: &Channel, spec: &QueueSpec, passive: bool) -> TransportResult<()> {
    let mut arguments = FieldTable::default();
    if spec.kind == QueueKind::Quorum {
        arguments.insert(
            "x-queue-type".into(),
            AMQPValue::LongString("quorum".into()),
        );
    }
    if let Some(exchange) = &spec.dead_letter_exchange {
        arguments.insert(
            "x-dead-letter-exchange".into(),
            AMQPValue::LongString(exchange.clone().into()),
        );
    }
    if let Some(routing_key) = &spec.dead_letter_routing_key {
        arguments.insert(
            "x-dead-letter-routing-key".into(),
            AMQPValue::LongString(routing_key.clone().into()),
        );
    }
    if let Some(message_ttl) = spec.message_ttl {
        arguments.insert(
            "x-message-ttl".into(),
            AMQPValue::LongUInt(duration_millis(message_ttl)),
        );
    }
    if let Some(expires) = spec.expires {
        arguments.insert(
            "x-expires".into(),
            AMQPValue::LongUInt(duration_millis(expires)),
        );
    }
    if let Some(delivery_limit) = spec.delivery_limit {
        arguments.insert(
            "x-delivery-limit".into(),
            AMQPValue::LongUInt(delivery_limit),
        );
    }
    for (name, value) in &spec.arguments {
        arguments.insert(name.clone().into(), publish_header_value(value));
    }

    channel
        .queue_declare(
            spec.name.clone().into(),
            QueueDeclareOptions {
                passive,
                durable: spec.durable,
                exclusive: spec.exclusive,
                auto_delete: spec.auto_delete,
                nowait: false,
            },
            arguments,
        )
        .await
        .map(|_| ())
        .map_err(map_lapin_error)
}

async fn queue_size(channel: &Channel, queue: &str) -> TransportResult<u32> {
    channel
        .queue_declare(
            queue.to_owned().into(),
            QueueDeclareOptions {
                passive: true,
                durable: false,
                exclusive: false,
                auto_delete: false,
                nowait: false,
            },
            FieldTable::default(),
        )
        .await
        .map(|queue| queue.message_count())
        .map_err(map_lapin_error)
}

fn duration_millis(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

async fn bind_queue(channel: &Channel, spec: &BindingSpec) -> TransportResult<()> {
    channel
        .queue_bind(
            spec.queue.clone().into(),
            spec.exchange.clone().into(),
            spec.routing_key.clone().into(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await
        .map_err(map_lapin_error)
}

async fn close_channel(channel: &Channel) -> TransportResult<()> {
    channel
        .close(200, "OK".into())
        .await
        .map_err(map_lapin_error)
}

fn publish_properties(request: &PublishRequest) -> BasicProperties {
    let mut properties = BasicProperties::default();
    if request.properties.persistent {
        properties = properties.with_delivery_mode(2);
    }
    if let Some(content_type) = &request.properties.content_type {
        properties = properties.with_content_type(content_type.clone().into());
    }
    if let Some(correlation_id) = &request.properties.correlation_id {
        properties = properties.with_correlation_id(correlation_id.clone().into());
    }
    if let Some(message_id) = &request.properties.message_id {
        properties = properties.with_message_id(message_id.clone().into());
    }
    let mut headers = FieldTable::default();
    for (name, value) in &request.properties.headers {
        headers.insert(name.clone().into(), publish_header_value(value));
    }
    if let Some(delay_ms) = request.properties.delay_ms {
        headers.insert(
            "x-delay".into(),
            AMQPValue::LongLongInt(i64::try_from(delay_ms).unwrap_or(i64::MAX)),
        );
    }
    if !request.properties.headers.is_empty() || request.properties.delay_ms.is_some() {
        properties = properties.with_headers(headers);
    }
    properties
}

fn publish_header_value(value: &HeaderValue) -> AMQPValue {
    match value {
        HeaderValue::Void => AMQPValue::Void,
        HeaderValue::Boolean(value) => AMQPValue::Boolean(*value),
        HeaderValue::Integer(value) => AMQPValue::LongLongInt(*value),
        HeaderValue::Double(value) => AMQPValue::Double(value.get()),
        HeaderValue::Binary(value) => AMQPValue::LongString(value.to_vec().into()),
        HeaderValue::Array(values) => AMQPValue::FieldArray(FieldArray::from(
            values.iter().map(publish_header_value).collect::<Vec<_>>(),
        )),
        HeaderValue::Table(values) => {
            let mut table = FieldTable::default();
            for (name, value) in values {
                table.insert(name.clone().into(), publish_header_value(value));
            }
            AMQPValue::FieldTable(table)
        }
    }
}

fn map_headers(headers: Option<&FieldTable>) -> super::Headers {
    headers.map_or_else(super::Headers::new, |headers| {
        headers
            .into_iter()
            .filter_map(|(name, value)| {
                map_header_value(value).map(|value| (name.to_string(), value))
            })
            .collect()
    })
}

fn map_header_value(value: &AMQPValue) -> Option<HeaderValue> {
    match value {
        AMQPValue::Boolean(value) => Some(HeaderValue::Boolean(*value)),
        AMQPValue::ShortShortInt(value) => Some(HeaderValue::Integer(i64::from(*value))),
        AMQPValue::ShortShortUInt(value) => Some(HeaderValue::Integer(i64::from(*value))),
        AMQPValue::ShortInt(value) => Some(HeaderValue::Integer(i64::from(*value))),
        AMQPValue::ShortUInt(value) => Some(HeaderValue::Integer(i64::from(*value))),
        AMQPValue::LongInt(value) => Some(HeaderValue::Integer(i64::from(*value))),
        AMQPValue::LongUInt(value) => Some(HeaderValue::Integer(i64::from(*value))),
        AMQPValue::LongLongInt(value) => Some(HeaderValue::Integer(*value)),
        AMQPValue::Float(value) => {
            super::HeaderFloat::new(f64::from(*value)).map(HeaderValue::Double)
        }
        AMQPValue::Double(value) => super::HeaderFloat::new(*value).map(HeaderValue::Double),
        AMQPValue::ShortString(value) => Some(HeaderValue::Binary(Bytes::copy_from_slice(
            value.as_str().as_bytes(),
        ))),
        AMQPValue::LongString(value) => Some(HeaderValue::Binary(Bytes::copy_from_slice(
            value.as_bytes(),
        ))),
        AMQPValue::Timestamp(value) => i64::try_from(*value).ok().map(HeaderValue::Integer),
        AMQPValue::FieldArray(values) => values
            .as_slice()
            .iter()
            .map(map_header_value)
            .collect::<Option<Vec<_>>>()
            .map(HeaderValue::Array),
        AMQPValue::FieldTable(values) => Some(HeaderValue::Table(map_headers(Some(values)))),
        AMQPValue::ByteArray(value) => Some(HeaderValue::Binary(Bytes::copy_from_slice(
            value.as_slice(),
        ))),
        AMQPValue::Void => Some(HeaderValue::Void),
        AMQPValue::DecimalValue(_) => None,
    }
}

fn map_returned_message(message: lapin::message::BasicReturnMessage) -> ReturnedMessage {
    let lapin::message::BasicReturnMessage {
        delivery,
        reply_code,
        reply_text,
    } = message;

    ReturnedMessage {
        reply_code,
        reply_text: reply_text.to_string(),
        exchange: delivery.exchange.to_string(),
        routing_key: delivery.routing_key.to_string(),
        payload: Bytes::from(delivery.data),
    }
}

fn map_lapin_error(error: lapin::Error) -> TransportError {
    let message = error.to_string();
    let authentication = matches!(error.kind(), lapin::ErrorKind::AuthProviderError(_))
        || matches!(
            error.kind(),
            lapin::ErrorKind::ProtocolError(protocol)
                if matches!(protocol.get_id(), 403 | 530)
        );
    let recoverable = error.can_be_recovered();
    drop(error);

    if authentication {
        TransportError::authentication(message)
    } else if recoverable {
        TransportError::connection(message)
    } else {
        TransportError::protocol(message)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use lapin::types::{AMQPValue, FieldTable};

    use super::{connection_uri, map_headers, publish_properties};
    use crate::config::{BrokerConfig, Credentials, Endpoint, TlsConfig};
    use crate::transport::{HeaderFloat, HeaderValue, PublishProperties, PublishRequest};

    #[test]
    fn uri_percent_encodes_credentials_and_vhost_as_segments() {
        let config = BrokerConfig {
            name: "primary".to_owned(),
            hosts: vec![Endpoint::new("rabbit.internal", 5671)],
            vhost: "tenant/one".to_owned(),
            credentials: Credentials::new("user@example.com", "p@ss/word"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        };

        let uri = connection_uri(&config, &config.hosts[0]).expect("valid URI");

        assert_eq!(uri.username(), "user%40example.com");
        assert_eq!(uri.password(), Some("p%40ss%2Fword"));
        assert_eq!(uri.path(), "/tenant%2Fone");
        assert_eq!(uri.query(), Some("heartbeat=30&frame_max=1048576"));
    }

    #[test]
    fn incoming_headers_preserve_scalar_amqp_types() {
        let mut table = FieldTable::default();
        table.insert("x-acquired-count".into(), AMQPValue::LongLongInt(7));
        table.insert("enabled".into(), AMQPValue::Boolean(true));
        table.insert("ratio".into(), AMQPValue::Double(1.5));
        table.insert(
            "name".into(),
            AMQPValue::LongString(b"worker".to_vec().into()),
        );
        table.insert("nothing".into(), AMQPValue::Void);

        let headers = map_headers(Some(&table));

        assert_eq!(
            headers.get("x-acquired-count"),
            Some(&HeaderValue::Integer(7))
        );
        assert_eq!(headers.get("enabled"), Some(&HeaderValue::Boolean(true)));
        assert_eq!(
            headers.get("ratio"),
            Some(&HeaderValue::Double(HeaderFloat::new(1.5).unwrap()))
        );
        assert_eq!(
            headers.get("name"),
            Some(&HeaderValue::Binary(Bytes::from_static(b"worker")))
        );
        assert_eq!(headers.get("nothing"), Some(&HeaderValue::Void));
    }

    #[test]
    fn outgoing_application_headers_are_merged_with_delay_header() {
        let mut request = PublishRequest {
            exchange: "jobs.delayed".into(),
            routing_key: "high".into(),
            payload: Bytes::from_static(b"job"),
            mandatory: true,
            properties: PublishProperties::default(),
        };
        request
            .properties
            .headers
            .insert("x-rabbit-rs-attempts".to_owned(), HeaderValue::Integer(3));
        request.properties.delay_ms = Some(5_000);

        let properties = publish_properties(&request);
        let headers = properties.headers().as_ref().expect("AMQP headers");

        assert!(matches!(
            headers.inner().get("x-rabbit-rs-attempts"),
            Some(AMQPValue::LongLongInt(3))
        ));
        assert_eq!(
            headers.inner().get("x-delay"),
            Some(&AMQPValue::LongLongInt(5_000))
        );
    }

    #[test]
    fn outgoing_headers_preserve_scalar_amqp_types() {
        let mut request = PublishRequest {
            exchange: "jobs".into(),
            routing_key: "default".into(),
            payload: Bytes::from_static(b"job"),
            mandatory: true,
            properties: PublishProperties::default(),
        };
        request
            .properties
            .headers
            .insert("nothing".to_owned(), HeaderValue::Void);
        request
            .properties
            .headers
            .insert("enabled".to_owned(), HeaderValue::Boolean(true));
        request
            .properties
            .headers
            .insert("count".to_owned(), HeaderValue::Integer(42));
        request.properties.headers.insert(
            "ratio".to_owned(),
            HeaderValue::Double(HeaderFloat::new(1.5).unwrap()),
        );
        request.properties.headers.insert(
            "name".to_owned(),
            HeaderValue::Binary(Bytes::from_static(b"worker")),
        );

        let properties = publish_properties(&request);
        let headers = properties.headers().as_ref().expect("AMQP headers");

        assert_eq!(headers.inner().get("nothing"), Some(&AMQPValue::Void));
        assert_eq!(
            headers.inner().get("enabled"),
            Some(&AMQPValue::Boolean(true))
        );
        assert_eq!(
            headers.inner().get("count"),
            Some(&AMQPValue::LongLongInt(42))
        );
        assert_eq!(headers.inner().get("ratio"), Some(&AMQPValue::Double(1.5)));
        assert!(matches!(
            headers.inner().get("name"),
            Some(AMQPValue::LongString(value)) if value.as_bytes() == b"worker"
        ));
    }
}
