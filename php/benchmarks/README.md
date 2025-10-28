# RabbitMQ PHP Client Benchmarks

This benchmark suite compares the performance of different RabbitMQ PHP client libraries:

- AMQPLib (php-amqplib/php-amqplib)
- Bunny (bunny/bunny)
- RabbitRs (your custom PHP extension in Rust)

## Requirements

- PHP 7.4+
- Composer
- RabbitMQ server running locally (or update config)
- php-amqplib and bunny libraries (for comparison)
- Rust toolchain (cargo) for building the RabbitRs extension

## Installation

From the repository root, run:

```bash
bash run-benchmarks.sh
```

This helper spins up RabbitMQ via Docker Compose, installs dependencies if
needed, and executes the benchmark suite. You can pass additional arguments to
forward them to `src/run-benchmarks.php`.
