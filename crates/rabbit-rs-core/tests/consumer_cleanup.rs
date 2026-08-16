use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, TlsConfig,
        TopologyMode,
    },
    consumer::{ConsumerErrorKind, ConsumerSet, Subscription, SubscriptionPolicy},
    pool::ConnectionKey,
    transport::{
        Delivery as TransportDelivery, Transport,
        mock::{MockTransport, TransportOperation},
    },
};

fn broker(name: &str, vhost: &str) -> BrokerConfig {
    BrokerConfig {
        name: name.to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: vhost.to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

fn connection_key(name: &str, vhost: &str) -> ConnectionKey {
    let config = Config {
        brokers: vec![broker(name, vhost)],
        workers: vec![],
        topology_mode: TopologyMode::External,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
    }
    .validate()
    .expect("valid config");
    ConnectionKey::from_config(&config)
}

fn delivery(tag: u64, payload: &'static [u8]) -> TransportDelivery {
    TransportDelivery {
        delivery_tag: tag,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: BTreeMap::new(),
        payload: Bytes::from_static(payload),
    }
}

async fn subscription(
    transport: &MockTransport,
    id: &str,
    key: ConnectionKey,
    prefetch: u16,
    priority: i16,
) -> Subscription {
    let channel = transport
        .connect(&broker(id, "/"))
        .await
        .expect("connection")
        .open_consumer()
        .await
        .expect("consumer channel");
    Subscription::new(id, key, format!("queue.{id}"), Arc::from(channel))
        .prefetch(prefetch)
        .channel_id(prefetch)
        .policy(SubscriptionPolicy::new(1, priority, Duration::from_secs(1)))
}

async fn let_actor_process() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

fn close_channel_count(transport: &MockTransport) -> usize {
    transport
        .operations()
        .iter()
        .filter(|op| matches!(op, TransportOperation::CloseChannel))
        .count()
}

#[tokio::test]
async fn drop_closes_subscription_channels_without_explicit_close() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        1,
        "dropping ConsumerHandle must close all subscription channels"
    );
}

#[tokio::test]
async fn drop_closes_channels_for_multiple_subscriptions() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(
        vec![
            subscription(&transport, "first", connection_key("first", "/"), 4, 0).await,
            subscription(&transport, "second", connection_key("second", "/"), 4, 0).await,
        ],
        2,
    )
    .await
    .expect("consumer set");

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        2,
        "dropping ConsumerHandle must close every subscription channel"
    );
}

#[tokio::test]
async fn drop_does_not_double_close_when_close_was_already_called() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    consumer.close().await.expect("explicit close");
    let closes_after_close = close_channel_count(&transport);

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        closes_after_close,
        "Drop must not send a second Close after explicit close()"
    );
}

#[tokio::test]
async fn drop_sends_close_only_once_across_clones() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let clone = consumer.clone();
    drop(consumer);
    let_actor_process().await;
    let closes_after_first_drop = close_channel_count(&transport);

    drop(clone);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        closes_after_first_drop,
        "Drop must not close channels twice when the last clone is dropped"
    );
}

#[tokio::test(start_paused = true)]
async fn next_after_drop_returns_typed_error_not_panic() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let clone = consumer.clone();
    drop(consumer);
    let_actor_process().await;

    let result = tokio::time::timeout(Duration::from_millis(100), clone.next()).await;
    match result {
        Ok(Err(error)) => assert_eq!(
            error.kind(),
            ConsumerErrorKind::Closed,
            "next() after drop must return a Closed error"
        ),
        Ok(Ok(_)) => panic!("next() must not succeed after drop"),
        Err(elapsed) => panic!(
            "next() must not hang after drop — it should return a typed error (elapsed: {elapsed:?})"
        ),
    }
}

#[tokio::test]
async fn drop_with_pending_delivery_still_closes_channels() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    let_actor_process().await;
    let _item = consumer.next().await.expect("delivery");

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        1,
        "channels must be closed even with an in-flight delivery"
    );
}
