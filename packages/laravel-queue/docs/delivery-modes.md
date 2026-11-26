# Choosing a delivery mode

Different jobs can tolerate different failure outcomes. An order must not disappear; a disposable telemetry event may be acceptable to lose. Rabbit RS lets you choose a publishing mode per Laravel queue connection, so these workloads can coexist in one application.

- [Compare the modes](#compare-the-modes)
- [Prevent duplicate business effects](#prevent-duplicate-business-effects)
- [Use fire-and-forget](#use-fire-and-forget)
- [Use several modes in one application](#use-several-modes-in-one-application)
- [Using the native PHP extension](#using-the-native-php-extension)

## Compare the modes

| Publisher mode | What the publisher tracks | What can happen | Suitable for |
|---|---|---|---|
| `safe` (default) | Broker confirms and mandatory returns for ordinary publishes | A publish may be retried after an ambiguous outcome and a delivery may be repeated. Unconfirmed in-memory work does not survive a PHP process crash. | Jobs where loss matters, with idempotent processing and an outbox when crash durability is needed |
| `unsafe` | No confirms or mandatory returns; the underlying publish waits for a socket write | The broker may drop an unroutable message and a later transport failure can lose it silently. A delivery can still be repeated. | Workloads that accept these trade-offs and need this particular write behavior |
| `blind` | No confirms or mandatory returns; a bounded background pump hands publications to the transport | Messages can be lost silently after hand-off or during recovery. A delivery can still be repeated. | Disposable events where silent loss is acceptable |

These are **publisher** settings. `blind` does not change consumer acknowledgement settings or guarantee that jobs run once. Conversely, `safe` does not deduplicate messages. For the detailed semantics, including delayed plugin messages, see [Safety modes](reference.md#safety-modes) and [Mandatory returns](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#mandatory-returns).

A normal Laravel dispatch can return while the extension still has buffered work. In `safe` mode, a later operation may surface a publish failure; if there is no later operation, teardown logs the unsurfaced failure. A return from `dispatch()` alone is not a broker confirmation. See [Where failures surface](reference.md#where-unroutable-publish-failures-surface-safe-mode).

## Prevent duplicate business effects

Rabbit RS does not offer an exactly-once delivery mode. Even with `safe`, RabbitMQ may redeliver a job after a worker completes the work but loses its acknowledgement. A publisher may also replay an ambiguous publication. Design the **business operation** so processing the same logical job again is harmless.

For example, suppose an order must be marked processed at most once, and every relevant effect is stored in the same database. Add a nullable `processed_at` column to `orders`, then serialize processing with a row lock and update the state in one transaction:

```php
use Illuminate\Support\Facades\DB;
use RuntimeException;

// Inside ProcessOrder::handle(); $this->orderId identifies the business operation.
DB::transaction(function (): void {
    $order = DB::table('orders')
        ->where('id', $this->orderId)
        ->lockForUpdate()
        ->first();

    if ($order === null) {
        throw new RuntimeException('Order not found');
    }

    if ($order->processed_at !== null) {
        return; // This order has already been processed.
    }

    DB::table('orders')->where('id', $this->orderId)->update([
        'status' => 'processed',
        'processed_at' => now(),
    ]);
});
```

This example assumes the database supports row locks and that processing an order is a one-time operation. The lock and state change must use the same database transaction; concurrent deliveries then cannot both pass the check. If the operation can legitimately happen more than once, use a separate stable **operation ID** and a database-enforced unique constraint instead of the order ID.

This protects the database effect shown; it does not make a card payment, email, or other external API call exactly once. For external effects, use an idempotency key supported by the provider or another durable coordination strategy. To protect publication across a PHP process crash, persist the intended message in an **external outbox** in the same business transaction and publish it separately. Rabbit RS does not provide the outbox. See [When to use an external outbox](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#when-to-use-an-external-outbox).

Redelivery metrics help identify recovery activity, but they do not detect every duplicate. Use the operation ID and durable application state to decide whether work has already been applied.

## Use fire-and-forget

Choose `blind` for jobs that can disappear without a retry or an error reaching the caller. For example, losing an occasional nonessential telemetry event might be acceptable:

```php
// config/queue.php, inside the telemetry connection
'safety' => 'blind',
```

The pump is bounded: hand-off can encounter backpressure or fail before it accepts work. After hand-off, the caller does not receive broker confirmation or a guarantee that the message was routed. In native PHP, `flush()` in blind mode waits for the pump's hand-off barrier, not for broker confirmation, and work can be dropped during recovery. Delayed publications use the configured delay routing, but a routing, topology, or transport failure after hand-off does not become a broker confirmation or a reliable error to the caller.

Use regular Laravel dispatch and a worker for the connection, as shown below. Choosing `blind` for publishing does not disable the worker's normal acknowledgements; explicitly opting into `early_ack` or `no_ack` changes consumption guarantees separately. See [Safety modes](reference.md#safety-modes) and [Subscriptions](reference.md#subscriptions-escape-hatch).

## Use several modes in one application

Define separate connections for important jobs and disposable telemetry. Both can point at the same RabbitMQ broker and vhost while using different queues and safety modes:

```php
// config/queue.php, inside the connections array
'orders' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
    'username' => env('RABBIT_RS_USERNAME', 'guest'),
    'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    'safety' => 'safe',
],

'telemetry' => [
    'driver' => 'rabbit-rs',
    'queue' => 'telemetry',
    'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
    'username' => env('RABBIT_RS_USERNAME', 'guest'),
    'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    'safety' => 'blind',
],
```

Dispatch each job to its intended connection:

```php
use App\Jobs\ProcessOrder;
use App\Jobs\RecordTelemetry;

ProcessOrder::dispatch($orderId)->onConnection('orders');
RecordTelemetry::dispatch($event)->onConnection('telemetry');
```

Run workers for both connections:

```bash
php artisan rabbit-rs:work --connection=orders,telemetry
```

Alternatively, run separate `php artisan queue:work orders` and `php artisan queue:work telemetry` processes if they need independent deployment or capacity. Each connection resolves its own native pool and configuration, even when it points to the same broker. Set the appropriate connection on every dispatch or choose a default using `QUEUE_CONNECTION`.

## Using the native PHP extension

For a standalone PHP application, configure the mode in the pool's publisher section:

```php
// Add this to the Pool configuration from the native getting-started guide.
'publisher' => [
    'safety' => 'safe', // or 'unsafe' or 'blind'
],
```

Use separate pool configurations when workloads need different publishing modes. The same limitations apply: `safe` permits duplicates, and `blind` neither confirms nor guarantees routing. The [native PHP guide](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/getting-started.md) shows how to create a pool, publish, flush, and consume.
