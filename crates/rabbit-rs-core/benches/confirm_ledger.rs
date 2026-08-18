use std::collections::{BTreeMap, HashMap};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

const SIZES: [usize; 2] = [256, 1024];

fn bench_btreemap(c: &mut Criterion) {
    let mut group = c.benchmark_group("confirm_ledger/insert_remove");
    group.sample_size(20);

    for &size in &SIZES {
        let bench_id = BenchmarkId::new("btreemap", size);
        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(bench_id, &size, |b, &size| {
            b.iter(|| {
                let mut map = BTreeMap::<u64, u64>::new();
                for seq in 0..u64::try_from(size).expect("size") {
                    map.insert(seq, seq);
                }
                for seq in 0..u64::try_from(size).expect("size") {
                    let _ = map.remove(&seq);
                }
            });
        });
    }

    group.finish();
}

fn bench_hashmap(c: &mut Criterion) {
    let mut group = c.benchmark_group("confirm_ledger/insert_remove");
    group.sample_size(20);

    for &size in &SIZES {
        let bench_id = BenchmarkId::new("hashmap", size);
        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(bench_id, &size, |b, &size| {
            b.iter(|| {
                let mut map = HashMap::<u64, u64>::new();
                for seq in 0..u64::try_from(size).expect("size") {
                    map.insert(seq, seq);
                }
                for seq in 0..u64::try_from(size).expect("size") {
                    let _ = map.remove(&seq);
                }
            });
        });
    }

    group.finish();
}

fn bench_vec_slab(c: &mut Criterion) {
    let mut group = c.benchmark_group("confirm_ledger/insert_remove");
    group.sample_size(20);

    for &size in &SIZES {
        let bench_id = BenchmarkId::new("vec_slab", size);
        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(bench_id, &size, |b, &size| {
            b.iter(|| {
                let mut slab: Vec<Option<u64>> = vec![None; size];
                for seq in 0..u64::try_from(size).expect("size") {
                    let idx = usize::try_from(seq).expect("sequence fits");
                    slab[idx] = Some(seq);
                }
                for seq in 0..u64::try_from(size).expect("size") {
                    let idx = usize::try_from(seq).expect("sequence fits");
                    slab[idx] = None;
                }
            });
        });
    }

    group.finish();
}

fn bench_confirm_ledger(c: &mut Criterion) {
    bench_btreemap(c);
    bench_hashmap(c);
    bench_vec_slab(c);
}

criterion_group!(confirm_ledger_group, bench_confirm_ledger);
criterion_main!(confirm_ledger_group);
