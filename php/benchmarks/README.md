# RabbitMQ PHP Client Benchmarks

This benchmark suite compares RabbitMQ client performance across multiple PHP
implementations and scenarios:

- RabbitRs (Rust extension loaded via `ext-rabbit_rs`)
- AMQPLib (`php-amqplib/php-amqplib`)
- Bunny (`bunny/bunny`)
- php-amqp extension (`ext-amqp`, backed by `librabbitmq`)

Each library is exercised in `fire-and-forget`, `batch-confirm`, and `auto-ack`
scenarios. RabbitRs also exposes an `auto-qos` benchmark for its adaptive
prefetch mode.

## Requirements

- PHP 7.4+
- Composer
- RabbitMQ server running locally (or update config)
- `php-amqplib/php-amqplib` and `bunny/bunny` (installed via Composer)
- `ext-amqp` (PECL) plus `librabbitmq` headers for php-amqp benchmarks:
  ```bash
  pecl install amqp
  printf "extension=amqp.so\n" > /etc/php.d/amqp.ini # or equivalent
  ```
- Rust toolchain (cargo) if you want to include RabbitRs in the matrix

## Installation

From the repository root, run:

```bash
bash run-benchmarks.sh
```

This helper spins up RabbitMQ via Docker Compose, installs dependencies if
needed, and executes the benchmark matrix. Append extra CLI flags to forward
them to `src/run-benchmarks.php`. For instance, to focus on auto-ack tests:

```bash
bash run-benchmarks.sh -- --scenario=auto-ack
```

If a library/extension is missing (e.g., `ext-amqp`), the runner will skip its
benchmarks while continuing with the remaining scenarios.
