//! Consumer pipeline hop benchmarks.
//!
//! Isolates the per-message hand-off costs of the consume wake chain
//! identified in #282: the `spawn_source`→actor mpsc hop, the actor→pop
//! flume hop, the `dispatch_notify` wake per pop, and the actor loop
//! structure (one command per select iteration vs draining every ready
//! command). Each hop runs on a current-thread runtime with a real task
//! wake: the sender wakes the parked receiver task, which answers back, so
//! the measured cost includes one full cooperative task schedule — without
//! the host-scheduler noise of a multi-threaded runtime.

use bytes::Bytes;
use rabbit_rs_core::transport::Headers;
use std::sync::Arc;

/// Payload size of a representative Laravel job body.
const PAYLOAD: Bytes = Bytes::from_static(b"hop-bench-payload-0123456789abcdef");

/// The command-shaped item handed across the pipeline channels.
struct Cmd {
    tag: u64,
    payload: Bytes,
    headers: Arc<Headers>,
}

impl Cmd {
    fn new(tag: u64) -> Self {
        Self {
            tag,
            payload: PAYLOAD.clone(),
            headers: Arc::new(Headers::new()),
        }
    }
}

/// A current-thread runtime plus the parked receiver task of one hop.
struct HopBench {
    runtime: tokio::runtime::Runtime,
}

impl HopBench {
    fn spawn() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("bench runtime");
        Self { runtime }
    }
}

fn main() {
    divan::main();
}

/// One `spawn_source`→actor hand-off: a tokio mpsc send that wakes the
/// parked receiver task. Bounds what batching this hop (lead 1 in #282)
/// can save per message.
#[divan::bench]
fn mpsc_hop(bencher: divan::Bencher) {
    let bench = HopBench::spawn();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Cmd>(64);
    let (done_tx, done_rx) = flume::unbounded();

    bench.runtime.block_on(async move {
        tokio::spawn(async move {
            let mut tag = 0_u64;
            while let Some(cmd) = rx.recv().await {
                divan::black_box((&cmd.tag, &cmd.payload, &cmd.headers));
                tag += 1;
                if done_tx.send(tag).is_err() {
                    return;
                }
            }
        });
    });

    let tag = std::sync::atomic::AtomicU64::new(0);
    bencher.bench(|| {
        let tag = tag.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        bench.runtime.block_on(async {
            tx.send(Cmd::new(tag)).await.expect("hop send");
            done_rx.recv_async().await.expect("hop receiver alive");
        });
    });
}

/// One actor→pop hand-off over the bounded flume buffer. Same wake shape
/// as [`mpsc_hop`] so the two channels compare directly.
#[divan::bench]
fn flume_hop(bencher: divan::Bencher) {
    let bench = HopBench::spawn();
    let (tx, rx) = flume::bounded::<Cmd>(64);
    let (done_tx, done_rx) = flume::unbounded();

    bench.runtime.block_on(async move {
        tokio::spawn(async move {
            let mut tag = 0_u64;
            while let Ok(cmd) = rx.recv_async().await {
                divan::black_box((&cmd.tag, &cmd.payload, &cmd.headers));
                tag += 1;
                if done_tx.send(tag).is_err() {
                    return;
                }
            }
        });
    });

    let tag = std::sync::atomic::AtomicU64::new(0);
    bencher.bench(|| {
        let tag = tag.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        bench.runtime.block_on(async {
            tx.send_async(Cmd::new(tag)).await.expect("hop send");
            done_rx.recv_async().await.expect("hop receiver alive");
        });
    });
}

/// One `dispatch_notify` wake: the `notify_one` that `next()` issues per
/// pop to re-arm the actor. Bounds what a low-watermark policy (lead 2 in
/// #282) can save per pop.
#[divan::bench]
fn notify_per_pop(bencher: divan::Bencher) {
    let bench = HopBench::spawn();
    let notify = Arc::new(tokio::sync::Notify::new());
    let (done_tx, done_rx) = flume::unbounded();

    bench.runtime.block_on({
        let notify = notify.clone();
        async move {
            tokio::spawn(async move {
                loop {
                    notify.notified().await;
                    if done_tx.send(()).is_err() {
                        return;
                    }
                }
            });
        }
    });

    bencher.bench(|| {
        bench.runtime.block_on(async {
            notify.notify_one();
            done_rx.recv_async().await.expect("notify waiter alive");
        });
    });
}

/// Actor loop structure over a pre-filled command queue: one await point
/// per command (the current select-per-iteration shape) versus one await
/// point draining every ready command (lead 3 in #282). Per-command cost
/// of the loop machinery itself, not of the command handling.
#[divan::bench(args = ["per_command", "drain"])]
fn actor_drain(bencher: divan::Bencher, mode: &str) {
    const COMMANDS: usize = 16;

    let bench = HopBench::spawn();
    let make_queue = || {
        let (tx, rx) = tokio::sync::mpsc::channel::<Cmd>(COMMANDS);
        for tag in 0..COMMANDS as u64 {
            tx.try_send(Cmd::new(tag)).expect("pre-fill command queue");
        }
        drop(tx);
        rx
    };

    match mode {
        "per_command" => bencher.with_inputs(make_queue).bench_values(
            |mut rx: tokio::sync::mpsc::Receiver<Cmd>| {
                while let Some(cmd) = bench.runtime.block_on(rx.recv()) {
                    divan::black_box(&cmd);
                }
            },
        ),
        "drain" => bencher.with_inputs(make_queue).bench_values(
            |mut rx: tokio::sync::mpsc::Receiver<Cmd>| {
                bench.runtime.block_on(async {
                    while let Ok(cmd) = rx.try_recv() {
                        divan::black_box(&cmd);
                    }
                });
            },
        ),
        other => panic!("unknown actor drain mode '{other}'"),
    }
}
