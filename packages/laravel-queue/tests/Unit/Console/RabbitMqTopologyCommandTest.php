<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\DoctorProbe;
use Illuminate\Support\Facades\Artisan;
use Illuminate\Support\Facades\Http;

/**
 * Binds a fake environment probe: the unit suite runs without ext-rabbit_rs
 * and without a broker, so the topology command's extension, queue, and
 * declare probes are substituted with configurable fakes.
 */
function bindFakeTopologyProbe($app, array $missingQueues = [], ?string $declareError = null): object
{
    $probe = new class($missingQueues, $declareError) extends DoctorProbe
    {
        public int $declareCalls = 0;

        /** @param list<string> $missingQueues */
        public function __construct(
            private readonly array $missingQueues,
            private readonly ?string $declareError,
        ) {}

        public function extensionLoaded(): bool
        {
            return true;
        }

        public function extensionVersion(): ?string
        {
            return '0.1.3';
        }

        public function queueSize(array $nativeConfig, string $broker, string $queue): ?string
        {
            if (! in_array($queue, $this->missingQueues, true)) {
                return null;
            }

            return sprintf(
                "protocol error: AMQP soft error: NOT-FOUND: NOT_FOUND - no queue '%s' in vhost '%s'",
                $queue,
                $nativeConfig['brokers'][0]['vhost'] ?? '/',
            );
        }

        public function declareTopology(array $nativeConfig, string $workerProfile): ?string
        {
            $this->declareCalls++;

            return $this->declareError;
        }
    };

    $app->instance(DoctorProbe::class, $probe);

    return $probe;
}

/**
 * Registers one rabbit-rs queue connection under queue.connections.
 */
function topologyConnection(string $name = 'rabbitmq', array $overrides = []): void
{
    config()->set('queue.connections.'.$name, array_merge([
        'driver' => 'rabbit-rs',
        'queue' => 'orders',
    ], $overrides));
}

/**
 * Registers a connection carrying the management url and the dead-letter
 * wiring the management-API checks read.
 *
 * @param  array<string, mixed>  $overrides
 */
function topologyConnectionWithManagement(string $name = 'rabbitmq', array $overrides = []): void
{
    topologyConnection($name, array_merge([
        'management_url' => 'http://localhost:15672',
        'dead_letter' => ['exchange' => 'orders_dlx', 'queue' => 'orders_dlq'],
    ], $overrides));
}

/**
 * Fakes the three management-API collections the topology command reads.
 *
 * @param  list<array<string, mixed>>  $exchanges
 * @param  list<array<string, mixed>>  $queues
 * @param  list<array<string, mixed>>  $bindings
 */
function fakeManagementApi(array $exchanges = [], array $queues = [], array $bindings = []): void
{
    Http::fake([
        '*/api/exchanges/*' => Http::response($exchanges),
        '*/api/queues/*' => Http::response($queues),
        '*/api/bindings/*' => Http::response($bindings),
    ]);
}

function missingQueueError(string $queue): string
{
    return sprintf(
        "protocol error: AMQP soft error: NOT-FOUND: NOT_FOUND - no queue '%s' in vhost '/orders-eu'",
        $queue,
    );
}

