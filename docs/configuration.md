# Configuration

Rabbit RS is configured via `config/rabbit-rs.php` in your Laravel application. Publish it with:

```bash
php artisan vendor:publish --tag="rabbit-rs-config"
```

## Structure

The configuration has six sections:

| Section | Purpose |
|---------|---------|
| `topology_mode` | How Rabbit RS interacts with broker topology |
| `brokers` | Connection endpoints, vhosts, TLS, credentials |
| `routes` | Publishing destinations (exchange + routing key) |
| `workers` | Consumer profiles with subscriptions and scheduler |
| `publisher` | Publisher confirms, mandatory routing, timeouts |
| `delay` | Delayed message routing (plugin or TTL fallback) |
| `topology` | Queue type, durability, delivery limits, DLQ |

## Environment variables

All settings can be overridden via environment variables. Below is the complete reference.

### Topology mode

```php
'topology_mode' => env('RABBIT_RS_TOPOLOGY_MODE', 'declare'),
```

| Env | Values | Default |
|-----|--------|---------|
| `RABBIT_RS_TOPOLOGY_MODE` | `declare`, `verify`, `external` | `declare` |

See [Topology](topology.md) for mode semantics.

### Brokers

```php
'brokers' => [
    'default' => [
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'vhost' => env('RABBIT_RS_VHOST', '/'),
        'credentials' => [
            'username' => env('RABBIT_RS_USERNAME', 'guest'),
            'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        ],
        'tls' => [
            'enabled' => (bool) env('RABBIT_RS_TLS', false),
            'server_name' => env('RABBIT_RS_TLS_SERVER_NAME'),
            'ca_cert' => env('RABBIT_RS_TLS_CA_CERT'),
            'client_cert' => env('RABBIT_RS_TLS_CLIENT_CERT'),
            'client_key' => env('RABBIT_RS_TLS_CLIENT_KEY'),
            'verify' => env('RABBIT_RS_TLS_VERIFY', 'peer'),
        ],
        'heartbeat' => (int) env('RABBIT_RS_HEARTBEAT', 30),
    ],
],
```

| Env | Description | Default |
|-----|-------------|---------|
| `RABBIT_RS_HOSTS` | Comma-separated `host:port` list | `127.0.0.1:5672` |
| `RABBIT_RS_VHOST` | AMQP vhost | `/` |
| `RABBIT_RS_USERNAME` | AMQP username | `guest` |
| `RABBIT_RS_PASSWORD` | AMQP password | `guest` |
| `RABBIT_RS_TLS` | Enable TLS | `false` |
| `RABBIT_RS_TLS_SERVER_NAME` | SNI server name override | `null` |
| `RABBIT_RS_TLS_CA_CERT` | Path to CA certificate PEM | `null` |
| `RABBIT_RS_TLS_CLIENT_CERT` | Path to client certificate | `null` |
| `RABBIT_RS_TLS_CLIENT_KEY` | Path to client private key | `null` |
| `RABBIT_RS_TLS_VERIFY` | `peer` or `none` | `peer` |
| `RABBIT_RS_HEARTBEAT` | Heartbeat interval in seconds | `30` |

#### Multiple hosts

Multiple hosts are comma-separated. Rabbit RS connects to the first reachable host:

```bash
RABBIT_RS_HOSTS=rabbit-1:5672,rabbit-2:5672,rabbit-3:5672
```

#### TLS configuration

Enable TLS by setting `RABBIT_RS_TLS=true`. The scheme switches to `amqps://`. If `server_name` is not set, the first host is used for SNI.

```php
'tls' => [
    'enabled' => true,
    'server_name' => 'rabbit.example.com',
    'ca_cert' => '/etc/ssl/certs/rabbit-ca.pem',
    'client_cert' => '/etc/ssl/certs/client.pem',
    'client_key' => '/etc/ssl/private/client.key',
    'verify' => 'peer', // or 'none' to skip verification (not recommended)
],
```

### Routes

Routes define publishing destinations. Each route maps a logical name to a broker, exchange, and routing key:

