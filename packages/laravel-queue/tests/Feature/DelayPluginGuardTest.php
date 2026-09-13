<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Exceptions\DelayPluginMissingException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\DelayPluginGuard;
use Illuminate\Support\Facades\Http;

const GUARD_MGMT_URL = 'http://mq.local:15672';

/**
 * Registers the guarded connection with a delay-mode override and resolves
 * it through the queue manager so the queue carries the connection name the
 * guard reads its config and cache by.
 */
function guardQueue(string $name, array $delay, array $overrides = []): RabbitMqQueue
{
    config()->set('queue.connections.'.$name, array_merge([
        'driver' => 'rabbit-rs',
        'queue' => 'orders',
        'management_url' => GUARD_MGMT_URL,
        'delay' => $delay,
    ], $overrides));

    /** @var RabbitMqQueue $queue */
    $queue = app('queue')->connection($name);
    $queue->setContainer(app());

    return $queue;
}

function guardPool(object $queue): object
{
    // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
    return (new ReflectionProperty($queue, 'pool'))->getValue($queue);
}

/**
 * Overview response whose exchange_types carry (or omit) the plugin's
 * delayed exchange type — the wire evidence the guard reads.
 */
function guardOverview(bool $pluginPresent): array
{
    $types = [
        ['name' => 'direct', 'enabled' => true],
        ['name' => 'fanout', 'enabled' => true],
    ];
    if ($pluginPresent) {
        $types[] = ['name' => 'x-delayed-message', 'enabled' => true];
    }

    return ['exchange_types' => $types];
}

beforeEach(function () {
    DelayPluginGuard::reset();
    bootFakeNativeExtension($this->app);
});

describe('auto-mode resolution (compile-level routing decision)', function () {
    it('compiles auto to the ttl bucket target when the plugin is absent', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);

        $queue = guardQueue('guard-auto-absent', ['mode' => 'auto']);

        expect(guardPool($queue)->config['delay']['mode'])->toBe('ttl');
    });

    it('keeps the plugin strategy for auto when the plugin is present', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(true))]);

        $queue = guardQueue('guard-auto-present', ['mode' => 'auto']);

        expect(guardPool($queue)->config['delay']['mode'])->toBe('auto');
    });

    it('degrades auto to the ttl bucket target when the plugin cannot be verified', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response('down', 500)]);

        $queue = guardQueue('guard-auto-error', ['mode' => 'auto']);

        expect(guardPool($queue)->config['delay']['mode'])->toBe('ttl');
    });

    it('degrades auto to the ttl bucket target without a management url', function () {
        Http::fake();

        $queue = guardQueue('guard-auto-nomgmt', ['mode' => 'auto'], ['management_url' => null]);

        Http::assertNothingSent();
        expect(guardPool($queue)->config['delay']['mode'])->toBe('ttl');
    });

    it('never rewrites explicit plugin and ttl modes', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);

        $plugin = guardQueue('guard-explicit-plugin', ['mode' => 'plugin']);
        $ttl = guardQueue('guard-explicit-ttl', ['mode' => 'ttl']);

        expect(guardPool($plugin)->config['delay']['mode'])->toBe('plugin')
            ->and(guardPool($ttl)->config['delay']['mode'])->toBe('ttl');
    });

    it('publishes delayed jobs in degraded auto mode without a per-publish probe', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);
        $queue = guardQueue('guard-auto-publish', ['mode' => 'auto']);

        $queue->later(5, 'stdClass');

        expect(guardPool($queue)->published)->toHaveCount(1)
            ->and(guardPool($queue)->published[0]['delay_ms'])->toBe(5000);
        Http::assertSentCount(1);
    });

    it('resolves the degraded ttl mode from the connection buckets', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);

        $queue = guardQueue('guard-auto-buckets', ['mode' => 'auto', 'buckets' => [10, 20]]);

        expect(guardPool($queue)->config['delay'])->toBe([
            'mode' => 'ttl',
            'buckets' => [10, 20],
            'max_buckets' => 8,
            'queue_expiry_margin' => 60,
        ]);
    });
});

describe('plugin-mode refusal (silent-loss guard)', function () {
    it('refuses the first delayed publish in plugin mode when the plugin is absent', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);
        $queue = guardQueue('guard-refuse', ['mode' => 'plugin']);

        try {
            $queue->later(5, 'stdClass');
            $this->fail('expected DelayPluginMissingException');
        } catch (DelayPluginMissingException $exception) {
            expect($exception->getMessage())->toContain('rabbitmq_delayed_message_exchange')
                ->and($exception->getMessage())->toContain('guard-refuse')
                ->and($exception->getMessage())->toContain('delay.mode');
        }

        expect(guardPool($queue)->published)->toBe([]);
        Http::assertSentCount(1);
    });

    it('publishes delayed jobs in plugin mode when the plugin is present', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(true))]);
        $queue = guardQueue('guard-allow', ['mode' => 'plugin']);

        $queue->later(5, 'stdClass');

        expect(guardPool($queue)->published)->toHaveCount(1)
            ->and(guardPool($queue)->published[0]['delay_ms'])->toBe(5000);
    });

    it('publishes through when the plugin state cannot be verified', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response('down', 500)]);
        $queue = guardQueue('guard-unverified', ['mode' => 'plugin']);

        $queue->later(5, 'stdClass');

        expect(guardPool($queue)->published)->toHaveCount(1);
    });

    it('does not consult the management api without a delay', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);
        $queue = guardQueue('guard-zero-delay', ['mode' => 'plugin']);

        $queue->later(0, 'stdClass');
        $queue->push('stdClass');

        Http::assertNothingSent();
        expect(guardPool($queue)->published)->toHaveCount(2);
    });

    it('does not consult the management api for ttl mode', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);
        $queue = guardQueue('guard-ttl-publish', ['mode' => 'ttl']);

        $queue->later(5, 'stdClass');

        Http::assertNothingSent();
        expect(guardPool($queue)->published)->toHaveCount(1);
    });

    it('caches the plugin verdict per connection', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(true))]);
        $queue = guardQueue('guard-cache', ['mode' => 'plugin']);

        $queue->later(5, 'stdClass');
        $queue->later(5, 'stdClass');

        Http::assertSentCount(1);
    });

    it('refuses delayed bulk publishes in plugin mode when the plugin is absent', function () {
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);
        $queue = guardQueue('guard-bulk', ['mode' => 'plugin']);

        try {
            $queue->bulk([
                'App\\Jobs\\Immediate',
                new DelayPluginGuardBulkJob,
            ]);
            $this->fail('expected DelayPluginMissingException');
        } catch (DelayPluginMissingException) {
        }

        expect(guardPool($queue)->publishedBatches)->toBe([]);
    });
});

final class DelayPluginGuardBulkJob
{
    public int $delay = 5;
}
