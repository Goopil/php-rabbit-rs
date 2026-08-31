# Design — Laravel configuration: connection-first

- **Date:** 2026-08-31
- **Status:** approved (brainstorm session with owner); awaiting implementation plan
- **Source:** 2026-08-31 technical audit (`docs/audits/2026-08-31-technical-audit.md`, F-27, F-28, F-31) + competitor review (vyuldashev/laravel-queue-rabbitmq, iamfarhad/LaravelRabbitMQ)
- **Absorbs:** #80 (config lifecycle: boot blast radius + Octane stale normalization)
- **Aligns with:** #78 (dead knobs removed from the published surface happen here, on the Laravel side)
- **Track:** Round H (DX) — scheduled after Round G stabilization, in its own wave (Track D files)

## Motivation

The driver has two config homes. `config/queue.php` connections carry only
`driver`/`queue`/`block_for`; everything real lives in a 429-line
`config/rabbit-rs.php` with three linked namespaces (`brokers.*` → `routes.*` →
`workers.*.subscriptions.*`) that the user must keep consistent by hand.
Adding a broker means wiring three cross-referencing blocks. Native Laravel
drivers (SQS, redis) put everything in `queue.connections.*` — that is where
users look first. Normalization also runs at `register()`/`boot()` with strict
throws: one `env()`-style string value crashes the whole app at boot even when
the driver is never used (audit F-27), and the frozen normalized config makes
`octane:reload` keep stale brokers (F-28).

## Goals

- One config home: `queue.connections.*` — the SQS/redis idiom.
- Multi-broker stays first-class: **one broker/vhost = one connection**; more
  brokers = more connections (the framework's own way of expressing multiple
  backends).
- Lazy, per-connection normalization at `connect()` — no boot blast radius,
  Octane reload picks up fresh config.
- Accept Laravel env strings for scalars; cast inside the driver (env() returns
  strings for numbers — the pain both competitors documented).
- The connection is also the worker profile and the route.
- No core or extension changes: the compiler emits exactly today's native
  config shape, so pool fingerprinting, at-least-once semantics, and the
  extension boundary are untouched.

## Non-goals

- No compatibility shim for the old `rabbit-rs.php` shape (0.x break;
  documented in the CHANGELOG as v0.1.0).
- No pool-size knobs, AMQP transactions, failed-job dual-sink, or
  poll/consume modes (considered from competitor surfaces; rejected — the
  native core manages these better or they conflict with the contract).
- No multi-vhost inside one connection (use two connections).

## 1. Primary surface: `queue.connections.*`

```php
'orders' => [
    'driver'   => 'rabbit-rs',
    'queue'    => env('RABBIT_RS_QUEUE', 'default'),

    // Connection — one connection = one broker/vhost = one native pool
    'hosts'    => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
    'vhost'    => env('RABBIT_RS_VHOST', '/'),
    'username' => env('RABBIT_RS_USERNAME', 'guest'),
    'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    'connection_name' => 'orders',          // broker-side label (management UI, rabbitmqctl)
    'heartbeat' => 30,                       // seconds, >= 1
    'tls'      => ['enabled' => false, 'ca_cert' => null, 'client_cert' => null, 'client_key' => null],

    // Publication
    'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'), // null = default exchange
    'routing_key' => '{queue}',              // {queue} placeholder; null = exchange default
    'safety'   => env('RABBIT_RS_SAFETY', 'safe'), // safe | unsafe | blind
    'confirm_timeout' => 30000,              // ms, >= 1000
    'after_commit' => false,

    // Consumption
    'prefetch' => 64,                        // per consumer channel = per worker process
    'wait_timeout' => 30000,                 // ms, 1000..86400000
    'block_for' => null,                     // seconds
    'best_effort' => false,                  // gates early_ack/no_ack of this connection

    // Topology (defaults from package config)
    'topology_mode' => 'declare',            // declare | verify | external
    'queue_type' => 'quorum',                // quorum | classic
    'queue_durable' => true,
    'delivery_limit' => null,
    'dead_letter' => null,                   // ['exchange' =>, 'queue' =>, 'routing_key' =>]

    // Delay
    'delay' => ['mode' => 'auto', 'buckets' => [1, 5, 30, 120], 'queue_expiry_margin' => 60],

    // Misc
    'worker' => 'default',                   // default | horizon
    'auto_subscribe' => true,
    'production_warning' => true,

    // Optional advanced escape hatch — replaces the derived single subscription.
    // Same fields as today's worker subscriptions, minus `broker` (the
    // connection IS the broker): queue, weight, priority_class, prefetch,
    // starvation_after, early_ack, no_ack.
    // 'subscriptions' => ['critical' => ['queue' => 'critical', 'weight' => 4, 'priority_class' => -5]],
],
```

