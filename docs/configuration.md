# Configuration

Rabbit RS is configured connection-first: every broker, its credentials, its
routes, and its consumer profile live on a single **queue connection** in
`config/queue.php`, exactly like Laravel's built-in `redis` and `sqs` drivers.

Two config homes:

| File | Role |
|------|------|
| `config/queue.php` → `queue.connections.*` | **The primary surface.** One connection = one broker/vhost = one native pool. |
| `config/rabbit-rs.php` | Cross-cutting **defaults** merged under every rabbit-rs connection (~50 lines). Publish with `php artisan vendor:publish --tag="rabbit-rs-config"`. |

Nothing is normalized at boot: each connection is compiled **lazily**, when the
queue manager first resolves it. A config typo only fails the driver's use —
never the whole application — and `octane:reload` picks up fresh config
automatically (see [Octane](octane.md#reload-worker-reload)).

## Single broker

Add one connection to `config/queue.php`:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => 'default',

        // Broker — one connection = one broker/vhost = one native pool
        'hosts' => '127.0.0.1:5672',
        'vhost' => '/',
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        'heartbeat' => 30,
        'tls' => [
            'enabled' => false,
            'ca_cert' => null,
            'client_cert' => null,
            'client_key' => null,
        ],

        // Publication
        'exchange' => 'laravel.jobs',   // null = default exchange (direct-to-queue)
        'routing_key' => '{queue}',     // {queue} placeholder; null = no routing key
        'safety' => 'safe',             // safe | unsafe | blind
        'confirm_timeout' => 30000,     // ms, >= 1000

        // Consumption
        'prefetch' => 64,               // per consumer channel = per worker process
        'wait_timeout' => 30000,        // ms, 1000..86400000

        // Topology (defaults inherited from config/rabbit-rs.php)
        'topology_mode' => 'declare',   // declare | verify | external
        'queue_type' => 'quorum',       // quorum | classic
        'queue_durable' => true,
        'delivery_limit' => null,
        'dead_letter' => null,

        // Framework keys
        'after_commit' => false,
        'block_for' => null,
    ],
],
```

Dispatch and consume:

```bash
php artisan queue:work rabbit-rs
# or the supervised fan-out command:
php artisan rabbit-rs:work
```

Every key above except `driver` and `queue` is optional — anything the
connection omits falls back to `config/rabbit-rs.php`
(see [Cross-cutting defaults](#cross-cutting-defaults)). The minimal
connection is therefore:

```php
'rabbit-rs' => [
    'driver' => 'rabbit-rs',
    'queue' => 'default',
],
```

## Connection reference

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `driver` | string | — | Must be `rabbit-rs` |
| `queue` | string | — (required) | Default queue name: the derived consumer subscription and the `pop()` target |
| `hosts` | string or string[] | `127.0.0.1:5672` | Comma-separated `host:port` list; IPv6 must be bracketed (`[::1]:5672`) |
| `vhost` | string | `/` | AMQP virtual host (a distinct vhost = a distinct AMQP connection) |
| `username` | string | `guest` | AMQP username |
| `password` | string | `guest` | AMQP password |
| `tls` | array | package defaults | `enabled`, `ca_cert`, `client_cert`, `client_key` (see [TLS](#tls)) |
| `heartbeat` | int (seconds) | `30` | AMQP heartbeat; positive integer |
| `management_url` | ?string | `null` | Laravel-only: RabbitMQ management API base URL for `rabbit-rs:status` (never sent to the native extension) |
| `exchange` | ?string | `laravel.jobs` | Publishing exchange; `null` publishes through the default exchange |
| `routing_key` | ?string | `{queue}` | `{queue}` is replaced with the queue name at publish time; `null` means no routing key (default-exchange/fanout usage) |
| `safety` | string | `safe` | `safe` (confirms + mandatory), `unsafe` (confirms only), `blind` (fire-and-forget) |
| `confirm_timeout` | int (ms) | `30000` | Publisher confirm timeout, minimum `1000`; during a recovery, a publish parked in replay is retried once with a fresh deadline, while a confirm timeout on a live connection stays terminal |
| `prefetch` | int | `64` | QoS prefetch per consumer channel, 1–65535 |
| `wait_timeout` | int (ms) | `30000` | Consumer acquisition deadline, 1000–86400000 |
| `max_attempts` | int | `20` | Inclusive cap on resolved delivery attempts before terminal settlement |
| `best_effort` | bool | `false` | Gates `early_ack`/`no_ack` on this connection's subscriptions |
| `auto_subscribe` | bool | `false` (package default) | Lets `pop()` resolve plain queue names via implicit profiles; an explicit `null` compiles to `true` |
| `topology_mode` | string | `declare` | `declare`, `verify`, `external` — see [Topology](topology.md) |
| `queue_type` | string | `quorum` | `quorum` or `classic` |
| `queue_durable` | bool | `true` | Queue durability |
| `delivery_limit` | ?int | `null` | Quorum-queue delivery limit; **requires `dead_letter`** when set |
| `dead_letter` | ?array | `null` | `['exchange' =>, 'queue' =>, 'routing_key' =>]` |
| `delay` | array | package defaults | `mode`, `buckets`, `max_buckets`, `queue_expiry_margin` (see [Delayed messages](#delayed-messages)) |
| `subscriptions` | array | — | Escape hatch replacing the derived subscription (see [Subscriptions escape hatch](#subscriptions-escape-hatch)) |
| `worker` | string | `default` | `default` or `horizon` (framework key) |
| `production_warning` | bool | `true` | Silences the unbounded-redelivery warning for this connection |
| `after_commit` | bool | `false` | Framework key: dispatch after the database transaction commits |
| `block_for` | ?int | `null` | Framework key: seconds to block for new jobs before returning |

`prefetch` applies **per consumer channel** — i.e. per `queue:work` process.
N concurrent workers × `prefetch` = total in-flight messages. This is standard
AMQP behavior; size your workers accordingly.

## Environment strings

Laravel's `env()` returns strings for numbers and flags from `.env`. The
compiler casts them lazily at connection resolution:

- **Booleans** accept `true`, `"1"`, `"true"`, `"on"`, `"yes"` / `false`,
  `"0"`, `"false"`, `"off"`, `"no"`, `""`; `null` falls back to the key's own
  fallback — for `auto_subscribe` that fallback is `true` (an explicit `null`
  enables auto-subscribe even though the package default is `false`).
- **Integers** accept signed digit strings (`"64"`, `"-1"`), then the existing
  range checks apply.
- Anything else, and any unknown key, throws `InvalidArgumentException` with
  the full config path, e.g. `queue.connections.orders.prefetch`.

The published `config/rabbit-rs.php` uses plain `env()` calls without casts —
put your env wiring wherever it reads best:

```php
// config/queue.php — everything via env
'rabbit-rs' => [
    'driver' => 'rabbit-rs',
    'queue' => env('RABBIT_RS_QUEUE', 'default'),
    'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
    'vhost' => env('RABBIT_RS_VHOST', '/'),
    'username' => env('RABBIT_RS_USERNAME', 'guest'),
    'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'),
    'max_attempts' => env('RABBIT_RS_MAX_ATTEMPTS', 20),
],
```

## Cross-cutting defaults

`config/rabbit-rs.php` holds only defaults that are merged under every
rabbit-rs connection. A key the connection omits is inherited from here — per
sub-key for the three nested sections (`tls`, `delay`, `dead_letter`), so a
connection that sets only `delay.mode` still inherits the package `buckets`.
A connection value — including an explicit `null` — always wins.

| Key | Env hook | Default |
|-----|----------|---------|
| `heartbeat` | `RABBIT_RS_HEARTBEAT` | `30` |
| `tls.enabled` | `RABBIT_RS_TLS` | `false` |
| `tls.ca_cert` | `RABBIT_RS_TLS_CA_CERT` | `null` |
| `tls.client_cert` | `RABBIT_RS_TLS_CLIENT_CERT` | `null` |
| `tls.client_key` | `RABBIT_RS_TLS_CLIENT_KEY` | `null` |
| `safety` | `RABBIT_RS_SAFETY` | `safe` |
| `confirm_timeout` | `RABBIT_RS_CONFIRM_TIMEOUT` | `30000` |
| `prefetch` | `RABBIT_RS_PREFETCH` | `64` |
| `wait_timeout` | `RABBIT_RS_CONSUMER_WAIT_TIMEOUT` | `30000` |
| `topology_mode` | `RABBIT_RS_TOPOLOGY_MODE` | `declare` |
| `delay.mode` | `RABBIT_RS_DELAY_MODE` | `auto` |
| `delay.buckets` | `RABBIT_RS_DELAY_BUCKETS` | `1,5,30,120` |
| `delay.max_buckets` | `RABBIT_RS_DELAY_MAX_BUCKETS` | `8` |
| `delay.queue_expiry_margin` | `RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN` | `60` |
| `worker` | `RABBIT_RS_WORKER` | `default` |
| `auto_subscribe` | `RABBIT_RS_AUTO_SUBSCRIBE` | `false` |
| `production_warning` | `RABBIT_RS_PRODUCTION_WARNING` | `true` |
| `best_effort` | `RABBIT_RS_BEST_EFFORT` | `false` |

Keys with no per-connection default wiring (`queue_type` = `quorum`,
`queue_durable` = `true`, `delivery_limit` = `null`, `dead_letter` = `null`)
are plain values in the file — set them per connection when you need to vary
them.

Connection-only keys have no entry in this file: `queue`, `hosts`, `vhost`,
`username`, `password`, `exchange`, `routing_key`, `subscriptions`,
`management_url`, `max_attempts`, and the framework keys (`after_commit`,
`block_for`). Wire them with `env()` directly on the connection, as shown
above.

## Multiple brokers and vhosts

A vhost owns a distinct AMQP connection. To consume from or publish to several
brokers or vhosts, define one connection per broker — more brokers is more
connections, the framework's own way of expressing multiple backends:

```php
'connections' => [
    'orders-eu' => [
        'driver' => 'rabbit-rs',
        'queue' => 'orders',
        'hosts' => ['rabbit-1:5672', 'rabbit-2:5672'],
        'vhost' => '/orders-eu',
        'username' => 'orders',
        'password' => env('ORDERS_PASSWORD'),
        'exchange' => 'laravel.jobs',
    ],

    'billing' => [
        'driver' => 'rabbit-rs',
        'queue' => 'invoices',
        'hosts' => 'rabbit-3:5672',
        'vhost' => '/billing',
        'username' => 'billing',
        'password' => env('BILLING_PASSWORD'),
        'tls' => [
            'enabled' => true,
            'ca_cert' => '/etc/ssl/certs/rabbit-ca.pem',
        ],
        'exchange' => 'billing.jobs',
    ],
],
```

Publish and consume per connection:

```php
BillingJob::dispatch($invoice)->onConnection('billing');
OrdersJob::dispatch($orderId)->onConnection('orders-eu');
```

```bash
# Consume every queue of every rabbit-rs connection (see Worker fan-out)
php artisan rabbit-rs:work

