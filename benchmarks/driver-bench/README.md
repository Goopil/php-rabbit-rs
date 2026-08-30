# driver-bench — Phase E (driver-level Laravel queue benchmark)

Standalone minimal Laravel app that benchmarks the full Laravel queue API
(dispatch / pop / ack) of three RabbitMQ drivers on the same lab broker:

| Connection        | Package                                        | Transport              |
|-------------------|------------------------------------------------|------------------------|
| `rabbit-rs`       | `goopil/rabbit-rs-laravel` (path repo)         | `ext-rabbit_rs` (Rust) |
| `rabbitmq-amqplib`| `vladimir-yuldashev/laravel-queue-rabbitmq`    | php-amqplib (pure PHP) |
| `rabbitmq-ext`    | `iamfarhad/laravel-rabbitmq`                   | `ext-amqp` (C)         |

Phase A measured the raw transport; Phase E answers: does the ~3x publish
gap survive once the framework layer is in front of it?

## Layout

```
driver-bench/
├── composer.json          # laravel/framework ^12||^13 + the 3 drivers
│                          # (composer.lock is generated locally — gitignored
│                          # by repo convention; platform.php pinned to 8.4
│                          # so it resolves identically local and Docker)
├── artisan                # minimal artisan (debugging only)
├── bootstrap/app.php      # minimal bootstrap + unambiguous driver names
├── config/
│   ├── app.php            # minimal
│   ├── queue.php          # the THREE connections (never interchangeable)
│   └── rabbit-rs.php      # published config of the goopil driver
├── bin/bench.php          # the benchmark runner
├── docker/Dockerfile      # php:8.4-cli + librabbitmq + pecl amqp + composer
└── .env.example           # per-driver credentials/vhosts for the lab
```

## Setup

```bash
cd benchmarks/driver-bench
cp .env.example .env
composer install --ignore-platform-req=ext-rabbit_rs --ignore-platform-req=ext-amqp
```

- `platform.php` is pinned to `8.4.0` so the lock file resolves identically
  for the local PHP (8.5) and the Docker image (php:8.4-cli).
- `--ignore-platform-req` is needed because neither `ext-rabbit_rs` nor
  `ext-amqp` is installed system-wide (protocol): the rabbit_rs extension is
  loaded per-run with `-d extension=<dylib>`, ext-amqp only exists in Docker.

Broker: start the lab if needed (`docker compose up -d` in `lab/`), broker at
`127.0.0.1:5672`.

### The ext-rabbit_rs extension (goopil driver, local runs)

Follow the benchmarks/ release protocol: build in release mode and load the
dylib explicitly, never install system-wide:

```bash
./scripts/install.sh --release   # builds target/release/librabbit_rs_php.dylib
```

Then prefix local rabbit-rs runs with
`php -d extension=<repo>/target/release/librabbit_rs_php.dylib`. A debug build
masks throughput by ~4x — do not benchmark it.

## Running

```bash
# dispatch: unit Queue::push x N (measured)
php -d extension=../../target/release/librabbit_rs_php.dylib bin/bench.php \
  --connection=rabbit-rs --mode=dispatch --count=1000

# worker: unmeasured fill (push x N) + settle, then measured pop+ack x N
php -d extension=../../target/release/librabbit_rs_php.dylib bin/bench.php \
  --connection=rabbit-rs --mode=worker --count=1000 --rounds=3

# vladimir (local, pure PHP — no extension flag)
php bin/bench.php --connection=rabbitmq-amqplib --mode=worker --count=1000

# iamfarhad (Docker only — local PHP has no ext-amqp)
docker build -t rabbit-rs-driver-bench ./docker
docker run --rm --network host -v "$PWD":/app -w /app rabbit-rs-driver-bench \
  php bin/bench.php --connection=rabbitmq-ext --mode=worker --count=1000
```

Options: `--connection=` (required), `--mode=dispatch|worker`,
`--count=` (default 10000), `--rounds=`, `--settle-ms=` (default 500),
`--output=PATH` (also writes the JSON to a file).

The run exits non-zero unless zero messages were lost: worker mode requires
exactly `count` popped+acked messages and an empty queue afterwards
(a 5 s settling window absorbs in-flight stragglers; anything surfacing there
is reported as `late_arrivals_after_drain` and fails the run).

Output is a single JSON object (stdout) in the style of the Phase A archives:
avg/min/max rate, per-round detail, payload size, masked config echo, and
environment metadata.

### Vladimir as the comparability bridge