Key semantics:

- `hosts` accepts a flat comma-separated string (`"a:5672,b:5672"`, canonical,
  env-friendly) or an array of such strings. The existing endpoint parser
  (IPv6 brackets, port validation) is reused.
- `exchange: null` publishes through the default exchange (direct-to-queue),
  same as today's routes.
- `routing_key: '{queue}'` replaces the queue name at publish time; `null`
  means "no routing key" (default-exchange and fanout usage).
- `prefetch` applies per consumer channel — i.e. per `queue:work` process.
  N concurrent workers × prefetch = total in-flight. This is standard AMQP
  behavior and must be documented.

## 2. Package defaults: `config/rabbit-rs.php` (~40 lines)

The published file shrinks to cross-cutting defaults merged under every
rabbit-rs connection: `heartbeat`, `tls`, `safety`, `confirm_timeout`,
`prefetch`, `wait_timeout`, `topology_mode`, `queue_type`, `queue_durable`,
`delivery_limit`, `dead_letter`, `delay`, `worker`, `auto_subscribe`,
`production_warning`, `best_effort`.

Merge rule: per top-level key, the connection value wins; for the known nested
sections (`tls`, `delay`, `dead_letter`) the merge is per sub-key (a
connection that sets only `delay.mode` inherits package `buckets`). Any other
key in the defaults file is passed through for connections that do not define
it, and ignored for connections that define their own.

## 3. The compiler replaces the normalizer

- New class `ConnectionCompiler` replaces `ConfigNormalizer` (deleted; 0.x).
- Entry point: `ConnectionCompiler::compile(string $connection, array $config, array $defaults): array`
  where `$config` is the raw `queue.php` connection array. Output is exactly
  today's normalized native shape: one broker entry, one route
  (exchange/routing_key), one worker profile named after the connection with
  either the derived single subscription or the `subscriptions` escape hatch.
- Called lazily inside `RabbitMqConnector::connect($config)` — per connection,
  per process, on first resolution. `QueueManager` caches resolved
  connections; `octane:reload` flushes them, so the next resolution re-merges
  current config. F-27 and F-28 are closed by construction.
- `RabbitMqServiceProvider` simplifies: the `rabbit-rs.config` normalized
  singleton is deleted; the `queue->extend()` closure captures only the
  defaults array and the pool factory. `register()` no longer normalizes
  anything at boot.

## 4. Env-string handling

Inside the compiler (documented at the top of the published config like
iamfarhad does):

- Booleans: `true`, `"1"`, `"true"`, `"on"`, `"yes"` / `false`, `"0"`,
  `"false"`, `"off"`, `"no"`, `""`, `null`→default. Anything else: typed error.
- Integers: `/^-?\d+$/` strings accepted (then range-checked by the existing
  bound rules); anything else: typed error.
- The published config file itself uses plain `env(...)` calls without casts.

## 5. Worker profiles, `rabbit-rs:work`, multi-consumer

