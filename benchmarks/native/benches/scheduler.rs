use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rabbit_rs_core::consumer::{
    Scheduler, SubscriptionId, SubscriptionPolicy, WeightedFairScheduler,
};

const SUBSCRIPTION_COUNTS: [usize; 3] = [2, 8, 32];

fn id(value: &str) -> SubscriptionId {
    SubscriptionId::new(value)
}

fn policy(weight: u16, priority_class: i16) -> SubscriptionPolicy {
    SubscriptionPolicy::new(weight, priority_class, Duration::from_secs(30))
}

fn setup_scheduler(count: usize) -> WeightedFairScheduler {
    let mut scheduler = WeightedFairScheduler::default();
    for index in 0..count {
        let weight = u16::try_from((index % 3) + 1).expect("weight");
        let priority_class = i16::try_from(index % 2).expect("priority class");
        scheduler.register(id(&format!("sub-{index}")), policy(weight, priority_class));
    }
    mark_all_ready(&mut scheduler, count);
    scheduler
}

fn mark_all_ready(scheduler: &mut WeightedFairScheduler, count: usize) {
    for index in 0..count {
        scheduler.mark_ready(&id(&format!("sub-{index}")));
    }
}

fn bench_scheduler_next(c: &mut Criterion) {
    let mut group = c.benchmark_group("scheduler/next");

    for &count in &SUBSCRIPTION_COUNTS {
        let bench_id = BenchmarkId::new("subscriptions", count);
        group.bench_with_input(bench_id, &count, |b, &count| {
            let now = Instant::now();

            b.iter_batched(
                || setup_scheduler(count),
                |mut scheduler| {
                    let _selected = scheduler.next(now);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_scheduler_register(c: &mut Criterion) {
    let mut group = c.benchmark_group("scheduler/register");

    for &count in &SUBSCRIPTION_COUNTS {
        let bench_id = BenchmarkId::new("subscriptions", count);
        group.bench_with_input(bench_id, &count, |b, &count| {
            b.iter(|| {
                let _scheduler = setup_scheduler(count);
            });
        });
    }

    group.finish();
}

fn bench_scheduler(c: &mut Criterion) {
    bench_scheduler_register(c);
    bench_scheduler_next(c);
}

criterion_group!(scheduler_group, bench_scheduler);
criterion_main!(scheduler_group);
