<?php

declare(strict_types=1);

use Closure;
use Goopil\RabbitRs\BackpressureException;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Str;
use InvalidArgumentException;

final class DelayedPublishTestJob
{
    public int $delay = 7;
}

/**
 * @return array{RabbitMqQueue, Pool}
 */
function publishQueue(): array
{
    $pool = new Pool();

    return [publishNewQueue($pool, publishRoutes(), 'default'), $pool];
}

/**
 * @param array<string, array<string, string>> $routes
 */
function publishNewQueue(
    Pool $pool,
    array $routes,
    string $defaultQueue,
    bool $dispatchAfterCommit = false,
): RabbitMqQueue {
    $queue = new RabbitMqQueue($pool, $routes, $defaultQueue, $dispatchAfterCommit);
    $queue->setContainer(test()->app);

    return $queue;
}

/**
 * @return array<string, array<string, string>>
 */
function publishRoutes(): array
{
    return [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'default.jobs',
            'routing_key' => '{queue}',
        ],
        'orders' => [
            'broker' => 'orders-broker',
            'exchange' => 'orders.jobs',
            'routing_key' => '{queue}.created.{queue}',
        ],
    ];
}

it('serializes the Laravel payload and uses its UUID as message id on push', function (): void {
    [$queue, $pool] = publishQueue();

    $messageId = $queue->push('App\\Jobs\\SendReport', ['report' => 42]);

    expect($pool->published)->toHaveCount(1);
    $message = $pool->published[0];
    $payload = json_decode($message['payload'], true, flags: JSON_THROW_ON_ERROR);
    expect('App\\Jobs\\SendReport')->toBe($payload['job'])
        ->and(['report' => 42])->toBe($payload['data'])
        ->and(Str::isUuid($payload['uuid']))->toBeTrue()
        ->and($payload['uuid'])->toBe($message['message_id'])
        ->and($message['message_id'])->toBe($messageId)
        ->and('application/json')->toBe($message['content_type']);
});

it('preserves the payload and accepts a stable message id on pushRaw', function (): void {
    [$queue, $pool] = publishQueue();
    $payload = "raw\0payload\xFF";
    $messageId = '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137';

    $result = $queue->pushRaw($payload, 'raw', [
        'message_id' => $messageId,
        'content_type' => 'application/octet-stream',
    ]);

    expect($payload)->toBe($pool->published[0]['payload'])
        ->and($messageId)->toBe($pool->published[0]['message_id'])
        ->and($messageId)->toBe($result)
        ->and('application/octet-stream')->toBe($pool->published[0]['content_type']);
});

it('selects the named route and feeds the routing key on pushOn', function (): void {
    [$queue, $pool] = publishQueue();

    $queue->pushOn('orders', 'App\\Jobs\\ShipOrder');

    expect('orders-broker')->toBe($pool->published[0]['broker'])
        ->and('orders.jobs')->toBe($pool->published[0]['exchange'])
        ->and('orders.created.orders')->toBe($pool->published[0]['routing_key']);
});

it('falls back to the default route for an unknown named route', function (): void {
    [$queue, $pool] = publishQueue();

    $queue->pushOn('invoices', 'App\\Jobs\\SendInvoice');

    expect('default-broker')->toBe($pool->published[0]['broker'])
        ->and('invoices')->toBe($pool->published[0]['routing_key']);
});

it('fails when neither the named nor default route exists', function (): void {
    $queue = publishNewQueue(new Pool(), [
        'orders' => publishRoutes()['orders'],
    ], 'missing');

    $this->expectException(InvalidArgumentException::class);
    $this->expectExceptionMessage('routes.missing');

    $queue->push('App\\Jobs\\MissingRoute');
});

it('passes the delay in milliseconds on later', function (): void {
    [$queue, $pool] = publishQueue();

    $queue->later(15, 'App\\Jobs\\SendReminder');

    expect(15_000)->toBe($pool->published[0]['delay_ms']);
    $payload = json_decode($pool->published[0]['payload'], true, flags: JSON_THROW_ON_ERROR);
    expect(15)->toBe($payload['delay']);
});

it('publishes immediately for a negative delay', function (): void {
    [$queue, $pool] = publishQueue();

    $queue->later(-5, 'App\\Jobs\\SendReminder');

    expect($pool->published[0])->not->toHaveKey('delay_ms');
});

