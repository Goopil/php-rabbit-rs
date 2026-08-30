<?php

use Illuminate\Foundation\Application;

/*
|--------------------------------------------------------------------------
| Minimal driver-bench application bootstrap
|--------------------------------------------------------------------------
|
| Phase E benchmarks the full Laravel queue API (dispatch / pop / ack) for
| three RabbitMQ drivers. This skeleton intentionally has no routes, views
| or migrations: only the queue layer is exercised.
|
*/

$app = Application::configure(basePath: dirname(__DIR__))
    ->withExceptions()
    ->create();

/*
 * Both third-party drivers register their connector under the generic
 * `rabbitmq` driver name and would silently shadow each other depending on
 * provider registration order. Re-register each one under an unambiguous
 * name so the three connections (`rabbit-rs`, `rabbitmq-amqplib`,
 * `rabbitmq-ext`) coexist and can never be confused inside a run.
 *
 * Runs in a `booting` callback: the `queue` binding only exists once the
 * framework providers are registered during the kernel bootstrap.
 */
$app->booting(static function (Application $app): void {
    $queue = $app->make('queue');

    $queue->extend('rabbitmq-amqplib', static fn () => new VladimirYuldashev\LaravelQueueRabbitMQ\Queue\Connectors\RabbitMQConnector($app->make(Illuminate\Contracts\Events\Dispatcher::class)));

    $queue->extend('rabbitmq-ext', static fn () => new iamfarhad\LaravelRabbitMQ\Connectors\RabbitMQConnector($app->make(Illuminate\Contracts\Events\Dispatcher::class)));
});

return $app;