```php
'routes' => [
    'default' => [
        'broker' => 'default',
        'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'),
        'routing_key' => '{queue}',
    ],
],
```

| Env | Description | Default |
|-----|-------------|---------|
| `RABBIT_RS_EXCHANGE` | Default exchange name | `laravel.jobs` |

The `{queue}` placeholder in the routing key is replaced with the queue name at publish time. For example, publishing to queue `orders` with routing key `{queue}` uses `orders` as the routing key.

### Workers

Workers define consumer profiles with subscriptions and a scheduler:

```php
'workers' => [
    'default' => [
        'scheduler' => [
            'strategy' => 'weighted_fair',
            'max_in_flight' => (int) env('RABBIT_RS_MAX_IN_FLIGHT', 64),
        ],
        'subscriptions' => [
            'default' => [
                'enabled' => true,
                'broker' => 'default',
                'queue' => env('RABBIT_RS_QUEUE', 'default'),
                'weight' => 1,
                'priority_class' => 0,
                'prefetch' => [
                    'mode' => 'fixed',
                    'value' => (int) env('RABBIT_RS_PREFETCH', 16),
                ],
                'starvation_after' => 30,
            ],
        ],
    ],
],
```

| Env | Description | Default |
|-----|-------------|---------|
| `RABBIT_RS_MAX_IN_FLIGHT` | Global in-flight message budget | `64` |
| `RABBIT_RS_QUEUE` | Default subscription queue name | `default` |
| `RABBIT_RS_PREFETCH` | Prefetch count per subscription | `16` |

#### Scheduler

The scheduler uses a deficit weighted round-robin (`weighted_fair`) algorithm with starvation prevention. `max_in_flight` caps the total number of unacknowledged messages across all subscriptions in the profile. Each subscription's prefetch must not exceed `max_in_flight`.

#### Subscriptions

Each subscription references a broker and queue. The `weight` controls the share of deliveries relative to other subscriptions. The `priority_class` enables inter-queue priority. The `starvation_after` setting (in seconds) activates aging to prevent starvation of low-weight subscriptions.

Set `enabled => false` to exclude a subscription without removing it from config.

### Publisher

```php
'publisher' => [
    'confirms' => true,
    'mandatory' => true,
    'confirm_timeout' => (int) env('RABBIT_RS_CONFIRM_TIMEOUT', 30000),
],
```

| Env | Description | Default |
|-----|-------------|---------|
| `RABBIT_RS_CONFIRM_TIMEOUT` | Publisher confirm timeout in ms | `30000` |

Publisher confirms and mandatory routing are **enabled by default**. This provides at-least-once delivery guarantees. Disabling either is possible but removes safety guarantees — see [Reliability](reliability.md).

- `confirms: true` — the publisher waits for a broker ACK before resolving the publish call
- `mandatory: true` — the broker returns unroutable messages instead of silently dropping them
- `confirm_timeout` — how long to wait for a confirm before timing out (milliseconds)

### Delay

```php
'delay' => [
    'mode' => env('RABBIT_RS_DELAY_MODE', 'auto'),
    'buckets' => array_map('intval', array_filter(array_map('trim', explode(',', env('RABBIT_RS_DELAY_BUCKETS', '1,5,30,120'))))),
    'max_buckets' => (int) env('RABBIT_RS_DELAY_MAX_BUCKETS', 8),
    'queue_expiry_margin' => (int) env('RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN', 60),
    'detection_timeout' => (int) env('RABBIT_RS_DELAY_DETECTION_TIMEOUT', 5),
],
```

| Env | Description | Default |
|-----|-------------|---------|
| `RABBIT_RS_DELAY_MODE` | `auto`, `plugin`, `ttl` | `auto` |
| `RABBIT_RS_DELAY_BUCKETS` | Comma-separated TTL bucket seconds | `1,5,30,120` |
| `RABBIT_RS_DELAY_MAX_BUCKETS` | Maximum number of TTL buckets | `8` |
| `RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN` | Queue expiry margin in seconds | `60` |
| `RABBIT_RS_DELAY_DETECTION_TIMEOUT` | Plugin detection timeout in seconds | `5` |

