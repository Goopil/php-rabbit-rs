# Rabbit RS — Native Rust PHP extension and Laravel driver for RabbitMQ

**Status:** approved on July 30, 2026

## Goal

Build a PHP extension written in Rust and a Laravel package capable of publishing and consuming RabbitMQ jobs at minimal cost, while preserving expected Laravel Queue behavior. A single worker must be able to aggregate multiple connections, vhosts, queues, and channels. Connections must be reused as much as possible within each PHP process and restored automatically after an outage.

## V1 scope

- PHP 8.4 and 8.5.
- Laravel 12 and 13.
- RabbitMQ 4.3.x.
- Linux x86_64 and ARM64.
- glibc and musl distributions.
- SAPIs: CLI, PHP-FPM, and Octane.
- Octane servers: FrankenPHP, RoadRunner, Open Swoole, and Swoole.
- AMQP 0-9-1.
- Quorum queues by default, classic queues configurable.
- At-least-once delivery.
- Multiple vhosts and subscriptions from a single Laravel worker.
- Topology managed by the library or provisioned externally.
- Immediate, delayed, released, failed, and bulk jobs.
- Reconnection, publisher confirms, mandatory routing, backpressure, and metrics.

## Out of initial scope

- PHP 8.3 and earlier.
- Windows and macOS as distributed production platforms.
- AMQP 1.0 and RabbitMQ Streams.
- Exactly-once.
- Sharing a TCP connection between multiple OS processes.
- Adaptive prefetch controller enabled by default.
- Horizon-equivalent dashboard.
- SQS benchmark.

## Key decisions

### Naming

The public name of the ecosystem is Rabbit RS. Its tagline is: High-performance RabbitMQ transport for PHP and Laravel, powered by Rust.

The technical names are:

- main repository: rabbit-rs/rabbit-rs;
- PIE package of the extension: goopil/rabbit-rs-native;
- internal name of the PHP extension: rabbit_rs;
- Composer platform dependency: ext-rabbit_rs;
- Laravel package: goopil/rabbit-rs-laravel;
- native PHP namespace: Goopil\RabbitRs;
- Laravel package namespace: Goopil\RabbitRs\Laravel;
- Rust crates: rabbit-rs-core and rabbit-rs-php;
- Laravel driver: rabbit-rs;
- Artisan commands: rabbit-rs:work and rabbit-rs:status;
- configuration file: rabbit-rs.php.

The extension and the Laravel package use a synchronized version. A 1.2.0 release therefore produces goopil/rabbit-rs-native 1.2.0 and goopil/rabbit-rs-laravel 1.2.0. The Laravel package requires a compatible version of ext-rabbit_rs.

### Hybrid Laravel architecture

The first layer is a standard Laravel driver. The queue:work command remains responsible for the processing loop, signals, events, memory limits, timeouts, and failed jobs.

A Laravel connection can reference an aggregated worker profile. This profile holds multiple subscriptions spread across different brokers and vhosts. The driver's pop method asks the Rust core for the next available message across the whole profile.

A rabbit-rs:work command will be added in a second milestone. It can supervise several standard queue:work processes and forward signals to them. It will not reimplement the Illuminate\Queue\Worker loop.

### Three layers

1. rabbit-rs-core: a Rust crate independent of PHP, containing configuration, runtime, pool, AMQP actors, topology, publishing, consuming, scheduling, reconnection, and metrics.
2. rabbit-rs-php: an ext-php-rs extension exposing a minimal PHP API and carrying only owned values between PHP and Rust.
3. goopil/rabbit-rs-laravel: a Composer package containing the connector, Queue driver, Job, configuration, commands, and Octane integration.

Lapin is the initial AMQP client. It uses Tokio, supports AMQP 0-9-1, publisher confirms, and automatic recovery. It stays hidden behind a transport abstraction so it can be replaced after benchmarking.

## Runtime lifecycle

Each PHP process owns exactly one native registry. The Tokio runtime and sockets are created lazily after the fork. The registry records the PID; a PID change invalidates all inherited resources.

A normalized connection key contains:

- host set and selection strategy;
- port and TLS settings;
- identity and authentication mechanism;
- vhost;
- heartbeat, timeouts, and negotiable AMQP settings;
- configuration fingerprint.

