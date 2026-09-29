<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Exceptions\DelayPluginMissingException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\DelayPluginGuard;
use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;
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
 * Registers a connection without a management URL and installs the given
 * native-probe verdict (true/false/null) on the guard seam.
 */
function guardNativeQueue(string $name, ?bool $verdict, array $delay = ['mode' => 'auto']): void
{
    config()->set('queue.connections.'.$name, [
        'driver' => 'rabbit-rs',
        'queue' => 'orders',
        'management_url' => null,
        'delay' => $delay,
    ]);

    DelayPluginGuard::$nativeProbe = static fn (string $connection): ?bool => $verdict;
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

    it('resolves the effective mode inside raw compiles so recompile sites share the connector fingerprint', function () {
        // The Octane /stats pattern: ConnectionCompiler::compile() called
        // directly on raw config, bypassing the connector. Before the mode
        // resolution moved into compile() this returned the unresolved 'auto'
        // while the connector path resolved 'ttl' — two native fingerprints,
        // two pools, and the stats endpoint read an empty pool forever.
        Http::fake([GUARD_MGMT_URL.'/api/overview' => Http::response(guardOverview(false))]);

        config()->set('queue.connections.guard-raw-compile', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'management_url' => GUARD_MGMT_URL,
            'delay' => ['mode' => 'auto'],
        ]);

        $compiled = ConnectionCompiler::compile(
            'guard-raw-compile',
            config('queue.connections.guard-raw-compile'),
            RabbitRsConnections::packageDefaults(),
        );

        expect($compiled['native']['delay']['mode'])->toBe('ttl');
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

describe('native probe fallback (no management_url)', function () {
    it('keeps the plugin strategy for auto when the native probe confirms the plugin', function () {
        Http::fake();
        guardNativeQueue('guard-native-present', true);

        expect(ConnectionCompiler::compile(
            'guard-native-present',
            config('queue.connections.guard-native-present'),
            RabbitRsConnections::packageDefaults(),
        )['native']['delay']['mode'])->toBe('auto');

        Http::assertNothingSent();
    });

    it('degrades auto to ttl when the native probe proves the plugin absent', function () {
        Http::fake();
        guardNativeQueue('guard-native-absent', false);

        expect(ConnectionCompiler::compile(
            'guard-native-absent',
            config('queue.connections.guard-native-absent'),
            RabbitRsConnections::packageDefaults(),
        )['native']['delay']['mode'])->toBe('ttl');

        Http::assertNothingSent();
    });

    it('degrades auto to ttl when the native probe is unverifiable', function () {
        Http::fake();
        guardNativeQueue('guard-native-null', null);

        expect(ConnectionCompiler::compile(
            'guard-native-null',
            config('queue.connections.guard-native-null'),
            RabbitRsConnections::packageDefaults(),
        )['native']['delay']['mode'])->toBe('ttl');
    });

    it('refuses a delayed publish in plugin mode when the native probe proves the plugin absent', function () {
        Http::fake();
        guardNativeQueue('guard-native-refuse', false, ['mode' => 'plugin']);

        try {
            ConnectionCompiler::compile(
                'guard-native-refuse',
                config('queue.connections.guard-native-refuse'),
                RabbitRsConnections::packageDefaults(),
            );
            DelayPluginGuard::assertPluginEnabled('guard-native-refuse');
            $this->fail('expected DelayPluginMissingException');
        } catch (DelayPluginMissingException $exception) {
            expect($exception->getMessage())->toContain('guard-native-refuse');
        }

        Http::assertNothingSent();
    });

    it('falls back to the ttl verdict without the extension and without a seam', function () {
        Http::fake();
        config()->set('queue.connections.guard-native-noext', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'management_url' => null,
            'delay' => ['mode' => 'auto'],
        ]);

        expect(ConnectionCompiler::compile(
            'guard-native-noext',
            config('queue.connections.guard-native-noext'),
            RabbitRsConnections::packageDefaults(),
        )['native']['delay']['mode'])->toBe('ttl');
    });

    it('publishes a delayed job through the native verdict path', function () {
        Http::fake();
        guardNativeQueue('guard-native-publish', true);

        $queue = app('queue')->connection('guard-native-publish');
        /** @var RabbitMqQueue $queue */
        $queue->setContainer(app());
        $queue->later(5, 'stdClass');

        expect(guardPool($queue)->published)->toHaveCount(1)
            ->and(guardPool($queue)->published[0]['delay_ms'])->toBe(5000);
        Http::assertNothingSent();
    });

    it('clears the native probe seam on reset', function () {
        DelayPluginGuard::$nativeProbe = static fn (string $connection): ?bool => true;
        DelayPluginGuard::reset();

        expect(DelayPluginGuard::$nativeProbe)->toBeNull();
    });
});

final class DelayPluginGuardBulkJob
{
    public int $delay = 5;
}
