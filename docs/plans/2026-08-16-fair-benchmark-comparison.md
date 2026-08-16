# Plan: Fair Benchmark Comparison — Confirms & Mandatory Parity

**Date:** August 16, 2026  
**Status:** Draft — not yet implemented  
**Supersedes:** The Laravel comparison section of `benchmarks/BASELINE.md`

## Problem

The current Laravel comparison lab benchmarks rabbit-rs against php-amqplib and vyuldashev, but the comparison is **not apples-to-apples**:

| Driver      | Publisher Confirms | Mandatory Returns | Persistent |
|-------------|:------------------:|:-----------------:|:----------:|
| rabbit-rs   | ✅ (default on)    | ✅ (default on)    | ✅         |
| php-amqplib | ❌                 | ❌                 | ✅         |
| vyuldashev  | ❌                 | ❌                 | ?          |
| redis       | N/A                | N/A               | ✅         |
| database    | N/A                | N/A               | ✅         |

rabbit-rs waits for a broker ACK per message (confirm) and verifies routability (mandatory). The other drivers fire-and-forget. This makes rabbit-rs appear 5–20× slower in raw throughput, but it's the only driver with **zero losses**.

The current numbers answer "is rabbit-rs fast?" but not "is rabbit-rs fast **for the same safety guarantees**?"

## Goal

Produce **three comparison axes** in the benchmark output:

1. **Unsafe (fire-and-forget):** All drivers with confirms/mandatory disabled. Measures raw FFI + I/O cost.
2. **Safe (confirms only):** All AMQP drivers with `confirm_select` enabled, no mandatory. Measures the cost of waiting for broker ACKs.
3. **Safest (confirms + mandatory):** All AMQP drivers with both enabled. This is rabbit-rs's default and the production-recommended mode.

This gives the user a clear picture: "rabbit-rs adds X µs of FFI overhead, confirms cost Y%, and mandatory costs Z%."

## Changes

### 1. Driver Interface: Add a Safety Mode

**File:** `benchmarks/laravel/drivers/BenchmarkDriver.php`

Add a `safetyMode` concept to the interface:

```php
enum PublishSafety: string {
    case Unsafe = 'unsafe';      // fire-and-forget
    case Confirms = 'confirms';   // confirm_select, no mandatory
    case Safest = 'safest';       // confirm_select + mandatory
}

interface BenchmarkDriver {
    public function setup(PublishSafety $safety = PublishSafety::Safest): void;
    // ... existing methods
}
```

### 2. PhpAmqplibDriver: Implement Safety Modes

**File:** `benchmarks/laravel/drivers/PhpAmqplibDriver.php`

- `unsafe`: `basic_publish()` without `confirm_select`, `mandatory = false`
- `confirms`: call `$channel->confirm_select()` before publishing, wait for ACKs via `$channel->wait_for_pending_acks()`
- `safest`: same as confirms + `mandatory = true` on `basic_publish()`

### 3. VyuldashevDriver: Implement Safety Modes

**File:** `benchmarks/laravel/drivers/VyuldashevDriver.php`

- vyuldashev/laravel-queue-rabbitmq may not expose `confirm_select` or `mandatory` directly. If it doesn't:
  - `unsafe`: use `pushRaw()` as-is (current behavior)
  - `confirms`/`safest`: fall back to a raw `php-amqplib` connection with the same exchange/queue, or mark as "not supported" and skip

### 4. RabbitRsDriver: Implement Safety Modes

**File:** `benchmarks/laravel/drivers/RabbitRsDriver.php`

- `unsafe`: set `publisher.confirms = false`, `publisher.mandatory = false` in the native config
- `confirms`: `publisher.confirms = true`, `publisher.mandatory = false`
- `safest`: both `true` (current default)

### 5. Redis & Database Drivers

These don't have AMQP confirms. They run the same in all three modes. Keep them as baselines.

### 6. run-matrix.sh: Run All Three Modes

**File:** `benchmarks/laravel/scripts/run-matrix.sh`

Loop over safety modes:

```bash
SAFETY_MODES=("unsafe" "confirms" "safest")
for safety in "${SAFETY_MODES[@]}"; do
    for driver in "${DRIVERS[@]}"; do
        # ... pass --safety=$safety to artisan
    done
done
```

### 7. Artisan Commands: Accept --safety Flag

**Files:** `PublishBenchmark.php`, `ConsumeBenchmark.php`

Add `--safety` option (default: `safest`), pass to driver `setup()`.

### 8. Results Format: Include Safety Mode

Each result entry includes `"safety": "safest"` so the JSON output can be filtered and compared.

### 9. BASELINE.md: Three Comparison Tables

Replace the single comparison table with three:

- **Unsafe (fire-and-forget):** Raw throughput — who's fastest without safety
- **Confirms only:** Cost of waiting for broker ACKs
- **Safest (confirms + mandatory):** Production-recommended — who's fastest with full safety

Add a "cost of safety" analysis: throughput delta between unsafe and safest per driver.

## Expected Outcome

The fair comparison should show:

1. **Unsafe mode:** rabbit-rs should be competitive with php-amqplib (within 1–2×), since the FFI overhead is only ~1–2 µs/call
2. **Safest mode:** rabbit-rs should be competitive with php-amqplib when both use confirms + mandatory, since the broker I/O dominates
3. **Cost of safety:** The throughput drop from unsafe → safest should be quantified per driver

## Testing

- Add a `DriverContractTestCase` assertion that each driver accepts all three safety modes
- Verify `run-matrix.sh --full` produces results with all three modes
- Verify the JSON output includes the `safety` field

## Files to Change

| File | Change |
|------|--------|
| `drivers/BenchmarkDriver.php` | Add `PublishSafety` enum, update interface |
| `drivers/PhpAmqplibDriver.php` | Implement confirm_select + mandatory per mode |
| `drivers/VyuldashevDriver.php` | Implement safety modes or mark unsupported |
| `drivers/RabbitRsDriver.php` | Read safety mode from config, set publisher flags |
| `drivers/RedisDriver.php` | No-op (same in all modes) |
| `drivers/DatabaseDriver.php` | No-op (same in all modes) |
| `app/Console/Commands/PublishBenchmark.php` | Add `--safety` option |
| `app/Console/Commands/ConsumeBenchmark.php` | Add `--safety` option |
| `scripts/run-matrix.sh` | Loop over safety modes |
| `config/benchmark.php` | Add safety mode config |
| `tests/DriverContractTestCase.php` | Assert safety mode support |
| `benchmarks/BASELINE.md` | Three comparison tables + cost-of-safety analysis |

## Priority

Medium — this is a benchmark correctness issue, not a production bug. The current numbers are honest (rabbit-rs IS slower with confirms on), but the comparison is misleading without the unsafe baseline.

## Estimate

~2–3 hours of implementation. Mostly mechanical changes to the driver classes and the matrix script.