describe('rabbit-rs:topology verify', function () {
    it('exits 0 and reports every plan item when the topology is complete', function () {
        bindFakeTopologyProbe($this->app);
        fakeManagementApi(
            exchanges: [[
                'name' => 'orders_dlx', 'type' => 'direct', 'durable' => true,
                'auto_delete' => false, 'arguments' => new stdClass,
            ]],
            queues: [[
                'name' => 'orders', 'durable' => true,
                'arguments' => ['x-queue-type' => 'quorum', 'x-dead-letter-exchange' => 'orders_dlx', 'x-dead-letter-routing-key' => 'orders'],
            ]],
            bindings: [[
                'source' => 'orders_dlx', 'destination' => 'orders_dlq',
                'destination_type' => 'queue', 'routing_key' => 'orders',
            ]],
        );
        topologyConnectionWithManagement();

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain("queue 'orders' exists")
            ->expectsOutputToContain("exchange 'orders_dlx'")
            ->assertExitCode(0);
    });

    it('exits 1 naming the config path when a queue is missing', function () {
        bindFakeTopologyProbe($this->app, missingQueues: ['orders']);
        topologyConnection();

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain("queue 'orders' is missing")
            ->expectsOutputToContain('queue.connections.rabbitmq.queue')
            ->assertExitCode(1);
    });

    it('fails with the config path when the connection does not compile', function () {
        bindFakeTopologyProbe($this->app);
        topologyConnection('broken', ['safety' => 'bogus']);

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain('must be safe')
            ->expectsOutputToContain('queue.connections.broken.safety')
            ->assertExitCode(1);
    });

    it('fails when the extension is not loaded', function () {
        $probe = bindFakeTopologyProbe($this->app);
        $reflection = new ReflectionClass($probe);
        // The fake reports loaded; rebind a probe whose extension check fails.
        $failing = new class extends DoctorProbe
        {
            public function extensionLoaded(): bool
            {
                return false;
            }
        };
        unset($probe, $reflection);
        $this->app->instance(DoctorProbe::class, $failing);

        topologyConnection();

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain('ext-rabbit_rs is not loaded')
            ->assertExitCode(1);
    });

    it('checks only the named connections with --connection', function () {
        bindFakeTopologyProbe($this->app, missingQueues: ['orders']);
        topologyConnection('rabbitmq');
        topologyConnection('other');

        Artisan::call('rabbit-rs:topology', ['--connection' => ['rabbitmq']]);

        expect(Artisan::output())->toContain('rabbitmq')
            ->and(Artisan::output())->not->toContain('other')
            ->and(Artisan::call('rabbit-rs:topology', ['--connection' => ['rabbitmq']]))->toBe(1);
    });

    it('fails with an unknown --connection value', function () {
        bindFakeTopologyProbe($this->app);
        topologyConnection();

        $this->artisan('rabbit-rs:topology', ['--connection' => ['nope']])
            ->expectsOutputToContain('nope')
            ->assertExitCode(1);
    });

    it('fails when no rabbit-rs connection is configured', function () {
        bindFakeTopologyProbe($this->app);

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain('No rabbit-rs queue connection')
            ->assertExitCode(1);
    });
});

describe('rabbit-rs:topology management api checks', function () {
    it('reports a missing dead-letter exchange with its config path', function () {
        bindFakeTopologyProbe($this->app);
        topologyConnectionWithManagement();
        fakeManagementApi();

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain("exchange 'orders_dlx' is missing")
            ->expectsOutputToContain('queue.connections.rabbitmq.dead_letter.exchange')
            ->assertExitCode(1);
    });

    it('reports an incorrect queue argument with its config path', function () {
        bindFakeTopologyProbe($this->app);
        topologyConnectionWithManagement();
        fakeManagementApi(queues: [[
            'name' => 'orders', 'durable' => true,
            'arguments' => ['x-queue-type' => 'classic'],
        ]]);

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain('x-queue-type')
            ->expectsOutputToContain('queue.connections.rabbitmq.queue_type')
            ->assertExitCode(1);
    });

    it('reports a missing dead-letter binding with its config path', function () {
        bindFakeTopologyProbe($this->app);
        topologyConnectionWithManagement();
        fakeManagementApi(
            exchanges: [[
                'name' => 'orders_dlx', 'type' => 'direct', 'durable' => true,
                'auto_delete' => false, 'arguments' => new stdClass,
            ]],
            queues: [[
                'name' => 'orders_dlq', 'durable' => true, 'arguments' => ['x-queue-type' => 'quorum'],
            ]],
        );

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain("binding 'orders_dlx' -> 'orders_dlq'")
            ->expectsOutputToContain('queue.connections.rabbitmq.dead_letter')
            ->assertExitCode(1);
    });

    it('warns that exchanges, bindings and arguments were not verified without a management url', function () {
        bindFakeTopologyProbe($this->app);
        topologyConnection();

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain('not verified')
            ->assertExitCode(0);
    });

    it('warns when the management api is unreachable', function () {
        bindFakeTopologyProbe($this->app);
        topologyConnectionWithManagement();
        Http::fake(['*' => Http::response('down', 500)]);

        $this->artisan('rabbit-rs:topology')
            ->expectsOutputToContain('management api')
            ->assertExitCode(0);
    });
});