A vhost requires its own AMQP connection. Channels are reused within that connection. Consumer channels remain dedicated for the lifetime of their consumer. Publishing channels come from a bounded pool.

FPM reuses the registry between requests of the same worker. Octane keeps it for the lifetime of the persistent worker. Two processes never share a registry.

Rust threads hold no zvals, Zend objects, PHP callbacks, Laravel containers, or Request objects. They only manipulate owned strings, bytes, numbers, and Rust structures.

## Publishing

The Laravel package serializes the job in Laravel format, assigns a stable message_id, then calls the extension.

The native publisher:

1. validates and copies the payload and properties;
2. enqueues the command into a bounded queue;
3. publishes with persistent delivery_mode and mandatory=true;
4. associates sequence numbers with confirm waiters;
5. processes basic.return before confirmations;
6. resolves each waiter only after ACK, NACK, return, or timeout.

A reliable publish call waits for its confirmation before handing control back to PHP. The publishBatch method transmits a full array in a single FFI crossing and is the fast path for Laravel bulk.

An outage before confirmation leaves the state ambiguous. By default, the at-least-once policy keeps unsent and ambiguous publications in process memory, then automatically republishes them with the same message_id once the connection, topology, and a channel with confirms are ready again. The original deadline keeps applying during the outage: it is never reset by a reconnection.

The publisher enters a suspended state during recovery but keeps accepting commands until its overall bounded capacity is reached. That capacity covers pending commands and in-flight confirms so that an actor draining its channel during a long outage cannot accumulate unbounded memory. Once capacity is reached, new publications receive Backpressure.

A publication never written can be replayed without ambiguity. A written but unconfirmed publication is replayed automatically to avoid any silent loss; this may create a duplicate and therefore requires idempotent jobs. ACK, NACK, basic.return, permanent error, or deadline expiry are terminal and resolve the waiter exactly once. This guarantee is process-local: a PHP process crash loses the in-memory buffer; a guarantee beyond a crash would require a persistent outbox, out of V1 scope.

## Multi-vhost consumption

A ConsumerSet owns multiple subscriptions. Each subscription references:

- a broker and its vhost;
- a queue;
- a stable alias;
- a fairness weight;
- an inter-queue priority class;
- a prefetch configuration;
- topology options and, when explicitly enabled, application-level dead-lettering options.

Deliveries land in bounded buffers. A deficit weighted round-robin scheduler picks the next message while honoring weights and an aging policy preventing starvation.

The AMQP priority of a message within a queue is distinct from the priority of a subscription across multiple queues.

V1 uses fixed prefetch per subscription and a global max_in_flight budget per worker. The metrics needed by the future adaptive controller are collected from V1: job duration, reserved time, buffer depth, ACK latency, and memory pressure.

