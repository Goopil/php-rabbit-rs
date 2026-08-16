use std::{num::NonZeroU32, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, TlsConfig,
        TopologyMode,
    },
    consumer::{
        APPLICATION_ATTEMPTS_HEADER, AttemptsErrorKind, AttemptsResolver, ConsumerSet, Headers,
        Subscription,
    },
    pool::ConnectionKey,
    publisher::{Destination, PublisherActor, PublisherConfig},
    topology::delay::DelayStrategy,
    transport::{
        Delivery as TransportDelivery, HeaderValue, PublishConfirmation, Transport,
        mock::{MockTransport, TransportOperation},
    },
};

fn headers(values: &[(&str, &str)]) -> Headers {
    values
        .iter()
        .map(|(name, value)| {
            (
                (*name).to_owned(),
                HeaderValue::Binary(Bytes::copy_from_slice(value.as_bytes())),
            )
        })
        .collect()
}

#[test]
fn first_acquisition_is_attempt_one() {
    let attempts = AttemptsResolver::default()
        .resolve(&Headers::new(), false)
        .expect("first acquisition");

    assert_eq!(attempts, 1);
}

#[test]
fn acquired_count_takes_precedence_over_redelivered_flag() {
    let attempts = AttemptsResolver::default()
        .resolve(&headers(&[("x-acquired-count", "7")]), true)
        .expect("RabbitMQ 4.3 acquired count");

    assert_eq!(attempts, 7);
}

#[test]
fn quorum_delivery_count_is_converted_from_failures_to_current_attempt() {
    let attempts = AttemptsResolver::default()
        .resolve(&headers(&[("x-delivery-count", "3")]), true)
        .expect("quorum delivery count");

    assert_eq!(attempts, 4);
}

#[test]
fn application_count_survives_a_fresh_broker_delivery() {
    let attempts = AttemptsResolver::default()
        .resolve(&headers(&[(APPLICATION_ATTEMPTS_HEADER, "5")]), false)
        .expect("application retry count");

    assert_eq!(attempts, 5);
}

#[test]
fn exceeding_the_configured_limit_is_a_typed_max_attempts_error() {
    let resolver = AttemptsResolver::new(NonZeroU32::new(3));

    let error = resolver
        .resolve(&headers(&[(APPLICATION_ATTEMPTS_HEADER, "4")]), false)
        .expect_err("fourth attempt exceeds a limit of three");

    assert_eq!(error.kind(), AttemptsErrorKind::MaxAttempts);
    assert_eq!(error.attempts(), 4);
    assert_eq!(error.max_attempts(), Some(3));
}

#[test]
fn default_attempt_limit_is_inclusive_at_twenty() {
    let resolver = AttemptsResolver::default();

    assert_eq!(
        resolver
            .resolve(&headers(&[(APPLICATION_ATTEMPTS_HEADER, "20")]), false)
            .expect("twentieth attempt is accepted"),
        20
    );
    let error = resolver
        .resolve(&headers(&[(APPLICATION_ATTEMPTS_HEADER, "21")]), false)
        .expect_err("twenty-first attempt exceeds the default limit");
    assert_eq!(error.kind(), AttemptsErrorKind::MaxAttempts);
    assert_eq!(error.max_attempts(), Some(20));
}

#[test]
fn classic_queue_without_counters_uses_the_documented_redelivery_fallback() {
    let resolver = AttemptsResolver::default();

    assert_eq!(
        resolver
            .resolve(&Headers::new(), false)
            .expect("fresh classic delivery"),
        1
    );
    assert_eq!(
        resolver
            .resolve(&Headers::new(), true)
            .expect("classic redelivery"),
        2
    );
}

