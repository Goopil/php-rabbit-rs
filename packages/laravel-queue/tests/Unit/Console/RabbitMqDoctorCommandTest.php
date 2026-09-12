<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\DoctorProbe;
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Illuminate\Support\Facades\Artisan;
use Illuminate\Support\Facades\Event;
use Illuminate\Support\Facades\Http;

const HORIZON_QUEUE_CLASS = 'Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue';
const BASE_QUEUE_CLASS = 'Goopil\RabbitRs\Laravel\RabbitMqQueue';

/**
 * Binds a fake environment probe: the unit suite runs without ext-rabbit_rs
 * and without a broker, so the doctor's extension and broker probes are
 * substituted with configurable fakes.
 */
function bindFakeProbe($app, bool $loaded = true, ?string $version = '0.3.1', ?string $brokerError = null): void
{
    $app->instance(DoctorProbe::class, new class($loaded, $version, $brokerError) extends DoctorProbe
    {
        public function __construct(
            private readonly bool $loaded,
            private readonly ?string $version,
            private readonly ?string $brokerError,
        ) {}

        public function extensionLoaded(): bool
        {
            return $this->loaded;
        }

        public function extensionVersion(): ?string
        {
            return $this->version;
        }

        public function broker(array $nativeConfig): ?string
        {
            return $this->brokerError;
        }
    });
}

/**
 * Registers one rabbit-rs queue connection under queue.connections.
 */
function doctorConnection(string $name = 'rabbitmq', array $overrides = []): void
{
    config()->set('queue.connections.'.$name, array_merge([
        'driver' => 'rabbit-rs',
        'queue' => 'orders',
    ], $overrides));
}

/**
 * Registers one Horizon supervisor in the real config shape, under the
 * environment the doctor reads (phpunit.xml sets APP_ENV=testing).
 */
function horizonSupervisor(array $supervisor, string $key = 'supervisor-1'): void
{
    config()->set('horizon', ['environments' => ['testing' => [$key => $supervisor]]]);
}

beforeEach(function () {
    bindFakeProbe($this->app);
});

describe('rabbit-rs:doctor worker class resolution', function () {
    it('reports the inherited worker without warning when it comes from the package defaults', function () {
        doctorConnection();
        config()->set('rabbit-rs.worker', 'horizon');

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain(HORIZON_QUEUE_CLASS)
            ->and($output)->toContain('worker class:')
            ->and($output)->not->toContain('inheritance trap')
            ->and(Artisan::call('rabbit-rs:doctor'))->toBe(0);
    });

    it('reports the Horizon worker class as ok when worker is set on the connection', function () {
        doctorConnection(overrides: ['worker' => 'horizon']);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain(HORIZON_QUEUE_CLASS)
            ->assertExitCode(0);
    });

    it('does not warn when the inherited worker resolves to the default queue', function () {
        doctorConnection();
        config()->set('rabbit-rs.worker', 'default');

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain(BASE_QUEUE_CLASS)
            ->assertExitCode(0);
    });
});

describe('rabbit-rs:doctor worker capacity', function () {
    it('reports the per-supervisor AMQP connection math', function () {
        doctorConnection();

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('1 worker(s) × 1 broker(s) → 1 AMQP connection(s) per supervisor')
            ->assertExitCode(0);
    });
});

describe('rabbit-rs:doctor broker probe', function () {
    it('fails when the broker is unreachable and still runs the other checks', function () {
        bindFakeProbe($this->app, brokerError: 'connection refused');
        doctorConnection();

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('connection refused')
            ->expectsOutputToContain(BASE_QUEUE_CLASS)
            ->assertExitCode(1);
    });

    it('skips the broker probe with a warning when the extension is missing', function () {
        bindFakeProbe($this->app, loaded: false, version: null);
        doctorConnection();

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('ext-rabbit_rs')
            ->assertExitCode(1);
    });

    it('fails when the extension version drifts from the composer constraint', function () {
        bindFakeProbe($this->app, loaded: true, version: '0.1.3');
        doctorConnection();

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('^0.3.1')
            ->assertExitCode(1);
    });

    it('reports the extension version when it satisfies the constraint', function () {
        doctorConnection();

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('0.3.1')
            ->assertExitCode(0);
    });
});