- Worker profile = connection name. `WorkerProfileResolver` maps connection
  names to profiles; auto-profiles (`__auto__.<queue>`) are unchanged for
  plain queue names.
- `rabbit-rs:work --connection=orders` — default: the default queue connection
  when it is a rabbit-rs connection; otherwise the command requires an explicit
  `--connection` and errors with the list of available rabbit-rs connections. `--connection=a,b` synthesizes one work
  profile spanning both connections' subscriptions (the native profile model
  already supports multi-broker subscriptions). `--queue=a,b` filters
  subscriptions within the selected connections (auto_subscribe fills gaps).
- Multi-consumer via standard `queue:work`: N processes, each with its own
  process-local native pool and channels. Consumer tags are per-channel
  (`rabbit-rs.<subscription>`) — AMQP requires uniqueness per channel only, so
  concurrent processes cannot collide (verified at `consumer/set.rs:187`).
  Nothing in the config may encode per-instance state; this design adds none.
- Improvement taken from iamfarhad: `connection_name` labels the AMQP
  connection in the management UI (`rabbitmqctl list_connections`) — default
  `"{connection}"`; the core already threads a connection name through
  `ConnectionProperties` (`transport/lapin.rs:37`).

## 6. Dropped from the published surface

- The `brokers.*`, `routes.*`, `workers.*` namespaces (replaced by the
  connection) and the `WorkerProfileResolver`'s config-shape coupling.
- `scheduler.strategy` (single allowed value — dead knob).
- `prefetch.mode` (single allowed value; returns with Round E adaptive
  prefetch).
- `publisher.confirms` / `publisher.mandatory` legacy flags (`safety` is the
  only switch; matches #78 on the core side).

## 7. Error handling

- Errors keep today's shape and strictness: `InvalidArgumentException` with
  fully qualified paths, now `queue.connections.<name>.<key>` instead of
  `brokers.*`/`workers.*`.
- Strictness unchanged: unknown keys inside known sections rejected, ranges
  enforced, `dead_letter` required with `delivery_limit`, `no_ack` requires
  `early_ack` + `best_effort`.

## 8. Testing

- **Unit (Pest):** compiler — full connection, minimal connection (defaults
  apply), env strings (each accepted/rejected spelling), deep-merge of `tls`/
  `delay`/`dead_letter`, `subscriptions` escape hatch, unknown-key rejection,
  every range/enum violation with its exact path.
- **Feature:** service provider registers without the extension (existing
  assertion kept); connector resolves two connections → two distinct native
  configs; same-config connections fingerprint identically (pool reuse kept).
- **Feature (work command):** `--connection` defaults, comma list, `--queue`
  filtering, auto-subscribe interaction.
- **Integration (lab):** two connections → two brokers/vhosts publish+consume
  end-to-end; multi-process `queue:work` consumers on one queue.

## 9. Docs & release

- Rewrite `docs/configuration.md` around connections with runnable
  copy-paste examples: single broker, multi-broker, env strings, worker
  profile targeting; update README quickstart and `docs/octane.md` (reload
  semantics change).
- CHANGELOG v0.1.0 entry: breaking config format change, migration table
  (old key → new location).

## 10. Files touched (implementation sketch)

```
packages/laravel-queue/config/rabbit-rs.php            (rewrite, ~40 lines)
packages/laravel-queue/src/Config/ConfigNormalizer.php (delete)
packages/laravel-queue/src/Config/ConnectionCompiler.php (new)
packages/laravel-queue/src/Connectors/RabbitMqConnector.php (lazy compile)
packages/laravel-queue/src/RabbitMqServiceProvider.php (boot simplification)
packages/laravel-queue/src/Console/RabbitMqWorkCommand*.php (--connection)
packages/laravel-queue/src/Support/WorkerProfileResolver.php (profile = connection)
packages/laravel-queue/tests/** (unit/feature rewrites)
docs/configuration.md, docs/octane.md, README.md, CHANGELOG
```
