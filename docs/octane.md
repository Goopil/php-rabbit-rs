# Octane Integration

Rabbit RS integrates with [Laravel Octane](https://laravel.com/docs/octane) to support long-lived worker processes. This guide covers the lifecycle hooks, configuration, and pitfalls.

## How Octane lifecycle hooks work

Octane keeps a PHP process alive across many requests. This means the native Rabbit RS pool (and its AMQP connections) persists between requests, which is beneficial for performance — but it requires careful cleanup to prevent resource leaks.

Rabbit RS hooks into three Octane lifecycle events:

### flush (per-request)

Called after each request via `$app->terminating()`:

- **Closes cached consumers** on all resolved `RabbitMqQueue` connections
- Does **not** close the native pool — connections are reused across requests
- Prevents AMQP channel leaks that would accumulate if consumers were never closed

```php
// Triggered automatically
OctaneLifecycle::flush();
```

### reload (worker reload)

Called when Octane reloads the worker (e.g., after `php artisan octane:reload`):

- Closes cached consumers on all resolved queues
- **Flushes the pool factory** — all native pools are closed and recreated on next use
- **Forgets the queue manager's resolved connections** — the next request
  recompiles every rabbit-rs connection from the current config, so broker and
  credential rotation via env variables takes effect immediately (no stale
  brokers serving the boot-time snapshot)

Because connections are compiled lazily at resolution time (see
[Configuration](configuration.md)), `octane:reload` picks up fresh config
automatically — no manual normalization step exists or is needed.

```php
// Triggered by WorkerReload event
OctaneLifecycle::reload();
```

### stop (worker shutdown)

Called when the Octane worker stops:

- Closes cached consumers
- Flushes the pool factory — all native pools are closed
- Ensures clean shutdown of AMQP connections

```php
// Triggered by WorkerStopping event
OctaneLifecycle::stop();
```

## Event registration

The service provider registers the hooks when Octane is detected:

```php
// In RabbitMqServiceProvider::registerOctaneLifecycle()
if (! class_exists(\Laravel\Octane\Octane::class)) {
    return;
}

$app->terminating(fn () => $lifecycle->flush());
$events->listen(WorkerReload::class, fn () => $lifecycle->reload());
$events->listen(WorkerStopping::class, fn () => $lifecycle->stop());
```

Octane is an optional dependency. If `laravel/octane` is not installed, the hooks are not registered, and the package works normally with FPM and CLI.

## Why not to retain Request or service-container state

Rust threads inside Rabbit RS never retain Zend values, PHP objects, callbacks, Request instances, or service-container references. They only handle owned Rust data (strings, bytes, numbers, structs).

This is critical for Octane because:

1. **Request objects are recycled** — Octane reuses Request instances between requests; retaining a reference would leak stale request state
2. **Service container is reset** — the container is flushed between requests; references to old bindings are invalid
3. **Callbacks must be re-invoked on the PHP thread** — Rabbit RS invokes registered callbacks (like `onConnectionState`) synchronously during `stats()` or other PHP-side operations, never from a Rust thread

### What this means for your code

- **Do not** store Rabbit RS `Pool` or `Consumer` objects in statics or singletons that persist across requests
- **Do not** pass Request objects, user models, or session data to publish/consume calls — use owned data (strings, arrays, serializable values)
- **Do** rely on the service container to resolve fresh instances each request
- **Do** let the Octane lifecycle hooks clean up consumers between requests

## Configuration for Octane

### queue.php

Add the Rabbit RS connection to `config/queue.php` as usual — one connection
= one broker/vhost = one native pool:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        'after_commit' => false,
    ],
],
```

Every other key falls back to the package defaults in
`config/rabbit-rs.php` — see [Configuration](configuration.md).

### Octane configuration

No special Octane configuration is needed. Rabbit RS is automatically detected and the lifecycle hooks are registered.

```bash
# Start Octane with FrankenPHP
php artisan octane:start --server=frankenphp

# Start with RoadRunner
php artisan octane:start --server=roadrunner

# Start with Swoole
php artisan octane:start --server=swoole
```

### Supported Octane servers

Rabbit RS is certified with all four Octane servers:

| Server | Status |
|--------|--------|
| FrankenPHP | Certified |
| RoadRunner | Certified |
| Open Swoole | Certified |
| Swoole | Certified |

### Worker count

Each Octane worker is a separate PHP process with its own native pool. Connections are not shared between workers. Set the worker count based on your CPU cores and RabbitMQ connection limits:

```bash
# Start with 4 workers (each has its own connection pool)
php artisan octane:start --workers=4
```

### Flushing the pool

If you need to force-close all connections (e.g., before a deployment), use:

```bash
php artisan octane:reload
```

This triggers `WorkerReload`, which flushes all pools **and forgets the
resolved queue connections**. New requests create fresh AMQP connections and
recompile every connection from the current config — env-based broker or
credential rotation takes effect without a restart.

## Fork safety

Octane with Swoole/Open Swoole may use coroutines or fork workers. Rabbit RS detects PID changes after a fork and invalidates all inherited handles. The child process creates fresh connections lazily on first use.

This is transparent to the application — no special configuration is needed.

## Consumer cleanup

The `RabbitMqQueue` class caches `Consumer` instances per worker profile. The `flush()` hook calls `closeConsumers()` on all resolved queues, which:

1. Calls `close()` on each cached consumer
2. Clears the consumer cache
3. The next request creates fresh consumers on demand

This prevents AMQP channel leaks across requests. Without this cleanup, each Octane request would accumulate consumer channels without closing them.

### PHP-side destruct

As a safety net, `RabbitMqQueue` has a `__destruct()` that calls `closeConsumers()`. The native `ConsumerHandle` also has a `Drop` implementation that sends a best-effort `Close` to the actor. These ensure cleanup even if the lifecycle hooks are not invoked.

## Connection reuse

The native pool (AMQP connections and channels) is **not** closed between requests. It is only closed on `reload()` and `stop()`. This provides optimal performance:

- **Per-request**: consumers are closed and recreated (cheap)
- **Per-worker**: connections are reused (expensive to recreate)
- **Per-reload**: everything is flushed and recreated (clean state)