# Consume only the listed connections
php artisan rabbit-rs:work --connection=orders-eu,billing
```

`hosts` accepts a flat comma-separated string (env-friendly:
`"rabbit-1:5672,rabbit-2:5672"`) or an array of such strings. Endpoints are
sorted and Rabbit RS connects to the first reachable host.

### Composed consumer behavior

A worker subscribed to several queues on one connection gets one composed
consumer: deliveries fan in through a single `pop()` call under weighted-fair
scheduling, and each delivery's ACK/Release/Reject is routed back to its
source. Ordering is guaranteed only within a single queue. A broker that is
recovering does not stop consumption from other connections; when its consumer
set is replaced after recovery, the composed consumer surfaces a one-shot
`Goopil\RabbitRs\ConnectionException` ("broker source replaced by recovery;
re-fetch consumer") — re-fetch the consumer (e.g. `closeConsumers()` on the
queue connector) and the fresh handle re-subscribes without duplicating
subscriptions (see [Reliability — Connection recovery](reliability.md#connection-recovery)).

## Subscriptions escape hatch

By default a connection consumes exactly one queue: its `queue` key. Set
`subscriptions` to consume several queues on the same broker with per-subscription
tuning. The alias is the array key; the broker is always this connection
(a subscription cannot cross brokers — use a second connection for that):

```php
'orders-eu' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    'hosts' => 'rabbit-1:5672',
    'vhost' => '/orders-eu',
    'username' => 'orders',
    'password' => 'secret',

    'subscriptions' => [
        'critical' => [
            'queue' => 'orders.critical',
            'weight' => 8,
            'priority_class' => 1,
            'prefetch' => 8,
        ],
        'bulk' => [
            'queue' => 'orders.bulk',
            'weight' => 2,
            'prefetch' => 32,
            'starvation_after' => 60,
        ],
    ],
],
```

| Field | Default | Description |
|-------|---------|-------------|
| `queue` | — (required) | Broker queue to consume |
| `weight` | `1` | Delivery share vs other subscriptions (1–65535) |
| `priority_class` | `0` | Inter-queue priority (-32768..32767) |
| `prefetch` | connection `prefetch` | QoS prefetch for this subscription |
| `starvation_after` | `30` | Seconds before aging kicks in to prevent starvation |
| `early_ack` | `false` | Requires `best_effort` |
| `no_ack` | `false` | Requires `early_ack` **and** `best_effort` |

Without the escape hatch, one subscription named `default` is derived from the
connection's `queue`. With it, the list replaces the derivation. Rules:
at least one entry, unique queues across aliases, unknown fields rejected.

## Worker fan-out

`rabbit-rs:work` supervises one `queue:work` child **per targeted
connection** (each child gets that connection's whole queue set through the
native weighted-fair scheduler). Cross-connection fairness is process-level,
same as every Laravel driver.

| Flag | Default | Behavior |
|------|---------|----------|
| *(none)* | — | Every rabbit-rs connection's every defined queue (`queue` key first, then `subscriptions` queues) |
| `--connection=a,b` | all | Restrict to the listed connections (unknown → error listing available connections) |
| `--queue=x,y` | all defined | Resolve each name **by definition**: a connection's `queue` key or a `subscriptions` alias. Unknown → error listing all defined queues |
| `--workers=N` | `1` | Children spawned **per connection** (N connections × N workers total) |
| `--max-restarts`, `--backoff` | `3`, `1` | Supervisor crash-loop protection |
| `--timeout`, `--tries`, `--memory`, `--max-jobs`, `--max-time` | — | Propagated to each `queue:work` child |

```bash
php artisan rabbit-rs:work
# → Starting 2 worker(s): orders-eu[orders, orders.critical, orders.bulk], billing[invoices]

