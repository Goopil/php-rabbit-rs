# rabbit-rs native benchmarks

Microbenchmarks for the Rust core and PHP extension, used to calibrate
batching, prefetch, and buffer defaults in Milestone E.

## Structure

```
benchmarks/native/
  Cargo.toml             # crate manifest
  benches/
    batching.rs          # publisher batching path (batch size x payload x confirms)
    ffi_conversion.rs    # core conversion: config validation, message, headers
    scheduler.rs          # weighted-fair scheduler decision cost
    transport.rs          # mock transport publish + confirm cycle
  php/
    ffi_conversion.php    # PHP harness exercising the compiled extension
```

## Measured dimensions

All suites use the mock transport — no broker is required.

| Suite          | Dimensions                                             |
| -------------- | ------------------------------------------------------ |
| batching       | batch 1/16/64/256, payload 256 B–1 MiB, ±confirms     |
| ffi_conversion | payload 256 B–1 MiB, header counts 0/8/32/128         |
| scheduler      | subscription counts 2/8/32                             |
| transport      | batch 1/16/64/256, payload 256 B–1 MiB, mock cycle    |

## Running the Rust benchmarks

```bash
cargo bench -p rabbit-rs-native-bench
```

To compile without running:

```bash
cargo bench -p rabbit-rs-native-bench --no-run
```

Criterion writes HTML reports to `target/criterion/`.

## Running the PHP benchmark

The PHP harness exercises the compiled extension and measures the
full FFI boundary (config validation, message conversion, payload and
header copying) without a broker connection.

```bash
php benchmarks/native/php/ffi_conversion.php
```

The extension must be installed first:

```bash
./scripts/install.sh
```

The script records PHP version, SAPI, NTS/ZTS mode, OS, kernel, CPU,
RabbitMQ version, payload size, and header count alongside each result.

## Environment recording

Every benchmark run should be accompanied by:

- Rust toolchain version (`rustc --version`)
- PHP version and SAPI (`php -v`, `php -i | grep Thread`)
- OS and kernel (`uname -a`)
- CPU model (`sysctl -n machdep.cpu.brand_string` on macOS,
  `lscpu | grep 'Model name'` on Linux)
- RabbitMQ version (when a broker is used)

## Integration with Milestone E

The baselines produced here feed into `benchmarks/baselines/` and
calibrate the default values for:

- `max_messages` (batch size)
- `max_bytes` (batch byte budget)
- `buffer_capacity` (publisher in-flight permits)
- `prefetch` (consumer QoS)
- `confirm_timeout` (publisher confirm deadline)

No thresholds are set before measuring on a documented reference machine.
