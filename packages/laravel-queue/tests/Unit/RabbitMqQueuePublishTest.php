<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Closure;
use Goopil\RabbitRs\BackpressureException;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Str;
use InvalidArgumentException;

final class DelayedPublishTestJob
{
    public int $delay = 7;
}

final class RabbitMqQueuePublishTest extends TestCase
{
    public function testPushSerializesTheLaravelPayloadAndUsesItsUuidAsMessageId(): void
    {
        [$queue, $pool] = $this->queue();

        $messageId = $queue->push('App\\Jobs\\SendReport', ['report' => 42]);

        self::assertCount(1, $pool->published);
        $message = $pool->published[0];
        $payload = json_decode($message['payload'], true, flags: JSON_THROW_ON_ERROR);
        self::assertSame('App\\Jobs\\SendReport', $payload['job']);
        self::assertSame(['report' => 42], $payload['data']);
        self::assertTrue(Str::isUuid($payload['uuid']));
        self::assertSame($payload['uuid'], $message['message_id']);
        self::assertSame($message['message_id'], $messageId);
        self::assertSame('application/json', $message['content_type']);
    }

    public function testPushRawPreservesThePayloadAndAcceptsAStableMessageId(): void
    {
        [$queue, $pool] = $this->queue();
        $payload = "raw\0payload\xFF";
        $messageId = '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137';

        $result = $queue->pushRaw($payload, 'raw', [
            'message_id' => $messageId,
            'content_type' => 'application/octet-stream',
        ]);

        self::assertSame($payload, $pool->published[0]['payload']);
        self::assertSame($messageId, $pool->published[0]['message_id']);
        self::assertSame($messageId, $result);
        self::assertSame('application/octet-stream', $pool->published[0]['content_type']);
    }

    public function testPushOnSelectsTheNamedRouteAndFeedsTheRoutingKey(): void
    {
        [$queue, $pool] = $this->queue();

        $queue->pushOn('orders', 'App\\Jobs\\ShipOrder');

        self::assertSame('orders-broker', $pool->published[0]['broker']);
        self::assertSame('orders.jobs', $pool->published[0]['exchange']);
        self::assertSame('orders.created.orders', $pool->published[0]['routing_key']);
    }

    public function testUnknownNamedRouteFallsBackToTheDefaultRoute(): void
    {
        [$queue, $pool] = $this->queue();

        $queue->pushOn('invoices', 'App\\Jobs\\SendInvoice');

        self::assertSame('default-broker', $pool->published[0]['broker']);
        self::assertSame('invoices', $pool->published[0]['routing_key']);
    }

    public function testPublishingFailsWhenNeitherTheNamedNorDefaultRouteExists(): void
    {
        $queue = $this->newQueue(new Pool(), [
            'orders' => $this->routes()['orders'],
        ], 'missing');

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('routes.missing');

        $queue->push('App\\Jobs\\MissingRoute');
    }

    public function testLaterPassesTheDelayInMilliseconds(): void
    {
        [$queue, $pool] = $this->queue();

        $queue->later(15, 'App\\Jobs\\SendReminder');

        self::assertSame(15_000, $pool->published[0]['delay_ms']);
        $payload = json_decode($pool->published[0]['payload'], true, flags: JSON_THROW_ON_ERROR);
        self::assertSame(15, $payload['delay']);
    }

    public function testNegativeDelayIsPublishedImmediately(): void
    {
        [$queue, $pool] = $this->queue();

        $queue->later(-5, 'App\\Jobs\\SendReminder');

        self::assertArrayNotHasKey('delay_ms', $pool->published[0]);
    }

    public function testBulkPublishesEveryLaravelPayloadInOneNativeCall(): void
    {
        [$queue, $pool] = $this->queue();

        $messageIds = $queue->bulk([
            'App\\Jobs\\First',
            'App\\Jobs\\Second',
            'App\\Jobs\\Third',
        ], ['batch' => true], 'orders');

        self::assertSame([], $pool->published);
        self::assertCount(1, $pool->publishedBatches);
        self::assertCount(3, $pool->publishedBatches[0]);
        self::assertSame(array_column($pool->publishedBatches[0], 'message_id'), $messageIds);
        foreach ($pool->publishedBatches[0] as $message) {
            $payload = json_decode($message['payload'], true, flags: JSON_THROW_ON_ERROR);
            self::assertSame($payload['uuid'], $message['message_id']);
            self::assertSame('orders.created.orders', $message['routing_key']);
        }
    }

    public function testEmptyBulkDoesNotCrossTheNativeBoundary(): void
    {
        [$queue, $pool] = $this->queue();

        self::assertSame([], $queue->bulk([]));
        self::assertSame([], $pool->publishedBatches);
    }

    public function testBulkMapsPerJobDelayWithoutSplittingTheNativeBatch(): void
    {
        [$queue, $pool] = $this->queue();

        $queue->bulk([
            new DelayedPublishTestJob(),
            'App\\Jobs\\Immediate',
        ]);

        self::assertCount(1, $pool->publishedBatches);
        self::assertSame(7_000, $pool->publishedBatches[0][0]['delay_ms']);
        self::assertArrayNotHasKey('delay_ms', $pool->publishedBatches[0][1]);
    }

    public function testBulkDefersOneNativeBatchWhenTheConnectionUsesAfterCommit(): void
    {
        $pool = new Pool();
        $queue = $this->newQueue($pool, $this->routes(), 'default', true);
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

        self::assertNull($queue->bulk([
            'App\\Jobs\\First',
            'App\\Jobs\\Second',
        ]));
        self::assertSame([], $pool->publishedBatches);

        ($transactions->callback)();

        self::assertCount(1, $pool->publishedBatches);
        self::assertCount(2, $pool->publishedBatches[0]);
    }

    public function testNativePublicationFailureBecomesAQueueException(): void
    {
        [$queue, $pool] = $this->queue();
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
    }

    public function testBackpressureRemainsARecognizableDedicatedException(): void
    {
        [$queue, $pool] = $this->queue();
        $native = new BackpressureException('publisher global capacity is exhausted');
        $pool->throwOnNextPublish($native);

        try {
            $queue->push('App\\Jobs\\Busy');
            self::fail('The backpressure exception was not raised.');
        } catch (BackpressureException $exception) {
            self::assertSame($native, $exception);
        }
    }

    public function testAfterCommitPublishingRemainsManagedByLaravelQueue(): void
    {
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

        self::assertNull($queue->push('App\\Jobs\\AfterCommit'));
        self::assertSame([], $pool->published);
        self::assertInstanceOf(Closure::class, $transactions->callback);

        ($transactions->callback)();

        self::assertCount(1, $pool->published);
    }

    /**
     * @return array{RabbitMqQueue, Pool}
     */
    private function queue(): array
    {
        $pool = new Pool();

        return [$this->newQueue($pool, $this->routes(), 'default'), $pool];
    }

    /**
     * @param array<string, array<string, string>> $routes
     */
    private function newQueue(
        Pool $pool,
        array $routes,
        string $defaultQueue,
        bool $dispatchAfterCommit = false,
    ): RabbitMqQueue
    {
        $queue = new RabbitMqQueue($pool, $routes, $defaultQueue, $dispatchAfterCommit);
        $queue->setContainer($this->app);

        return $queue;
    }

    /**
     * @return array<string, array<string, string>>
     */
    private function routes(): array
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
}