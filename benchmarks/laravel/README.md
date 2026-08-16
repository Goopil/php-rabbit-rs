# rabbit-rs Laravel queue benchmark lab

Reproducible comparison of Laravel queue drivers to measure throughput,
latency, and resource usage across rabbit-rs, php-amqplib, vyuldashev,
Redis, and database.

## Structure

```
benchmarks/laravel/
  composer.json
  artisan                       # CLI entry point
  bootstrap/app.php             # minimal Laravel bootstrap
  app/
    Jobs/BenchmarkJob.php       # serializable job
    Console/Commands/
      PublishBenchmark.php       # publish N messages
      ConsumeBenchmark.php       # consume N messages and measure
  config/
    benchmark.php               # driver configs, payload/batch sizes
  drivers/
    BenchmarkDriver.php          # contract interface
    Support/MeasuresResources.php # metrics trait
    RabbitRsDriver.php           # native extension + Laravel driver
    PhpAmqplibDriver.php         # direct php-amqplib usage
    VyuldashevDriver.php         # vladimir-yuldashev/laravel-queue-rabbitmq
    RedisDriver.php              # Laravel Redis queue (predis)
    DatabaseDriver.php           # SQLite/MySQL queue (témoin)
  scripts/
    run-matrix.sh                # run all driver x payload x batch combos
    merge-result.php             # merge publish/consume JSON into entry
  tests/
    DriverContractTestCase.php   # contract test base
    DriverRegistryTest.php       # verifies all 5 drivers implement interface
  results/                       # JSON output from run-matrix
```

## Drivers

| Driver       | Package                                   | Requires broker |
| ------------ | ----------------------------------------- | --------------- |
| rabbit-rs    | ext-rabbit_rs + goopil/rabbit-rs-laravel   | RabbitMQ        |
| php-amqplib  | php-amqplib/php-amqplib                   | RabbitMQ        |
| vyuldashev   | vladimir-yuldashev/laravel-queue-rabbitmq | RabbitMQ        |
| redis        | predis/predis                             | Redis           |
| database     | PDO (SQLite/MySQL)                         | none            |

Drivers requiring an unavailable service (RabbitMQ, Redis) skip gracefully
and report zero throughput. The database driver uses file-based SQLite by
default so results persist across the publish/consume process boundary.

## Metrics

Each run captures:

- **throughput** (msg/s)
- **p50 / p95 / p99** latency (ms)
- **cpu_seconds** — cumulative CPU time
- **rss_kb** — resident set size
- **connections** — active connections
- **channels** — AMQP channels (RabbitMQ drivers only)
- **duplicates** — duplicate deliveries detected
- **losses** — messages published but not consumed

Environment metadata (PHP version, SAPI, OS, machine architecture, timestamp)
is recorded alongside every result set.

## Modes

The harness supports `cli`, `fpm`, and `octane` execution modes via the
`--mode` option on the artisan commands, or the `BENCH_MODE` environment
variable. The default is `cli`.

- **cli** — run via `php artisan publish|consume` (default; the matrix script uses this).
- **fpm** — invoke the benchmark through a web endpoint under PHP-FPM. The
  artisan command rejects `--mode=fpm` unless it is actually running under a
  web SAPI; for real FPM runs, wire a route to the command's logic.
- **octane** — run under a Laravel Octane worker. Start the Octane server and
  route the publish/consume logic through it; the command rejects
  `--mode=octane` unless invoked under the corresponding runtime.

Passing an unsupported mode aborts the run with a non-zero exit code.

## Running

### Smoke test (CI)

```bash
cd benchmarks/laravel
composer install
bash scripts/run-matrix.sh --smoke
```

Runs the database driver with 50 messages at 256-byte payload. Produces
a JSON file in `results/`.

### Full matrix

```bash
bash scripts/run-matrix.sh --full
```

Runs all 5 drivers across payload sizes 256 B–100 KB and batch sizes 1–256
with 5000 messages each. Requires RabbitMQ and Redis to be available;
unavailable brokers are skipped gracefully.

### Single run

```bash
# Publish
php artisan publish --driver=database --count=100 --payload-size=1024 --batch-size=16 --mode=cli

# Consume
php artisan consume --driver=database --count=100 --payload-size=1024 --batch-size=16 --mode=cli
```

### Contract tests

```bash
vendor/bin/phpunit
```

Verifies all 5 drivers implement the `BenchmarkDriver` interface and return
the expected metrics shape.

## Environment variables

| Variable                  | Default       | Description              |
| ------------------------- | ------------- | ------------------------ |
| `BENCH_RABBIT_RS_DSN`    | amqp://...    | rabbit-rs broker DSN     |
| `BENCH_AMQPLIB_HOST`     | 127.0.0.1     | php-amqplib host         |
| `BENCH_VYULDASHEV_DSN`   | amqp://...    | vyuldashev broker DSN    |
| `BENCH_REDIS_HOST`       | 127.0.0.1     | Redis host               |
| `BENCH_DB_CONNECTION`    | sqlite        | Database driver          |
| `BENCH_DB_DATABASE`      | tmp/bench.sqlite | Database path         |
| `BENCH_MODE`             | cli           | cli, fpm, octane         || `BENCH_SMOKE_COUNT`      | 50            | Messages in smoke mode   |
| `BENCH_FULL_COUNT`       | 5000          | Messages in full mode    |

## Integration with Milestone E

The results from this lab feed into `benchmarks/baselines/` (Task 40) to
calibrate default batching, prefetch, and buffer sizes. No thresholds are
set before measuring on a documented reference machine.