`rabbitmq-amqplib` runs BOTH locally (same host as goopil) and inside the
Docker image (same environment as iamfarhad). Use its local vs Docker delta
to normalize the two execution environments when reading cross-driver
numbers. iamfarhad runs Docker-only.

## Fairness

- Same lab broker (`127.0.0.1:5672`), **one dedicated vhost + user per
  driver**, using the grants that already exist in
  `lab/rabbitmq/rabbitmq/definitions.json` (d35580c) — nothing was modified:

  | Driver             | Vhost        | User        | Queues                    |
  |--------------------|--------------|-------------|---------------------------|
  | rabbit-rs (goopil) | `/`          | `rabbit_rs` | `bench.goopil.*`          |
  | rabbitmq-amqplib   | `/orders-eu` | `admin`     | `bench.vladimir.*`        |
  | rabbitmq-ext       | `/billing`   | `admin`     | `bench.iamfarhad.*`       |

  (`rabbit_rs` configure grant on `/` is `^(amq\.|bench\.|benchmark_)`;
  `admin` has full grants on all vhosts.)

- Same payload for every driver: the real Laravel queue envelope (JSON job
  payload) sized to 1024 bytes (`bench.php` measures the exact serialized
  body through the driver's own payload path and reports it as
  `payload_body_bytes`).
- Same bench model: unit `Queue::push` (dispatch) and fill-blind +
  unit pop+ack (worker), mirroring the Phase A `laravel-*` scenarios.
- Each queue is purged before every run (driver purge API); each worker
  round starts from a fresh queue connection.

### Driver default configuration (as run)

| Setting             | rabbit-rs (goopil)            | rabbitmq-amqplib (vladimir)  | rabbitmq-ext (iamfarhad)     |
|---------------------|-------------------------------|------------------------------|------------------------------|
| Publish API         | `Queue::push` (unit)          | `Queue::push` (unit)         | `Queue::push` (unit)         |
| Publisher confirms  | yes + mandatory (`safety=safe`) | none                        | **off by default** (`publisher_confirms.enabled=false`); confirms variant: `IAMFARHAD_PUBLISHER_CONFIRMS=true` |
| Pop implementation  | pull consumer `next(0)`       | `basic_get`                  | `basic_get` (poll mode)      |
| Prefetch            | 64 (worker subscription)      | n/a for `basic_get`          | qos only in consume mode; n/a for poll |
| Queue declaration   | `topology_mode=declare`       | on first push                | on first push/pop            |
| Queue type          | **classic** durable (deviation: package default is quorum) | classic durable | classic durable (`quorum=false`) |
| Exchange            | default (`''`)                | default (`''`)               | default (`''`)               |
| Delay handling      | ttl buckets / plugin detect   | per-TTL delay queues         | ttl buckets / plugin         |

Honest differences to keep in mind when reading numbers:

- goopil publishes with confirms+mandatory per message; vladimir publishes
  fire-and-forget; iamfarhad's confirms are opt-in. For a confirms-fair
  dispatch comparison, enable `IAMFARHAD_PUBLISHER_CONFIRMS=true` (and
  `_MANDATORY=true`).
- goopil's pop is a pull consumer over a native subscription (prefetch
  window 64); vladimir and iamfarhad pop with `basic_get`. Prefetch for
  goopil is set through `RABBIT_RS_PREFETCH` (64, matching the Phase A
  `laravel-worker` scenario).
- goopil queue type was moved from its package default (quorum) to classic
  so all three drivers use classic durable queues.

### Known ext-rabbit_rs consumer quirks worked around by the runner

Both issues are observed at driver level under unit pop+ack churn and are
NOT fixed in this task (driver/core changes are out of scope); the runner
documents and works around them:

1. **Consumer created before the fill misses deliveries.** If the first
   `pop()` (which creates the native consumer) happens before the fill is
   ingested, a fraction of the filled messages never surface. The worker
   mode therefore never pops before the measured drain, and each round
   rebuilds the queue connection (fresh pools + consumer).
2. **Ack pipeline stall mid-drain.** During sustained pop+ack the consumer
   can stop receiving deliveries while messages remain ready in the queue
   (independent of prefetch: observed with 1 and 64). The drain detects the
   stall (400 consecutive null pops) and rebuilds the connection, mirroring
   a real worker's idle-reconnect. The stall wait stays inside the measured
   time, and `stall_recoveries` is reported per round. A run only counts as
   OK when every message was accounted for (0 losses, 0 late arrivals).
