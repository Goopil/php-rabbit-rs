# Using Rabbit RS with Laravel

Once the [installation and first job](getting-started.md) are working, use this guide for everyday queue operations. Rabbit RS is a Laravel queue driver: you dispatch jobs using Laravel's APIs and run them with Laravel's worker. RabbitMQ handles the transport.

- [Connections and queues](#connections-and-queues)
- [Dispatching jobs](#dispatching-jobs)
- [Running workers](#running-workers)
- [Retries and failed jobs](#retries-and-failed-jobs)
- [Testing](#testing)
- [Checking your queues](#checking-your-queues)
- [Going to production](#going-to-production)

## Connections and queues

A **connection** describes where to send jobs: its broker hosts, credentials, virtual host, and driver settings live in `config/queue.php`. A **queue** is a named destination within that connection. One connection can consume several queues.

For example, this connection consumes `orders` and `emails` on the same broker:

```php
// config/queue.php, inside the connections array
'rabbit-rs' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
    'username' => env('RABBIT_RS_USERNAME', 'guest'),
    'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    'subscriptions' => [
        'orders' => ['queue' => 'orders', 'weight' => 1, 'prefetch' => 16],
        'emails' => ['queue' => 'emails', 'weight' => 1, 'prefetch' => 16],
    ],
],
```

Set `QUEUE_CONNECTION=rabbit-rs` in `.env` to make this connection the default. If your configuration is cached, rebuild the cache after changing it.

The `queue` key selects the default queue. When using `subscriptions`, it must name one of the subscribed queues. Declare every queue that your workers should consume in the connection's `queue` or `subscriptions` configuration; selecting a queue when dispatching does not by itself add a consumer. See [Subscriptions](reference.md#subscriptions-escape-hatch) for weights and prefetch settings.

## Dispatching jobs

### Dispatch a job

Dispatch a job as you would with any Laravel queue driver:

```php
use App\Jobs\ProcessOrder;

ProcessOrder::dispatch($order);
```

This sends the job to the default connection and its default queue. To choose a connection or queue explicitly:

```php
ProcessOrder::dispatch($order)
    ->onConnection('rabbit-rs')
    ->onQueue('orders');
```

For example, `onQueue('emails')` targets the `emails` subscription from the configuration above. Start a worker that consumes that queue as described below.

### Delay a job

Use Laravel's `delay` method:

```php
use App\Jobs\SendReminder;

SendReminder::dispatch($user)->delay(now()->addMinutes(5));
```

The default `auto` delay mode uses the RabbitMQ delayed-message plugin when it can verify that the plugin is available; otherwise it uses TTL queues. TTL mode groups delays into configured buckets, so a job may become available later than the requested time. The `blind` safety mode does not honor delays. See [Delayed messages](reference.md#delayed-messages) before choosing a mode.

### Dispatch after a database transaction

A worker may receive a job before the database transaction that created its data commits. When the job needs that data, dispatch it after the transaction commits:

```php
use App\Jobs\ProcessOrder;
use Illuminate\Support\Facades\DB;

DB::transaction(function () use ($order) {
    // Make your database changes here...

    ProcessOrder::dispatch($order)->afterCommit();
});
```

You may instead set `'after_commit' => true` on the queue connection to defer its jobs by default. This controls dispatch timing; it does not create a durable outbox. For durability across a PHP process crash between committing and publishing, use an external outbox. See [When to use an external outbox](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#when-to-use-an-external-outbox).

## Running workers

### Run a worker for the default queue

Start Laravel's worker with the connection name:

```bash
php artisan queue:work rabbit-rs
```

Without `--queue`, this consumes the connection's default `queue`. To consume a specific configured queue:

```bash
php artisan queue:work rabbit-rs --queue=emails
```

A worker targeting one queue on a multi-queue connection consumes that queue, not its siblings. For the precise resolution rules, see [Consuming jobs](reference.md#consuming-jobs).

### Run workers across connections

Use the Rabbit RS supervisor when you want to start and supervise workers for your configured Rabbit RS connections:

```bash
php artisan rabbit-rs:work
```

By default it starts one `queue:work` child per Rabbit RS connection and includes that connection's configured queues. To start two children per connection, or restrict the connections:

```bash
php artisan rabbit-rs:work --workers=2
php artisan rabbit-rs:work --connection=rabbit-rs
```

Use a process supervisor such as systemd, Supervisor, or Kubernetes to keep `rabbit-rs:work` running. See [Worker supervisor](reference.md#rabbit-rswork-supervisor) for restart, scaling, and shutdown options. For Horizon, follow the [Horizon setup](reference.md#laravel-horizon).

### Deploy changes

Queue workers are long-running processes and do not automatically load changed application code. Include Laravel's worker restart step in your deployment process, and make sure your process supervisor starts replacement workers. See Laravel's [queue workers and deployment](https://laravel.com/docs/12.x/queues#queue-workers-and-deployment) documentation.

## Retries and failed jobs

Laravel's worker handles jobs using its normal attempts and failure lifecycle. Configure your job's retry policy and your application's failed-job storage as you would for Laravel queues; see Laravel's [dealing with failed jobs](https://laravel.com/docs/12.x/queues#dealing-with-failed-jobs).

RabbitMQ can redeliver a job after a worker or connection fails, and a publish retried after an ambiguous outcome can also produce another copy. Make the job's effects **idempotent**: use a stable business identifier or message ID and record completed work in durable storage. A redelivery counter is a useful signal, but it cannot detect every duplicate.

For poison messages, configure a dead-letter destination and a delivery limit appropriate for your workload:

```php
// config/queue.php, inside the rabbit-rs connection
'delivery_limit' => 20,
'dead_letter' => [
    'exchange' => 'dead-letters',
    'queue' => 'failed-jobs',
    'routing_key' => null,
],
```

A RabbitMQ dead-letter queue and Laravel's failed-job storage are different mechanisms. Configure and monitor each according to the failure paths you need to investigate. See [Dead-letter configuration](reference.md#dlq-configuration) and [Reliability](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#reliability).

## Testing

For an application test that checks whether your code dispatches a job, Laravel's queue fake avoids sending messages to RabbitMQ:

```php
use App\Jobs\ProcessOrder;
use Illuminate\Support\Facades\Queue;

Queue::fake();

// Exercise the application code that dispatches the job...

Queue::assertPushed(ProcessOrder::class);
```

A fake does not test the extension, broker configuration, routing, or worker. Use a separate integration test with a real RabbitMQ broker when you need to verify those pieces together. See Laravel's [queue testing documentation](https://laravel.com/docs/12.x/queues#testing).

## Checking your queues

Run the doctor to check the connection and its configuration:

```bash
php artisan rabbit-rs:doctor --connection=rabbit-rs
```

Use `php artisan rabbit-rs:status` for a snapshot, or add `--format=json` for machine-readable output. Native pool counters belong to the PHP process that collected them: a new Artisan process cannot read the counters of your running workers. Optional management API counters provide broker-side queue information. See [Diagnostics](reference.md#diagnostics) and [Measuring duplicates](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#measuring-duplicates).

## Going to production

Before deploying a worker, decide how you will supervise it, handle repeated jobs, configure dead-lettering, and monitor your broker. The following guides cover the details:

- [Configuration](reference.md#configuration) — broker connections, credentials, TLS, and defaults.
- [Topology](reference.md#topology) — queues, exchanges, bindings, and dead-lettering.
- [Operations](reference.md#operations) — diagnostics, process supervision, Kubernetes, and monitoring.
- [Octane](octane.md) — lifecycle and server-specific configuration.
- [Recipes](recipes.md) — topology patterns and capacity planning.