- `auto` — use the `rabbitmq_delayed_message_exchange` plugin if available, otherwise fall back to TTL queues
- `plugin` — require the plugin; fail if it is not installed
- `ttl` — always use TTL queue buckets

See [Topology — Delay routing](topology.md#delay-routing) for details.

### Topology

```php
'topology' => [
    'queue' => [
        'type' => 'quorum',
        'durable' => true,
        'delivery_limit' => 20,
    ],
    'dead_letter' => null,
],
```

| Setting | Description | Default |
|---------|-------------|---------|
| `queue.type` | `quorum` or `classic` | `quorum` |
| `queue.durable` | Queue durability | `true` |
| `queue.delivery_limit` | Max delivery attempts before dead-letter | `20` |
| `dead_letter` | DLQ configuration or `null` | `null` |

By default, no DLQ is created. To enable a DLQ:

```php
'topology' => [
    'queue' => [
        'type' => 'quorum',
        'durable' => true,
        'delivery_limit' => 20,
    ],
    'dead_letter' => [
        'exchange' => 'laravel.jobs.dlx',
        'queue' => 'laravel.jobs.dead',
        'routing_key' => 'dead', // or null to use the queue name
    ],
],
```

See [Topology — DLQ](topology.md#dlq-configuration) for details.

## Multiple brokers and vhosts

A vhost owns a distinct AMQP connection. To consume from multiple vhosts, define multiple brokers:

```php
'brokers' => [
    'orders_eu' => [
        'hosts' => ['rabbit-1:5672', 'rabbit-2:5672'],
        'vhost' => '/orders-eu',
        'credentials' => ['username' => 'orders', 'password' => 'secret'],
        'tls' => ['enabled' => false],
        'heartbeat' => 30,
    ],
    'billing' => [
        'hosts' => ['rabbit-3:5672'],
        'vhost' => '/billing',
        'credentials' => ['username' => 'billing', 'password' => 'secret'],
        'tls' => [
            'enabled' => true,
            'server_name' => 'rabbit.example.com',
            'ca_cert' => '/etc/ssl/certs/rabbit-ca.pem',
        ],
        'heartbeat' => 30,
    ],
],

'routes' => [
    'orders' => [
        'broker' => 'orders_eu',
        'exchange' => 'laravel.jobs',
        'routing_key' => '{queue}',
    ],
    'invoices' => [
        'broker' => 'billing',
        'exchange' => 'billing.jobs',
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
                'enabled' => true,
                'broker' => 'orders_eu',
                'queue' => 'orders.high',
                'weight' => 8,
                'priority_class' => 1,
                'prefetch' => ['mode' => 'fixed', 'value' => 8],
                'starvation_after' => 30,
            ],
            'orders_low' => [
                'enabled' => true,
                'broker' => 'orders_eu',
                'queue' => 'orders.low',
                'weight' => 2,
                'priority_class' => 0,
                'prefetch' => ['mode' => 'fixed', 'value' => 16],
                'starvation_after' => 30,
            ],
            'invoices' => [
                'enabled' => true,
                'broker' => 'billing',
                'queue' => 'invoices',
                'weight' => 4,
                'priority_class' => 0,
                'prefetch' => ['mode' => 'fixed', 'value' => 16],
                'starvation_after' => 30,
            ],
        ],
    ],
],
```

This configures a single worker profile (`main`) consuming from three queues across two brokers and vhosts, with weighted-fair scheduling.

## Connection key

Rabbit RS uses a connection key to determine pool reuse. The key includes:

- Hosts and selection strategy
- Port and TLS parameters
- Credentials and auth mechanism
- Vhost
- Heartbeat, timeouts, and AMQP parameters
- Configuration fingerprint

Two connections with the same key share the same native pool within a PHP process. A different vhost always produces a different key — each vhost gets its own AMQP connection.

## Fork safety

The runtime registry is process-local. After a fork (e.g., `pcntl_fork()`), the child process detects the PID change and invalidates all inherited handles. The child creates fresh connections lazily on first use. This is transparent to the application.
