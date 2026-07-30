use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
    pool::connection_actor::ConnectionActor,
    recovery::{Clock, ConnectionState, IdentityJitter, JitterSource, RecoveryPolicy},
    transport::{TransportError, TransportErrorKind, mock::MockTransport},
};
use tokio::sync::watch;

#[derive(Clone, Default)]
struct RecordingClock {
    delays: Arc<Mutex<Vec<Duration>>>,
}

impl RecordingClock {
    fn delays(&self) -> Vec<Duration> {
        self.delays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Clock for RecordingClock {
    async fn sleep(&self, duration: Duration) {
        self.delays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(duration);
        tokio::time::sleep(duration).await;
    }
}

struct AdditiveJitter(Duration);

impl JitterSource for AdditiveJitter {
    fn apply(&self, delay: Duration) -> Duration {
        delay.saturating_add(self.0)
    }
}

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

async fn wait_for(
    receiver: &mut watch::Receiver<ConnectionState>,
    predicate: impl Fn(&ConnectionState) -> bool,
) -> ConnectionState {
    loop {
        let current = receiver.borrow().clone();
        if predicate(&current) {
            return current;
        }
        receiver.changed().await.expect("connection actor alive");
    }
}

#[tokio::test(start_paused = true)]
async fn transitions_from_disconnected_through_connecting_to_ready() {
    let transport = Arc::new(MockTransport::default());
    let actor = ConnectionActor::spawn(transport, broker(), RecoveryPolicy::default());
    let mut states = actor.subscribe();

    assert_eq!(*states.borrow(), ConnectionState::Disconnected);
    actor.start().await.expect("start command");
    wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Connecting { attempt: 1 })
    })
    .await;
    let ready = wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Ready { .. })
    })
    .await;

    assert_eq!(ready, ConnectionState::Ready { generation: 1 });
}

#[tokio::test(start_paused = true)]
async fn retries_with_100_200_and_400_millisecond_backoff() {
    let transport = Arc::new(MockTransport::default());
    for attempt in 1..=3 {
        transport.push_connect_result(Err(TransportError::connection(format!(
            "failure {attempt}"
        ))));
    }
    let clock = Arc::new(RecordingClock::default());
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        clock.clone(),
        Arc::new(IdentityJitter),
    );
    let mut states = actor.subscribe();

    actor.start().await.expect("start command");
    wait_for(&mut states, |state| {
        matches!(
            state,
            ConnectionState::Recovering {
                retry_in,
                ..
            } if *retry_in == Duration::from_millis(100)
        )
    })
    .await;
    assert_eq!(clock.delays(), vec![Duration::from_millis(100)]);

    tokio::time::advance(Duration::from_millis(100)).await;
    wait_for(&mut states, |state| {
        matches!(
            state,
            ConnectionState::Recovering {
                retry_in,
                ..
            } if *retry_in == Duration::from_millis(200)
        )
    })
    .await;

    tokio::time::advance(Duration::from_millis(200)).await;
    wait_for(&mut states, |state| {
        matches!(
            state,
            ConnectionState::Recovering {
                retry_in,
                ..
            } if *retry_in == Duration::from_millis(400)
        )
    })
    .await;

    assert_eq!(
        clock.delays(),
        vec![
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
        ]
    );
}

#[test]
fn exponential_backoff_is_capped_at_30_seconds() {
    let policy = RecoveryPolicy::default();

    assert_eq!(policy.delay_for_failure(20), Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn injected_jitter_controls_the_observed_retry_delay() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::connection("offline")));
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(AdditiveJitter(Duration::from_millis(25))),
    );
    let mut states = actor.subscribe();

    actor.start().await.expect("start command");
    let recovering = wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;

    assert!(matches!(
        recovering,
        ConnectionState::Recovering { retry_in, .. }
            if retry_in == Duration::from_millis(125)
    ));
}

#[tokio::test(start_paused = true)]
async fn authentication_failure_is_permanent() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::authentication("access refused")));
    let actor = ConnectionActor::spawn(transport, broker(), RecoveryPolicy::default());
    let mut states = actor.subscribe();

    actor.start().await.expect("start command");
    let failed = wait_for(&mut states, |state| {
        matches!(state, ConnectionState::FailedPermanent { .. })
    })
    .await;

    assert!(matches!(
        failed,
        ConnectionState::FailedPermanent {
            kind: TransportErrorKind::Authentication,
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn ready_connection_loss_enters_recovery() {
    let transport = Arc::new(MockTransport::default());
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(IdentityJitter),
    );
    let mut states = actor.subscribe();
    actor.start().await.expect("start command");
    wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 1 })
    })
    .await;

    actor
        .connection_lost(TransportError::connection("heartbeat missed"))
        .await
        .expect("loss command");
    let recovering = wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;

    assert!(matches!(
        recovering,
        ConnectionState::Recovering {
            attempt: 1,
            retry_in,
            ..
        } if retry_in == Duration::from_millis(100)
    ));
}

#[tokio::test(start_paused = true)]
async fn close_interrupts_an_active_backoff() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::connection("offline")));
    let actor = ConnectionActor::spawn(transport, broker(), RecoveryPolicy::default());
    let mut states = actor.subscribe();
    actor.start().await.expect("start command");
    wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;

    actor.close().await.expect("close during backoff");

    assert_eq!(*states.borrow(), ConnectionState::Closed);
}

#[tokio::test(start_paused = true)]
async fn generation_increments_after_successful_recovery() {
    let transport = Arc::new(MockTransport::default());
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(IdentityJitter),
    );
    let mut states = actor.subscribe();
    actor.start().await.expect("start command");
    wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 1 })
    })
    .await;

    actor
        .connection_lost(TransportError::connection("socket reset"))
        .await
        .expect("loss command");
    wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;
    tokio::time::advance(Duration::from_millis(100)).await;
    let ready = wait_for(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 2 })
    })
    .await;

    assert_eq!(ready, ConnectionState::Ready { generation: 2 });
}
