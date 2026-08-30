# Milestone B Audit

**Date:** 2026-08-01
**Scope:** performance, stability, and test coverage of the native PHP extension at the end of Milestone B.

## Verdict

Milestone B is functionally advanced, but it must not yet be considered production-stable.

- **Performance: 6/10** — promising architecture, but no real measurements yet validate the high-performance promise.
- **Stability: 5/10** — two significant concurrency defects can affect delivery guarantees and resource lifecycle.
- **Coverage: 7/10** — good deterministic coverage, with several critical blind spots still to address.

## Priority fixes

### 1. Critical — a delivery can be absorbed by an expired waiter

`Consumer::next()` cancels its future after expiry, but the cancelled waiter may remain in the queue. The next delivery can then be pulled off the buffer and sent to this closed waiter, making it invisible to PHP while leaving it unacknowledged on the broker side.

Possible consequences:

- progressive consumer stall until a closure or redelivery;
- prefetch saturation after several timeouts;
- practical violation of the "no silent loss" goal.

Files involved:

- `crates/rabbit-rs-core/src/consumer.rs`;
- `crates/rabbit-rs-core/src/consumer/set.rs`;
- `crates/rabbit-rs-core/src/consumer/actor.rs`.

Expected fix: make waiters cancellable and guarantee that a delivery is only removed from the buffer when an active recipient can receive it. Add a deterministic test for "timeout, then a delivery arrives".

### 2. High — race between closure and operation creation

`ClientPool::publish()` and `ClientPool::consumer()` check the closed state before creating or fetching their resources. A concurrent closure can drain the registries between that check and the insertion, allowing a connection, publisher, or consumer to be created after `close()`.

Possible consequences:

- live resources after the declared pool closure;
- unpredictable behavior under Octane or PHP ZTS;
- incomplete shutdown and network resource leaks.

Main file: `crates/rabbit-rs-core/src/client.rs`.

Expected fix: make the closure transition atomic with respect to resource creation. Add tests covering the `close/publish` and `close/consumer` races, and closure during a confirm.

### 3. High — unbounded memory at the PHP boundary

The individual payload is capped at 1 MiB, but the message count, cumulative size of a `publishBatch()`, and header volume are not bounded. The batch is fully converted and copied before publisher backpressure can apply.

Possible consequences:

- large memory spike or memory exhaustion;
- high CPU cost during conversion;
- backpressure arriving too late to protect the PHP process.

Main file: `crates/rabbit-rs-php/src/conversion.rs`.

Expected fix: define and enforce explicit limits for message count, total batch size, header count, header depth, and cumulative header size. Errors must precisely identify the offending input path.

### 4. Medium — extreme durations and potentially unbounded shutdown

`timeout_ms` accepts values up to `PHP_INT_MAX`. Their conversion, or their addition to an `Instant`, can overflow platform limits. Additionally, `RuntimeRegistry::close()` awaits network closures without a deadline.

Possible consequences:

- panic on an invalid or excessive duration;
- a shutdown or FPM reload hanging;
- uncontrolled operational latency.

Files involved:

- `crates/rabbit-rs-php/src/conversion.rs`;
- `crates/rabbit-rs-core/src/runtime.rs`.

Expected fix: cap and validate durations before conversion, then bound shutdown time with an explicit policy for resources that fail to close within the allotted window.

## Performance

### Strengths

- runtime and connections created lazily;
- handles reused per PID and configuration normalized;
- publisher and replay queues explicitly bounded;
- atomic metrics;
- `publishBatch()` crosses the PHP boundary once and submits messages before waiting for confirmations.

### Current limitations

- no benchmarks for throughput, p50/p95/p99 latency, CPU, or RSS;
- payload and header copies at the PHP boundary;
- full configuration revalidation and hashing on every `Pool` construction;
- publisher, consumer, and connection mutexes held during some network operations, which can serialize the initialization of independent profiles.

The "high-performance" promise therefore remains plausible, but undemonstrated.

## Test coverage

### Existing coverage

The last full gate was green with:

- 112 Rust tests;
- 9 PHPT tests;
- Clippy and formatting;
- Composer validation;
- a two-worker FPM lab.

Covered scenarios include, among others: API and reflection, binary payloads, invalid configurations, secrets, double ACK, backpressure, closure, fork, and handle reuse under FPM. Mock fixtures are not exposed in the production binary.

### Tests still needed

- consumer timeout followed by a delivery arrival;
- `close/publish` and `close/consumer` races;
- closure during a publisher confirm;
- `release()` and `reject()` through the PHP API;
- successful publish, mandatory return, confirm timeout, and `ConnectionException` from PHP;
- batches and headers at and beyond the limits;
- extreme duration values;
- shutdown with live connections and actors;
- validation on PHP 8.5, Linux glibc/musl, ARM64, and ZTS;
- a real RabbitMQ cluster, chaos scenarios, and benchmarks.

## Recommended treatment order

1. Fix the cancelled consumer waiter that can absorb a delivery.
2. Make closure atomic against concurrent operations.
3. Bound batches, headers, and durations.
4. Add concurrency and shutdown tests.
5. Establish a baseline of microbenchmarks for FFI, conversion, and batching.

Milestone B can be considered stabilized once the first four points are fixed and covered by deterministic tests. Benchmarks and cross-platform validation can then serve as entry criteria for production qualification.
