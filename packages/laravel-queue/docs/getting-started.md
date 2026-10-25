# Getting started — Laravel queue driver

Rabbit RS ships as a standard Laravel queue driver: jobs are dispatched with the usual `Job::dispatch()` API and consumed by the usual `queue:work` — the connection pooling, publisher confirms, scheduling, and recovery run natively in Rust underneath. This guide goes from a first delivered message to production operation in four levels.

The same engine is also usable without Laravel — see the [native extension getting started](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/getting-started.md).

## Install

```bash
# Native extension (Linux via PIE)
pie install goopil/rabbit-rs-native

# Laravel queue driver (service provider auto-discovered)
composer require goopil/rabbit-rs-laravel
```

Details — macOS (Homebrew), Docker, manual binaries: the [installation guide](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#installation). Requirements: PHP 8.2–8.5, Laravel 12/13, a reachable RabbitMQ 4.2.9+ broker.

## 1. Your first job in 3 minutes

Prerequisites: the native extension installed ([Install](#install)) and a reachable RabbitMQ at `127.0.0.1:5672` — the repo's lab broker (`docker compose up -d rabbitmq` in `benchmarks/`, credentials `rabbit_rs` / `rabbit_rs_lab`) or your own (a stock install uses `guest` / `guest`).

Add one connection to `config/queue.php` — one connection = one broker/vhost = one native pool:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    ],
],
```

```bash
QUEUE_CONNECTION=rabbit-rs   # .env
```

Every other key falls back to the package defaults — quorum queues, `safe` publishing, prefetch 1000.

Generate the job:

```bash
php artisan make:job SendWelcomeEmail
```

```php
// app/Jobs/SendWelcomeEmail.php
namespace App\Jobs;

use Illuminate\Bus\Queueable;
use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Foundation\Bus\Dispatchable;
use Illuminate\Queue\InteractsWithQueue;
use Illuminate\Queue\SerializesModels;
use Illuminate\Support\Facades\Log;

class SendWelcomeEmail implements ShouldQueue
{
    use Dispatchable;
    use InteractsWithQueue;
    use Queueable;
    use SerializesModels;

    public function __construct(public readonly string $email) {}

    public function handle(): void
    {
        Log::info("Welcome email for {$this->email}");

        // Mail::to($this->email)->send(new WelcomeMail($this->email));
    }
}
```

Dispatch it from a route:

```php
// routes/web.php
use App\Jobs\SendWelcomeEmail;
use Illuminate\Support\Facades\Route;

Route::get('/welcome/{email}', function (string $email) {
    SendWelcomeEmail::dispatch($email);

    return "Queued a welcome email for {$email}.";
});
```

Open the route (e.g. `/welcome/ada@example.com`) and watch the worker:

```bash
php artisan queue:work rabbit-rs
```

```text
2026-10-25 10:00:00 App\Jobs\SendWelcomeEmail ..................... RUNNING
2026-10-25 10:00:00 App\Jobs\SendWelcomeEmail .......... 2.12ms DONE
```

That is Laravel's standard worker output; the `Log::info` line lands in `storage/logs/laravel.log`. In the management UI (`http://localhost:15672`, same credentials as the broker), the Queues tab lists the `default` queue — stop the worker, dispatch again, and one message is sitting in it.

The queue is declared on first use (`topology_mode: declare`). In production, run the supervised worker and let a process supervisor own its lifetime:

```bash
php artisan rabbit-rs:work    # one supervised queue:work child per rabbit-rs connection, crash restarts with backoff
```

The [Supervisor config example](https://github.com/Goopil/php-rabbit-rs/blob/main/examples/laravel/worker-supervisor.conf) covers restart-on-boot; every connection key (TLS, vhost, exchange, safety mode) is in [Configuration](reference.md#configuration).

## 2. Reliability

Delivery is **at-least-once**: once a message is accepted into the confirmed delivery path, silent loss is unacceptable, and duplicates are permitted and measurable. Jobs **must be idempotent** — the stable `message_id` (UUID from the Laravel payload) is the deduplication key. Full contract: [Reliability](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#reliability).

The `safety` key selects the publisher guarantees (default `safe`: publisher confirms plus mandatory routing — there is no separate `mandatory` switch, it follows the safety mode). In `safe` mode every publish is tracked to ACK, return, or timeout; unconfirmed publications survive connection recovery in bounded process memory and are replayed with their original `message_id`. That replay buffer is **process memory, not durability**: it does not survive a PHP crash, and cross-process durability needs an external outbox.

The one production must-do: bound redeliveries with a delivery limit and a dead-letter exchange, so poison messages are routed instead of looped forever:

```php
'rabbit-rs' => [
    // ...
    'delivery_limit' => 20,          // after 20 redeliveries, dead-letter
    'dead_letter' => [
        'exchange' => 'dead-letters',
        'queue' => 'failed-jobs',
        'routing_key' => null,       // null = keep the original routing key
    ],
],
```

## 3. Scale

**Multiple brokers/vhosts** — define one connection per broker; the framework's own way to express multiple backends:

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
        'exchange' => 'billing.jobs',
    ],
],
```

```php
BillingJob::dispatch($invoice)->onConnection('billing');
```

```bash
php artisan rabbit-rs:work --connection=orders-eu,billing
```

**Several queues per connection** — the `subscriptions` escape hatch fans deliveries through one weighted-fair consumer:

```php
'orders-eu' => [
    // ...
    'subscriptions' => [
        'critical' => ['queue' => 'orders.critical', 'weight' => 8, 'prefetch' => 8],
        'bulk' => ['queue' => 'orders.bulk', 'weight' => 2, 'prefetch' => 32],
    ],
],
```

`weight` (1–65535) sets each subscription's delivery share — weighted-fair cannot starve by construction. `prefetch` also accepts an adaptive controller that keeps about `target_buffer_seconds` of ready work buffered:

```php
'prefetch' => ['mode' => 'adaptive', 'initial' => 64, 'min' => 1, 'max' => 256, 'target_buffer_seconds' => 5],
```

Full reference — every connection key, `hosts` failover semantics, validation rules: [Configuration](reference.md#configuration).

## 4. Operate

| Command | Purpose |
| ------- | ------- |
| `php artisan rabbit-rs:work` | Supervised fan-out worker (`--connection`, `--queue`, `--workers`, `--max-restarts`, `--backoff`) |
| `php artisan rabbit-rs:status` | Per-connection pool metrics and counters; `--format=json` for monitoring |
| `php artisan rabbit-rs:doctor` | One-shot health report — `ok`/`warn`/`fail` checks, non-zero exit on failure (CI-friendly) |
| `php artisan rabbit-rs:topology` | Preflight topology check; `--fix` declares missing topology |
| `php artisan rabbit-rs:probe` | Kubernetes probes (`startup`/`ready`/`alive`/`prestop`) over the worker statefiles; liveness never touches the broker |

Details: [Operations](reference.md#operations) (supervision, Kubernetes, metrics), [Laravel usage](reference.md#usage) (dispatch API, events, job class).

**Horizon** — set `RABBIT_RS_WORKER=horizon` (or `'worker' => 'horizon'` on the connection) and Rabbit RS jobs appear in the Horizon dashboard alongside Redis jobs — configured in `config/horizon.php` exactly like a Redis queue; Redis stores observability state while RabbitMQ stays the transport. Setup and the event contract: [Laravel usage — Horizon](reference.md#laravel-horizon).

**Octane** — detected automatically: consumers are closed after each request, pools are flushed and re-normalized on worker reload, stopped on shutdown. No configuration needed — see [Octane](octane.md).

## Going further

- [Configuration](reference.md#configuration) — every connection key, subscriptions escape hatch, safety/delay/topology modes, validation
- [Topology management](reference.md#topology) — declare/verify/external, dead-letter wiring
- [Operations](reference.md#operations) — systemd/Supervisor/Kubernetes, metrics, backpressure
- [Recipes](recipes.md) — topology patterns, broker tuning, capacity planning
- [Laravel usage](reference.md#usage) — RabbitMqQueue API, events, job class, Horizon integration
- [Reliability](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#reliability) — the at-least-once contract
- [Native extension getting started](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/getting-started.md) — the engine underneath
