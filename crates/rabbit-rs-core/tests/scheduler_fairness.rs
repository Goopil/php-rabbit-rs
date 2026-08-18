use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use rabbit_rs_core::consumer::{
    Scheduler, SubscriptionId, SubscriptionPolicy, WeightedFairScheduler,
};

fn id(value: &str) -> SubscriptionId {
    SubscriptionId::new(value)
}

fn policy(weight: u16, priority_class: i16) -> SubscriptionPolicy {
    SubscriptionPolicy::new(weight, priority_class, Duration::from_secs(1))
}

#[test]
fn selects_the_only_ready_subscription() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(id("only"), policy(1, 0));
    scheduler.mark_ready(&id("only"));

    assert_eq!(scheduler.next(now), Some(id("only")));
}

#[test]
fn follows_configured_weight_ratio() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(id("high-weight"), policy(8, 0));
    scheduler.register(id("low-weight"), policy(2, 0));
    scheduler.mark_ready(&id("high-weight"));
    scheduler.mark_ready(&id("low-weight"));

    let mut counts = BTreeMap::new();
    for _ in 0..10_000 {
        *counts.entry(scheduler.next(now).unwrap()).or_insert(0_u32) += 1;
    }

    assert_eq!(counts[&id("high-weight")], 8_000);
    assert_eq!(counts[&id("low-weight")], 2_000);
}

#[test]
fn empty_subscription_does_not_accumulate_credit() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(id("temporarily-empty"), policy(1, 0));
    scheduler.register(id("always-ready"), policy(1, 0));
    scheduler.mark_ready(&id("temporarily-empty"));
    scheduler.mark_ready(&id("always-ready"));

    let first = scheduler.next(now).unwrap();
    scheduler.mark_empty(&id("temporarily-empty"));
    for _ in 0..100 {
        assert_eq!(scheduler.next(now), Some(id("always-ready")));
    }

    scheduler.mark_ready(&id("temporarily-empty"));
    let resumed = [scheduler.next(now).unwrap(), scheduler.next(now).unwrap()];

    assert!(resumed.contains(&id("temporarily-empty")));
    assert!(resumed.contains(&id("always-ready")));
    assert!(first == id("temporarily-empty") || first == id("always-ready"));
}

#[test]
fn subscription_can_return_after_being_empty() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(id("queue"), policy(1, 0));
    scheduler.mark_ready(&id("queue"));
    scheduler.mark_empty(&id("queue"));

    assert_eq!(scheduler.next(now), None);

    scheduler.mark_ready(&id("queue"));

    assert_eq!(scheduler.next(now), Some(id("queue")));
}

#[test]
fn aging_prevents_lower_priority_starvation() {
    let start = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(id("high"), policy(1, 3));
    scheduler.register(id("low"), policy(1, 0));
    scheduler.mark_ready(&id("high"));
    scheduler.mark_ready(&id("low"));

    let selected = (0..=6)
        .map(|seconds| {
            scheduler
                .next(start + Duration::from_secs(seconds))
                .unwrap()
        })
        .collect::<Vec<_>>();

    assert_eq!(selected[0], id("high"));
    assert!(selected.contains(&id("low")));
}

#[test]
fn produces_a_deterministic_sequence() {
    let start = Instant::now();
    let mut first = WeightedFairScheduler::default();
    let mut second = WeightedFairScheduler::default();

    for scheduler in [&mut first, &mut second] {
        scheduler.register(id("a"), policy(5, 0));
        scheduler.register(id("b"), policy(3, 0));
        scheduler.register(id("c"), policy(1, 0));
        scheduler.mark_ready(&id("a"));
        scheduler.mark_ready(&id("b"));
        scheduler.mark_ready(&id("c"));
    }

    let first_sequence = (0..100)
        .map(|tick| first.next(start + Duration::from_millis(tick)).unwrap())
        .collect::<Vec<_>>();
    let second_sequence = (0..100)
        .map(|tick| second.next(start + Duration::from_millis(tick)).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(first_sequence, second_sequence);
}