describe('rabbit-rs:doctor horizon check', function () {
    it('warns when worker=horizon but Horizon is not installed', function () {
        doctorConnection(overrides: ['worker' => 'horizon']);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('laravel/horizon')
            ->assertExitCode(0);
    });

    it('does not warn no supervisors when the running environment configures one for this connection', function () {
        doctorConnection(overrides: ['worker' => 'horizon']);
        horizonSupervisor(['connection' => 'rabbitmq', 'queue' => ['orders']]);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain('horizon supervisors aligned')
            ->and($output)->not->toContain('no supervisors configured')
            ->and(Artisan::call('rabbit-rs:doctor'))->toBe(0);
    });

    it('flags supervisor queues outside the connection subscriptions', function () {
        doctorConnection(overrides: ['worker' => 'horizon']);
        horizonSupervisor(['connection' => 'rabbitmq', 'queue' => ['orders', 'ghost-queue']]);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('ghost-queue')
            ->assertExitCode(0);
    });

    it('reminds about readyNow for auto-balancing supervisors', function () {
        doctorConnection(overrides: ['worker' => 'horizon']);
        horizonSupervisor(['connection' => 'rabbitmq', 'queue' => ['orders'], 'balance' => 'auto']);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('readyNow')
            ->assertExitCode(0);
    });

    it('does not count a supervisor on another connection as consuming this one', function () {
        doctorConnection(overrides: ['worker' => 'horizon']);
        horizonSupervisor(['connection' => 'redis', 'queue' => ['orders']]);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain("no Horizon supervisor consumes this connection's queues")
            ->and(Artisan::call('rabbit-rs:doctor'))->toBe(0);
    });

    it('names the supervisor by its config key', function () {
        doctorConnection(overrides: ['worker' => 'horizon']);
        horizonSupervisor(['connection' => 'rabbitmq', 'queue' => ['orders', 'ghost-queue']], 'supervisor-orders');

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('supervisor-orders')
            ->assertExitCode(0);
    });
});

describe('rabbit-rs:doctor management api check', function () {
    it('warns when a configured management api is unreachable', function () {
        Http::fake(['*' => Http::response('down', 500)]);
        doctorConnection(overrides: ['management_url' => 'http://localhost:15672']);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('management api')
            ->assertExitCode(0);
    });

    it('reports a configured management api as reachable', function () {
        Http::fake(['*' => Http::response([], 200)]);
        doctorConnection(overrides: ['management_url' => 'http://localhost:15672']);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('management api reachable')
            ->assertExitCode(0);
    });

    it('does not mention the management api when none is configured', function () {
        doctorConnection();

        Artisan::call('rabbit-rs:doctor');

        expect(Artisan::output())->not->toContain('management api');
    });
});

describe('rabbit-rs:doctor events check', function () {
    it('reports registered event listeners as ok', function () {
        doctorConnection();
        Event::listen(BackpressureDetected::class, fn () => null);
        Event::listen(ConnectionStateChanged::class, fn () => null);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('BackpressureDetected')
            ->assertExitCode(0);
    });

    it('warns when package events have no listeners', function () {
        doctorConnection();

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('BackpressureDetected')
            ->assertExitCode(0);
    });
});

describe('rabbit-rs:doctor targeting and exit codes', function () {
    it('exits 0 when only warnings fire', function () {
        doctorConnection();

        $this->artisan('rabbit-rs:doctor')->assertExitCode(0);
    });

    it('checks only the named connections with --connection', function () {
        bindFakeProbe($this->app, brokerError: 'connection refused');
        doctorConnection('rabbitmq');
        doctorConnection('other');

        Artisan::call('rabbit-rs:doctor', ['--connection' => ['rabbitmq']]);
        $output = Artisan::output();

        expect($output)->toContain('rabbitmq')
            ->and($output)->not->toContain('other')
            ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => ['rabbitmq']]))->toBe(1);
    });

    it('fails with an unknown --connection value', function () {
        doctorConnection();

        $this->artisan('rabbit-rs:doctor', ['--connection' => ['nope']])
            ->expectsOutputToContain('nope')
            ->assertExitCode(1);
    });

    it('fails when no rabbit-rs connection is configured', function () {
        config()->set('queue.connections.rabbitmq', null);

        $this->artisan('rabbit-rs:doctor')->assertExitCode(1);
    });

    it('reports a connection that fails to compile and keeps checking the others', function () {
        doctorConnection('rabbitmq');
        config()->set('queue.connections.broken', [
            'driver' => 'rabbit-rs',
            'queue' => 'x',
            'safety' => 'bogus',
        ]);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('must be safe')
            ->assertExitCode(1);
    });

    it('reports effective safety settings after compilation', function () {
        doctorConnection(overrides: ['safety' => 'blind']);

        $this->artisan('rabbit-rs:doctor')
            ->expectsOutputToContain('safety: blind')
            ->assertExitCode(0);
    });
});