php artisan rabbit-rs:work --connection=billing
php artisan rabbit-rs:work --queue=critical
php artisan rabbit-rs:work --connection=orders-eu --queue=critical,bulk --workers=4
```

Semantics worth knowing:

- A queue name defined on two targeted connections is **consumed on both**
  (one child per connection).
- Combining `--connection` and `--queue` intersects both filters: only listed
  connections that define the listed queues run.
- Plain `php artisan queue:work rabbit-rs` still works for a
  single connection; multi-driver setups remain N processes, as with every
  Laravel driver.

## TLS

```php
'billing' => [
    'driver' => 'rabbit-rs',
    'queue' => 'invoices',
    'hosts' => 'rabbit-3:5672',
    'vhost' => '/billing',
    'username' => 'billing',
    'password' => env('BILLING_PASSWORD'),
    'tls' => [
        'enabled' => true,
        'ca_cert' => '/etc/ssl/certs/rabbit-ca.pem',
        'client_cert' => '/etc/ssl/certs/client.pem',   // optional, enables mTLS
        'client_key' => '/etc/ssl/private/client.key',
    ],
],
```

Or globally through the package defaults with `RABBIT_RS_TLS=true` and the
`RABBIT_RS_TLS_CA_CERT` / `RABBIT_RS_TLS_CLIENT_CERT` / `RABBIT_RS_TLS_CLIENT_KEY`
env hooks — a connection that omits a `tls` sub-key inherits it.

## Delayed messages

Delay configuration is per connection, with `mode`, `buckets`,
`max_buckets`, and `queue_expiry_margin`:

```php
'orders-eu' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    // ...
    'delay' => [
        'mode' => 'auto',      // auto | plugin | ttl
        'buckets' => [1, 5, 30, 120],
        'max_buckets' => 8,
        'queue_expiry_margin' => 60,
    ],
],
```

- `auto` — publish delayed messages through the `x-delayed-message` exchange (same as `plugin`); use `ttl` when the plugin is not installed
- `plugin` — require the plugin; fail if it is not installed
- `ttl` — always use TTL queue buckets

> **Note:** when `safety` is `blind`, delayed jobs are **not** honored — the
> blind pump bypasses delay routing and publishes immediately. Use `safe` or
> `unsafe` when you need delay routing.

See [Topology — Delay routing](topology.md#delay-routing) for details.

## Status monitoring

`php artisan rabbit-rs:status` prints native pool metrics per connection and,
when a connection defines `management_url`, cross-process queue counters
(`delivered`, `acked`, `redelivered`) fetched from the RabbitMQ management API
using the connection's `username`/`password` and `vhost` for basic auth:

```php
'orders-eu' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    'hosts' => 'rabbit-1:5672',
    'username' => 'orders',
    'password' => 'secret',
    'management_url' => 'http://rabbit-1:15672',
],
```

```bash
php artisan rabbit-rs:status
php artisan rabbit-rs:status --format=json
```

`management_url` is Laravel-only: it is validated on the connection but never
propagated to the native extension. `null` or blank disables the feature. See
[Reliability — Measuring duplicates](reliability.md#measuring-duplicates) for
what the counters mean.

## Safety modes

The `safety` setting selects the delivery guarantee level; publisher confirms
and mandatory routing are **derived from it**, never set independently:

- `safe` (default) — at-least-once: confirms + mandatory routing. Unconfirmed
  publications are retained in bounded process memory and replayed with their
  original `message_id` across connection recovery.
- `unsafe` — confirms without mandatory routing: the publish still waits for a
  broker ACK, but unroutable messages are silently dropped by the broker.
- `blind` — explicit fire-and-forget: publishing hands the message to a
  bounded background pump and returns without waiting for any transport
  outcome. A transport failure after the hand-off is a silent loss. Delayed
  jobs are not honored in this mode.

See [Reliability](reliability.md) for the full contract.

## Validation and strict errors

Every config problem throws `InvalidArgumentException` with the exact path,
and only when the affected connection is resolved:

- Unknown keys — on the connection or inside `tls`, `delay`, `dead_letter`,
  `subscriptions` — are rejected (`queue.connections.<name>.<key>: unknown key`).
- `hosts` must contain at least one non-empty `host:port` entry; empty
  segments (e.g. `"host1:5672,,host2"`) are rejected. Ports must be 1–65535.
- `safety` must be `safe`, `unsafe`, or `blind`; `confirm_timeout` ≥ 1000;
  `wait_timeout` 1000–86400000; `prefetch` and `weight` 1–65535;
  `heartbeat`, `starvation_after`, `max_attempts`, and delay buckets are
  positive integers.
- `dead_letter` is **required** when `delivery_limit` is set — without a DLX,
  poison messages are silently dropped after the limit is reached.
- `early_ack` requires `best_effort`; `no_ack` requires `early_ack` and
  `best_effort`.
- `subscriptions` must contain at least one entry, with unique queues across
  aliases.

## Pool reuse and fingerprints

The compiled connection feeds the native pool fingerprint. Two connections
with **identical arrays** compile identically and share the same native pool
within a PHP process (each keeps its own name and callbacks). A different
vhost, host, or credential always produces a different fingerprint — each
vhost gets its own AMQP connection.

## Fork safety

The runtime registry is process-local. After a fork (e.g., `pcntl_fork()`),
the child process detects the PID change and invalidates all inherited
handles. The child creates fresh connections lazily on first use. This is
transparent to the application.