fn broker() -> BrokerConfig {
    BrokerConfig {
        name: "default".to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

fn connection_key() -> ConnectionKey {
    ConnectionKey::from_config(
        &Config {
            brokers: vec![broker()],
            workers: vec![],
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
        }
        .validate()
        .expect("valid config"),
    )
}

#[tokio::test]
async fn broker_message_id_is_preserved_as_delivery_id() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(TransportDelivery {
        delivery_tag: 42,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        headers: Headers::new(),
        payload: Bytes::from_static(b"job"),
        message_id: Some("uuid-stable-job-id".to_owned()),
        correlation_id: Some("corr-1".to_owned()),
    }));
    let connection = transport.connect(&broker()).await.expect("connection");
    let consumer_channel = connection.open_consumer().await.expect("consumer channel");
    let subscription = Subscription::new(
        "jobs",
        connection_key(),
        "jobs",
        Arc::from(consumer_channel),
    );
    let consumer = ConsumerSet::spawn(vec![subscription], 1)
        .await
        .expect("consumer set");
    let delivery = consumer.next().await.expect("delivery");

    assert_eq!(
        delivery.id.as_str(),
        "uuid-stable-job-id",
        "delivery id must use the broker message_id, not a synthetic tag"
    );
}

#[tokio::test]
async fn missing_broker_message_id_falls_back_to_synthetic_id() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(TransportDelivery {
        delivery_tag: 42,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        headers: Headers::new(),
        payload: Bytes::from_static(b"job"),
        message_id: None,
        correlation_id: None,
    }));
    let connection = transport.connect(&broker()).await.expect("connection");
    let consumer_channel = connection.open_consumer().await.expect("consumer channel");
    let subscription = Subscription::new(
        "jobs",
        connection_key(),
        "jobs",
        Arc::from(consumer_channel),
    );
    let consumer = ConsumerSet::spawn(vec![subscription], 1)
        .await
        .expect("consumer set");
    let delivery = consumer.next().await.expect("delivery");

    assert!(
        !delivery.id.as_str().is_empty(),
        "delivery id must have a fallback synthetic id"
    );
    assert!(
        delivery.id.as_str().contains('1'),
        "synthetic id should contain the generation: {}",
        delivery.id.as_str()
    );
}

#[tokio::test]
async fn delayed_release_increments_the_application_attempt_header() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(TransportDelivery {
        delivery_tag: 8,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: headers(&[
            (APPLICATION_ATTEMPTS_HEADER, "2"),
            ("trace-id", "trace-42"),
            ("x-delivery-count", "1"),
        ]),
        payload: Bytes::from_static(b"job"),
    }));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let connection = transport.connect(&broker()).await.expect("connection");
    let consumer_channel = connection.open_consumer().await.expect("consumer channel");
    let publisher_channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");
    let publisher = PublisherActor::spawn(
        Arc::from(publisher_channel),
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            8,
            Duration::from_secs(5),
        ),
    );
    let subscription = Subscription::new(
        "jobs",
        connection_key(),
        "jobs",
        Arc::from(consumer_channel),
    )
    .delayed_publisher(publisher, Destination::new("jobs", "high"))
    .delay_strategy(DelayStrategy::Plugin);
    let consumer = ConsumerSet::spawn(vec![subscription], 1)
        .await
        .expect("consumer set");
    let delivery = consumer.next().await.expect("delivery");

    assert_eq!(delivery.attempts, 2);
    delivery
        .release(Duration::from_secs(5))
        .await
        .expect("delayed release");

    let published_request = transport
        .operations()
        .into_iter()
        .find_map(|operation| match operation {
            TransportOperation::Publish(request) => Some(request),
            _ => None,
        })
        .expect("republished message");
    assert_eq!(
        published_request
            .properties
            .headers
            .get(APPLICATION_ATTEMPTS_HEADER),
        Some(&HeaderValue::Integer(3))
    );
    assert_eq!(
        published_request.properties.headers.get("trace-id"),
        Some(&HeaderValue::Binary(Bytes::from_static(b"trace-42")))
    );
    assert!(
        !published_request
            .properties
            .headers
            .contains_key("x-delivery-count")
    );
}