> **Note (2026-08-29):** the global `max_in_flight` budget described above as live in V1 has since been removed — unacknowledged deliveries are now bounded only by the per-consumer-channel QoS prefetch; the removal is tracked by the consumer-tuning plan (PR #29). This document remains a point-in-time record.

## ACK, retry, and attempts

Each message handed to PHP carries an opaque native token with connection identity, channel, consumer, delivery tag, and connection generation.

- delete sends basic.ack.
- release(0) sends basic.reject with requeue=true.
- release(delay > 0) republishes to the delay mechanism, waits for the publisher confirm, then ACKs the original message.
- a failed delayed publication leaves the original unacknowledged.
- a connection closure automatically requeues unacknowledged messages.

basic.reject is preferred over basic.nack for a single delivery: quorum queues can then increment their delivery counters. The x-acquired-count and x-delivery-count headers of RabbitMQ 4.3 are used together with the application counter to implement attempts.

After an outage, an ACK carrying an old generation is rejected by the extension. The broker redelivers the message. If the job had already completed on the PHP side before the ACK failure, its processing may therefore be repeated.

## Delays

The delay driver is auto by default:

1. use rabbitmq_delayed_message_exchange when available and permitted;
2. otherwise use TTL queues with a dead-letter exchange.

The TTL fallback uses bounded, configurable buckets. Delay queues are declared lazily, durable when needed, and given a queue expiry to avoid unbounded topology growth. Delays are rounded up to the bucket so a job is never delivered before its due time.

## Reconnection

The connection state machine is:

    Disconnected -> Connecting -> Ready -> Recovering -> Ready
                                   |
                                   +-> Draining -> Closed

Retries use exponential backoff with jitter and a cap. Authentication errors or incompatible topologies are classified as permanent and surfaced without an infinite loop in publishing contexts. A consuming worker may keep retrying according to its policy.

Recovery follows a deterministic order:

1. connection and negotiation;
2. channels;
3. exchanges;
4. queues;
5. bindings;
6. QoS;
7. consumers.

Interrupted publisher confirms are classified as ambiguous without immediately resolving the call, then placed back into the bounded replay buffer. After a new generation, topology and confirm mode are restored before their republishing. Consumed but unacknowledged messages are redelivered by RabbitMQ.

## Topology

Three modes are available:

- declare: idempotently declare the topology and fail on incompatibility;
- verify: perform passive declarations and check expected properties;
- external: use the topology without modifying it.

Automatically created queues are durable, non-exclusive, non-auto-delete quorum queues. Classic remains configurable. No application DLQ is created by default: dead-lettering exchange, queue, and bindings must be explicitly enabled or provisioned by the infrastructure. This rule does not concern the internal dead-letter exchange required by the TTL fallback for delayed messages. Cluster policies remain preferably infrastructure-managed.

## Laravel configuration

Configuration is split into four concepts:

- brokers: endpoints, vhosts, TLS, and authentication;
- routes: destinations used for publishing;
- topologies: exchanges, queues, bindings, and delays;
- workers: sets of subscriptions and scheduling policy.

Conceptual example:

    return [
        'brokers' => [
            'orders_eu' => [
                'hosts' => ['rabbit-1:5672', 'rabbit-2:5672'],
                'vhost' => '/orders-eu',
            ],
        ],

        'routes' => [
            'orders' => [
                'broker' => 'orders_eu',
                'exchange' => 'laravel.jobs',
                'routing_key' => '{queue}',
            ],
        ],

        'workers' => [
            'main' => [
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 64,
                ],
                'subscriptions' => [
                    'orders_high' => [
                        'broker' => 'orders_eu',
                        'queue' => 'orders.high',
                        'weight' => 8,
                        'prefetch' => ['mode' => 'fixed', 'value' => 8],
                    ],
                ],
            ],
        ],
    ];

Sane initial values are:

- confirms and mandatory enabled;
- durable quorum queue;
- delivery limit of 20 unless an external policy exists;
- no application DLQ without explicit configuration;
- publisher buffer bounded to 8192 commands;
- initial prefetch of 16, bounded by max_in_flight;
- reconnection from 100 ms to 30 s, multiplier 2, and 20% jitter.

Prefetch values must be calibrated by benchmark before the stable V1.

## Laravel compatibility

The package registers a rabbit-rs driver via Queue::extend. It implements the Queue, ClearableQueue, and Monitor contracts where relevant.

RabbitMqQueue implements push, pushRaw, later, bulk, pop, size, and clear. RabbitMqJob implements delete, release, attempts, getJobId, and getRawBody.

To keep queue:work without replacing Worker, the connection's queue value normally represents an aggregated profile. Advanced subscription selection and multiprocess mode will be provided by rabbit-rs:work in the second milestone.

The native Laravel events JobQueued, JobProcessing, JobProcessed, JobFailed, and JobExceptionOccurred remain emitted by the framework.

## Octane and FPM

The package keeps no reference to Application, Request, or Config in persistent singletons. It normalizes configuration into immutable values before creating the native handle.

Octane hooks cleanly close resources on worker shutdown or reload. A completed request does not tear down the native pool. Confirmations already awaited keep a bounded deadline.

The registry detects forks, including those occurring after accidental handle initialization.

## Observability

The core exposes a snapshot without imposing a backend:

- connection states and generation;
- open, borrowed, and invalidated channels;
- publisher commands in buffer;
- ACK/NACK/timeout confirmations;
- messages returned as unroutable;
- ready and unacknowledged deliveries;
- ACK, reject, release, and redeliveries;
- reconnection attempts and duration;
- publish, confirm, wait, and processing latencies;
- theoretical weight and effective distribution per subscription.

The Laravel package turns this data into events and can provide Prometheus or OpenTelemetry adapters later. Logs are structured and never contain a password, full URI, or private certificate.

## Validation

Four levels of testing are required:

