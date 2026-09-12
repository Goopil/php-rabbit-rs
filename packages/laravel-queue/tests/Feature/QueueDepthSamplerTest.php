<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\QueueDepthSampler;
use Illuminate\Support\Facades\Http;

const SAMPLER_MGMT_URL = 'http://mq.local:15672';

/**
 * @param  list<array{connection: string, queues: list<string>}>  $plan
 * @param  (Closure(string, string): int|null)|null  $nativeDepth
 */
function sampler(array $plan, ?Closure $nativeDepth = null): QueueDepthSampler
{
    return new QueueDepthSampler($plan, $nativeDepth);
}

/**
 * @return Closure(string, string): int|null a seam answering from a
 *                                           connection=>queue=>depth map, recording every call
 */
function seamRecorder(array $answers, array &$calls): Closure
{
    $calls = [];

    return function (string $connection, string $queue) use ($answers, &$calls): ?int {
        $calls[] = [$connection, $queue];

        return $answers[$connection][$queue] ?? null;
    };
}

beforeEach(function () {
    config()->set('queue.connections.mq', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'username' => 'worker',
        'password' => 'secret',
    ]);
});

describe('QueueDepthSampler source selection', function () {
    it('reads from the management api when management_url is configured', function () {
        config()->set('queue.connections.mq.management_url', SAMPLER_MGMT_URL);
        Http::fake([
            SAMPLER_MGMT_URL.'/api/queues/*' => Http::response(['messages_ready' => 5]),
        ]);
        $calls = [];
        $native = seamRecorder([], $calls);

        $depths = sampler([['connection' => 'mq', 'queues' => ['default']]], $native)->depths();

        expect($depths)->toBe(['mq' => 5])
            ->and($calls)->toBe([]);
        Http::assertSentCount(1);
    });

    it('falls back to the native probe when management_url is not configured', function () {
        Http::fake();
        $calls = [];
        $native = seamRecorder(['mq' => ['default' => 3]], $calls);

        $depths = sampler([['connection' => 'mq', 'queues' => ['default']]], $native)->depths();

        expect($depths)->toBe(['mq' => 3])
            ->and($calls)->toBe([['mq', 'default']]);
        Http::assertNothingSent();
    });

    it('sums the native depth of every planned queue of a connection', function () {
        $calls = [];
        $native = seamRecorder(['mq' => ['orders' => 2, 'billing' => 7]], $calls);

        $depths = sampler([['connection' => 'mq', 'queues' => ['orders', 'billing']]], $native)->depths();

        expect($depths)->toBe(['mq' => 9]);
    });

    it('keeps partial native answers: unreadable queues contribute nothing', function () {
        $calls = [];
        $native = seamRecorder(['mq' => ['orders' => 4, 'billing' => null]], $calls);

        $depths = sampler([['connection' => 'mq', 'queues' => ['orders', 'billing']]], $native)->depths();

        expect($depths)->toBe(['mq' => 4]);
    });

    it('reports null when no queue depth is readable through the native path', function () {
        $calls = [];
        $native = seamRecorder([], $calls);

        $depths = sampler([['connection' => 'mq', 'queues' => ['default']]], $native)->depths();

        expect($depths)->toBe(['mq' => null]);
    });

    it('does not cascade into the native probe when the configured management api fails', function () {
        config()->set('queue.connections.mq.management_url', SAMPLER_MGMT_URL);
        Http::fake(['*' => Http::response('down', 500)]);
        $calls = [];
        $native = seamRecorder(['mq' => ['default' => 3]], $calls);

        $depths = sampler([['connection' => 'mq', 'queues' => ['default']]], $native)->depths();

        expect($depths)->toBe(['mq' => null])
            ->and($calls)->toBe([]);
    });

    it('mixes strategies across connections: management api where configured, native elsewhere', function () {
        config()->set('queue.connections.mq.management_url', SAMPLER_MGMT_URL);
        config()->set('queue.connections.local', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
        ]);
        Http::fake([
            SAMPLER_MGMT_URL.'/api/queues/*' => Http::response(['messages_ready' => 5]),
        ]);
        $calls = [];
        $native = seamRecorder(['local' => ['default' => 2]], $calls);

        $depths = sampler([
            ['connection' => 'mq', 'queues' => ['default']],
            ['connection' => 'local', 'queues' => ['default']],
        ], $native)->depths();

        expect($depths)->toBe(['mq' => 5, 'local' => 2]);
        Http::assertSentCount(1);
    });
});