describe('rabbit-rs:topology fix', function () {
    it('refuses --fix in verify mode without --force and declares nothing', function () {
        $probe = bindFakeTopologyProbe($this->app, missingQueues: ['orders']);
        topologyConnection(overrides: ['topology_mode' => 'verify']);

        $this->artisan('rabbit-rs:topology', ['--fix' => true])
            ->expectsOutputToContain('--fix refused')
            ->expectsOutputToContain('topology_mode=verify')
            ->expectsOutputToContain('--force')
            ->assertExitCode(1);

        expect($probe->declareCalls)->toBe(0);
    });

    it('refuses --fix in external mode without --force and declares nothing', function () {
        $probe = bindFakeTopologyProbe($this->app, missingQueues: ['orders']);
        topologyConnection(overrides: ['topology_mode' => 'external']);

        $this->artisan('rabbit-rs:topology', ['--fix' => true])
            ->expectsOutputToContain('--fix refused')
            ->expectsOutputToContain('topology_mode=external')
            ->assertExitCode(1);

        expect($probe->declareCalls)->toBe(0);
    });

    it('declares with --fix in declare mode', function () {
        $probe = bindFakeTopologyProbe($this->app);
        topologyConnection();

        $this->artisan('rabbit-rs:topology', ['--fix' => true])
            ->expectsOutputToContain('topology declared')
            ->assertExitCode(0);

        expect($probe->declareCalls)->toBe(1);
    });

    it('declares with --fix --force in external mode', function () {
        $probe = bindFakeTopologyProbe($this->app);
        topologyConnection(overrides: ['topology_mode' => 'external']);

        $this->artisan('rabbit-rs:topology', ['--fix' => true, '--force' => true])
            ->expectsOutputToContain('topology declared')
            ->assertExitCode(0);

        expect($probe->declareCalls)->toBe(1);
    });

    it('fails and keeps the missing items visible when the declaration fails', function () {
        bindFakeTopologyProbe($this->app, missingQueues: ['orders'], declareError: 'access refused');
        topologyConnection();

        $this->artisan('rabbit-rs:topology', ['--fix' => true])
            ->expectsOutputToContain('declaration failed')
            ->expectsOutputToContain('access refused')
            ->assertExitCode(1);
    });

    it('reports declaration success with a readiness warning when no worker is running', function () {
        // Bootstrap scenario (issue #195): the recovery generation declares
        // the topology, then the consumer readiness wait times out because
        // no worker consumes the profile — the declare step still succeeded.
        bindFakeTopologyProbe(
            $this->app,
            declareError: "consumer profile 'orders' did not become ready within 30s",
        );
        topologyConnection();

        // Artisan::output() empties the buffer on each fetch: capture once.
        Artisan::call('rabbit-rs:topology', ['--fix' => true]);
        $output = Artisan::output();

        expect($output)->toContain('topology declared')
            ->and($output)->toContain('did not become ready')
            ->and(Artisan::call('rabbit-rs:topology', ['--fix' => true]))->toBe(0);
    });

    it('exits 0 when --fix declares a queue that was missing at verify time', function () {
        $probe = bindFakeTopologyProbe($this->app, missingQueues: ['orders']);
        topologyConnection();

        $this->artisan('rabbit-rs:topology', ['--fix' => true])
            ->expectsOutputToContain('topology declared')
            ->assertExitCode(0);

        expect($probe->declareCalls)->toBe(1);
    });
});
