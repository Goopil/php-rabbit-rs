use async_trait::async_trait;
use bytes::Bytes;
use futures_lite::StreamExt;
use lapin::{
    BasicProperties, Channel, Confirmation, Connection, ConnectionProperties,
    options::{
        BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, BasicQosOptions,
        BasicRejectOptions, ConfirmSelectOptions, ExchangeDeclareOptions, QueueBindOptions,
        QueueDeclareOptions,
    },
    types::{AMQPValue, FieldTable},
};
use url::Url;

use super::{
    BindingSpec, ConsumerChannel, ConsumerRequest, Delivery, DeliveryStream, ExchangeKind,
    ExchangeSpec, PublishConfirmation, PublishReceipt, PublishRequest, PublisherChannel, QueueKind,
    QueueSpec, ReturnedMessage, TopologyChannel, Transport, TransportConnection, TransportError,
    TransportResult,
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
        let mut last_error = None;

        for endpoint in config.hosts() {
            let uri = connection_uri(config, endpoint)?;
            match Connection::connect(uri.as_str(), properties.clone()).await {
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

#[async_trait]
impl TransportConnection for LapinConnection {
    async fn open_publisher(&self) -> TransportResult<Box<dyn PublisherChannel>> {
        let channel = self.inner.create_channel().await.map_err(map_lapin_error)?;
        Ok(Box::new(LapinPublisherChannel { inner: channel }))
    }

    async fn open_consumer(&self) -> TransportResult<Box<dyn ConsumerChannel>> {
        let channel = self.inner.create_channel().await.map_err(map_lapin_error)?;
        Ok(Box::new(LapinConsumerChannel { inner: channel }))
    }

    async fn close(&self) -> TransportResult<()> {
        self.inner
            .close(200, "OK".into())
            .await
            .map_err(map_lapin_error)
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
        let confirmation = self
            .inner
            .basic_publish(
                request.exchange.clone().into(),
                request.routing_key.clone().into(),
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
                    no_ack: false,
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
                        payload: Bytes::from(delivery.data),
                    })
                },
            )
        })
    }
}

fn connection_uri(config: &BrokerConfig, endpoint: &Endpoint) -> TransportResult<Url> {
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
        .append_pair("heartbeat", &config.heartbeat.as_secs().to_string());

    Ok(uri)
}

async fn declare_exchange(
    channel: &Channel,
    spec: &ExchangeSpec,
    passive: bool,
) -> TransportResult<()> {
    channel
        .exchange_declare(
            spec.name.clone().into(),
            match spec.kind {
                ExchangeKind::Direct => lapin::ExchangeKind::Direct,
                ExchangeKind::Fanout => lapin::ExchangeKind::Fanout,
                ExchangeKind::Topic => lapin::ExchangeKind::Topic,
                ExchangeKind::Headers => lapin::ExchangeKind::Headers,
            },
            ExchangeDeclareOptions {
                passive,
                durable: spec.durable,
                auto_delete: spec.auto_delete,
                internal: spec.internal,
                nowait: false,
            },
            FieldTable::default(),
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
    if let Some(delay_ms) = request.properties.delay_ms {
        let mut headers = FieldTable::default();
        headers.insert(
            "x-delay".into(),
            AMQPValue::LongLongInt(i64::try_from(delay_ms).unwrap_or(i64::MAX)),
        );
        properties = properties.with_headers(headers);
    }
    properties
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

    use super::connection_uri;
    use crate::config::{BrokerConfig, Credentials, Endpoint, TlsConfig};

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
        assert_eq!(uri.query(), Some("heartbeat=30"));
    }
}