1. deterministic Rust unit tests;
2. PHPT and Laravel package tests;
3. integration tests on a RabbitMQ cluster;
4. chaos tests and benchmarks.

The main property is: no silent loss in at-least-once scenarios; duplicates are permitted, identified, and measured.

The repository contains three labs:

- benchmarks/native: Rust, Lapin, batching, confirms, and FFI cost;
- benchmarks/laravel: Laravel application with the native extension, php-amqplib, the existing Laravel RabbitMQ driver, Redis, and a database control;
- lab/rabbitmq: a three-node RabbitMQ 4.3 cluster, metrics, and fault injection.

Reference payloads are 256 B, 1 KiB, 10 KiB, 100 KiB, and 1 MiB. Metrics are throughput, p50/p95/p99, CPU per message, RSS, connections, channels, recovery time, losses, duplicates, and fairness error.

Absolute targets are calibrated on a reference machine after the prototype, then recorded alongside comparative gains as anti-regression budgets.

## Distribution

Distribution optimizes user simplicity and cleanly separates the system binary from Laravel code.

### Native extension

The main repository is registered on Packagist as a goopil/rabbit-rs-native package of type php-ext. Its root composer.json declares extension-name = rabbit_rs, Linux only, NTS and ZTS support, and download-url-method = pre-packaged-binary.

The public installation is:

    pie install goopil/rabbit-rs-native

PIE replaces PECL as the primary channel. It selects the right binary according to PHP version, architecture, libc, and NTS/ZTS mode, installs the shared file, and enables the extension in the right PHP configuration.

CI produces 16 release archives:

- PHP 8.4 and 8.5;
- x86_64 and ARM64;
- glibc and musl;
- NTS and ZTS.

Debug builds are not distributed. Each archive follows the PIE naming convention exactly, for example:

    php_rabbit_rs-1.2.0_php8.5-x86_64-linux-glibc-nts.zip

Rust and TLS dependencies are statically linked as much as possible; libc remains the only expected system dependency. glibc builds use a documented, sufficiently old baseline. Each archive is tested with the target PHP, accompanied by a SHA-256, an SBOM, and a GitHub provenance attestation.

Building from source remains documented for contributors with Cargo and cargo-php, but it is not the V1 PIE fallback. No PECL package, privileged Composer installer, Debian/RPM/APK package, or full PHP image is maintained in V1. User Dockerfiles install the extension with PIE.

### Laravel package

The packages/laravel-queue package is published on Packagist as goopil/rabbit-rs-laravel. Its installation is:

    composer require goopil/rabbit-rs-laravel

It requires PHP ^8.4, Laravel 12 or 13, and ext-rabbit_rs with the same major version. Composer checks for the extension's presence but never attempts to install or enable a system binary.

The monorepo remains the development source. A subtree split CI publishes packages/laravel-queue to a read-only mirror repository, then pushes the same tag as the extension. The native GitHub release is only published after all binaries are produced and validated, the Laravel mirror tag is pushed, and both Packagist metadata are verified.

The stable V1 is only released after certification of CLI, FPM, and the four announced Octane servers.

## Planned evolutions

- adaptive prefetch based on EWMA, target buffer time, hysteresis, and memory pressure;
- multiprocess rabbit-rs:work command;
- Prometheus and OpenTelemetry exporters;
- additional routing and failover strategies;
- possible alternative AMQP backend if benchmarks justify it;
- RabbitMQ Streams support in a distinct product if a real need appears.

## Technical sources

- PHP Supported Versions: https://www.php.net/supported-versions.php
- Laravel Queue Worker: https://github.com/laravel/framework/blob/13.x/src/Illuminate/Queue/Worker.php
- Laravel Octane: https://laravel.com/docs/13.x/octane
- RabbitMQ Consumer Acknowledgements and Publisher Confirms: https://www.rabbitmq.com/docs/confirms
- RabbitMQ Quorum Queues: https://www.rabbitmq.com/docs/quorum-queues
- RabbitMQ Release Information: https://www.rabbitmq.com/release-information
- Lapin: https://github.com/amqp-rs/lapin
- ext-php-rs: https://github.com/davidcole1340/ext-php-rs
- PIE: https://github.com/php/pie
- Composer Platform Packages: https://getcomposer.org/doc/01-basic-usage.md#platform-packages