it('publishes every Laravel payload in one native call on bulk', function (): void {
    [$queue, $pool] = publishQueue();

    $messageIds = $queue->bulk([
        'App\\Jobs\\First',
        'App\\Jobs\\Second',
        'App\\Jobs\\Third',
    ], ['batch' => true], 'orders');

    expect($pool->published)->toBe([])
        ->and($pool->publishedBatches)->toHaveCount(1)
        ->and($pool->publishedBatches[0])->toHaveCount(3)
        ->and(array_column($pool->publishedBatches[0], 'message_id'))->toBe($messageIds);
    foreach ($pool->publishedBatches[0] as $message) {
        $payload = json_decode($message['payload'], true, flags: JSON_THROW_ON_ERROR);
        expect($payload['uuid'])->toBe($message['message_id'])
            ->and('orders.created.orders')->toBe($message['routing_key']);
    }
});

it('does not cross the native boundary on empty bulk', function (): void {
    [$queue, $pool] = publishQueue();

    expect($queue->bulk([]))->toBe([])
        ->and($pool->publishedBatches)->toBe([]);
});

it('maps per-job delay without splitting the native batch on bulk', function (): void {
    [$queue, $pool] = publishQueue();

    $queue->bulk([
        new DelayedPublishTestJob(),
        'App\\Jobs\\Immediate',
    ]);

    expect($pool->publishedBatches)->toHaveCount(1)
        ->and(7_000)->toBe($pool->publishedBatches[0][0]['delay_ms'])
        ->and($pool->publishedBatches[0][1])->not->toHaveKey('delay_ms');
});

it('defers one native batch when the connection uses after commit', function (): void {
    $pool = new Pool();
    $queue = publishNewQueue($pool, publishRoutes(), 'default', true);
    $transactions = new class
    {
        public ?Closure $callback = null;

        public function addCallback(Closure $callback): null
        {
            $this->callback = $callback;

            return null;
        }
    };
    $this->app->instance('db.transactions', $transactions);

    expect($queue->bulk([
        'App\\Jobs\\First',
        'App\\Jobs\\Second',
    ]))->toBeNull()
        ->and($pool->publishedBatches)->toBe([]);

    ($transactions->callback)();

    expect($pool->publishedBatches)->toHaveCount(1)
        ->and($pool->publishedBatches[0])->toHaveCount(2);
});

it('translates a native publication failure into a QueueException', function (): void {
    [$queue, $pool] = publishQueue();
    $native = new NativeException(
        'message 018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137 was returned as unroutable (AMQP 312)',
    );
    $pool->throwOnNextPublish($native);

    try {
        $queue->push('App\\Jobs\\Unroutable');
        self::fail('The native publication failure was not translated.');
    } catch (QueueException $exception) {
        self::assertSame($native, $exception->getPrevious());
        self::assertStringContainsString('unroutable', $exception->getMessage());
    }
});

it('keeps backpressure as a recognizable dedicated exception', function (): void {
    [$queue, $pool] = publishQueue();
    $native = new BackpressureException('publisher global capacity is exhausted');
    $pool->throwOnNextPublish($native);

    try {
        $queue->push('App\\Jobs\\Busy');
        self::fail('The backpressure exception was not raised.');
    } catch (BackpressureException $exception) {
        self::assertSame($native, $exception);
    }
});

it('keeps after-commit publishing managed by the Laravel queue', function (): void {
    $pool = new Pool();
    $factory = new NativePoolFactory(
        createPool: static fn (array $config): Pool => $pool,
    );
    $connector = new RabbitMqConnector(
        $factory,
        ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs')),
    );
    $queue = $connector->connect([
        'queue' => 'default',
        'after_commit' => true,
    ]);
    $queue->setContainer($this->app);
    $transactions = new class
    {
        public ?Closure $callback = null;

        public function addCallback(Closure $callback): null
        {
            $this->callback = $callback;

            return null;
        }
    };
    $this->app->instance('db.transactions', $transactions);

    expect($queue->push('App\\Jobs\\AfterCommit'))->toBeNull()
        ->and($pool->published)->toBe([])
        ->and($transactions->callback)->toBeInstanceOf(Closure::class);

    ($transactions->callback)();

    expect($pool->published)->toHaveCount(1);
});
