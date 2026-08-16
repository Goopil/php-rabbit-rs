# Topology

Rabbit RS manages RabbitMQ topology through three modes. The mode is set via `topology_mode` in `config/rabbit-rs.php`.

## Topology modes

### declare (default)

Rabbit RS declares all exchanges, queues, and bindings idempotently. If the existing topology is incompatible (e.g., a queue exists with different arguments), the declaration fails with a permanent error.

Use `declare` when Rabbit RS owns the topology and you want it created automatically:

```php
'topology_mode' => 'declare',
```

### verify

Rabbit RS performs passive declarations to verify that the expected topology exists with the correct properties. No resources are created. If a declaration or property mismatch is detected, the connection fails with a permanent error.

Use `verify` when an external system (Terraform, Puppet, management CLI) provisions the topology and you want to catch drift:

```php
'topology_mode' => 'verify',
```

### external

Rabbit RS uses the topology without declaring or verifying anything. No AMQP declaration commands are sent. The broker must already have the correct exchanges, queues, and bindings.

Use `external` when you trust the infrastructure and want to avoid any declaration overhead:

```php
'topology_mode' => 'external',
```

## Queue types

### Quorum queues (default)

Quorum queues are the default and recommended queue type. They provide replicated, durable message storage with Raft consensus:

```php
'topology' => [
    'queue' => [
        'type' => 'quorum',
        'durable' => true,
        'delivery_limit' => 20,
    ],
],
```

Quorum queues support:
- `delivery_limit` — max delivery attempts before dead-lettering (emitted as `x-delivery-limit`)
- Automatic replication across cluster nodes
- Crash recovery without message loss

### Classic queues

Classic queues are non-replicated and suitable for workloads where durability is less critical:

```php
'topology' => [
    'queue' => [
        'type' => 'classic',
        'durable' => true,
        'delivery_limit' => 20,
    ],
],
```

> Classic queues do not support `x-delivery-limit` in all RabbitMQ versions. The `delivery_limit` setting is emitted as a queue argument regardless, but only quorum queues enforce it.

## Exchange and queue declaration

In `declare` mode, Rabbit RS declares:

1. **Exchanges** — the exchange from each route configuration
2. **Queues** — the queue from each subscription
3. **Bindings** — queue-to-exchange bindings using the routing key

The exchange type defaults to `direct` (matching the `{queue}` routing key pattern). Queues are declared as durable, non-exclusive, and non-auto-delete.

### Recovery order

After a connection recovery, topology is reconciled in deterministic order:

1. Connection and negotiation
2. Channels
3. Exchanges
4. Queues
5. Bindings
6. QoS (prefetch)
7. Consumers
8. Publisher replay (unconfirmed publications)

This order ensures that consumers are only re-registered after their queues and bindings exist.

## DLQ configuration

By default, Rabbit RS does **not** create a dead-letter queue. Dead-lettering must be enabled explicitly:

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
        'routing_key' => 'dead',
    ],
],
```

When `dead_letter` is non-null, Rabbit RS:

1. Declares the dead-letter exchange (`laravel.jobs.dlx`)
2. Declares the dead-letter queue (`laravel.jobs.dead`)
3. Binds the DLQ to the DLX with the specified routing key (`dead`)
4. Sets `x-dead-letter-exchange` and `x-dead-letter-routing-key` on the main queue

The `routing_key` is optional. If set to `null`, the queue name is used as the routing key.

### How dead-lettering works

When a message exceeds `delivery_limit` (default: 20), RabbitMQ dead-letters it to the configured exchange. The message arrives in the DLQ with `x-death` headers recording the original queue, reason, and count.

> **Note:** The `delivery_limit` is enforced by quorum queues. Classic queues rely on application-level attempt tracking.

## Delay routing

Rabbit RS supports delayed message delivery via two strategies, selected by the `delay.mode` setting.

### Auto (default)

```php
'delay' => [
    'mode' => 'auto',
    'buckets' => [1, 5, 30, 120],
    'max_buckets' => 8,
    'queue_expiry_margin' => 60,
    'detection_timeout' => 5,
],
```

In `auto` mode, Rabbit RS detects whether the `rabbitmq_delayed_message_exchange` plugin is installed:

1. **Plugin available** → uses `x-delayed-message` exchange type
2. **Plugin absent** → falls back to TTL queue buckets

The detection is bounded by `detection_timeout` (seconds) and cached per connection generation.

### Plugin mode

```php
'delay' => [
    'mode' => 'plugin',
],
```

Requires the `rabbitmq_delayed_message_exchange` plugin. Rabbit RS declares an `x-delayed-message` exchange with the underlying exchange type (e.g., `direct`) and publishes delayed messages with the `x-delay` header.

Install the plugin:

```bash
# On RabbitMQ server
rabbitmq-plugins enable rabbitmq_delayed_message_exchange
```

If the plugin is not installed, `plugin` mode fails with a permanent error.

### TTL fallback mode

```php
'delay' => [
    'mode' => 'ttl',
    'buckets' => [1, 5, 30, 120],
    'max_buckets' => 8,
    'queue_expiry_margin' => 60,
],
```

TTL mode uses a set of bounded delay queues with `x-message-ttl` and dead-letter exchange configurations. Messages are routed to the appropriate bucket based on the requested delay:

- Delays are rounded **up** to the nearest bucket to ensure a job is never delivered before its deadline
- Each bucket has a TTL queue with `x-dead-letter-exchange` pointing back to the original destination
- TTL queues are declared lazily on first use and have a queue expiry (`x-expires`) to avoid unbounded topology growth
- The queue expiry is set to `max_bucket_delay + queue_expiry_margin` seconds

For example, with buckets `[1, 5, 30, 120]`:
- A 3-second delay → bucket `5` (5-second TTL queue)
- A 10-second delay → bucket `30` (30-second TTL queue)
- A 45-second delay → bucket `120` (120-second TTL queue)

## Topology and recovery

After a connection loss and recovery, the `TopologyReconciler` replays the topology plan for the new connection generation. This ensures that exchanges, queues, and bindings exist before consumers are re-registered and publishers resume.

In `external` mode, no reconciliation commands are sent. The infrastructure is expected to be stable.

In `verify` mode, passive declarations are re-issued to detect drift after recovery.

See [Reliability — Recovery](reliability.md#connection-recovery) for the full recovery sequence.
