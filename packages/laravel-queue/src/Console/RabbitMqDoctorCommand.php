<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;
use Illuminate\Console\Command;
use Illuminate\Support\Facades\Http;
use InvalidArgumentException;
use Laravel\Horizon\Horizon;

/**
 * One-shot integration diagnostics: per rabbit-rs connection, runs the
 * package's actual config compilation and probes the environment (extension,
 * broker, Horizon, event listeners). Each check prints ok / warn / fail;
 * the command exits non-zero when any check fails (warnings are allowed).
 */
final class RabbitMqDoctorCommand extends Command
{
    protected $signature = 'rabbit-rs:doctor {--connection=* : Connections to check (default: all rabbit-rs connections)}';

    protected $description = 'Run integration diagnostics for Rabbit RS connections';

    /** Sentinel returned by the broker check when the probe could not run. */
    private const BROKER_SKIPPED = '__skipped__';

    private int $failures = 0;

    private int $warnings = 0;

    public function handle(DoctorProbe $probe): int
    {
        $connections = $this->resolveConnections();
        if ($connections === null) {
            return self::FAILURE;
        }

        $this->line('Rabbit RS Doctor');
        $this->line('');

        foreach ($connections as $name => $config) {
            $this->doctorConnection($name, $config, $probe);
        }

        $summary = sprintf('%d failure(s), %d warning(s)', $this->failures, $this->warnings);
        if ($this->failures > 0) {
            $this->error($summary);

            return self::FAILURE;
        }
        $this->line($summary);

        return self::SUCCESS;
    }

    /**
     * Resolves the targeted connections, reporting an error and returning
     * null when none apply: a listed connection is unknown, or no
     * rabbit-rs connection is configured.
     *
     * @return array<string, array<string, mixed>>|null
     */
    private function resolveConnections(): ?array
    {
        try {
            $connections = RabbitRsConnections::targeted((array) $this->option('connection'));
        } catch (InvalidArgumentException $e) {
            $this->error($e->getMessage());

            return null;
        }

        if ($connections === []) {
            $this->error('No rabbit-rs queue connection is configured in queue.connections.');

            return null;
        }

        return $connections;
    }

    /**
     * @param  array<string, mixed>  $config
     */
    private function doctorConnection(string $name, array $config, DoctorProbe $probe): void
    {
        $this->info($name);

        try {
            $compiled = ConnectionCompiler::compile($name, $config, RabbitRsConnections::packageDefaults());
        } catch (InvalidArgumentException $e) {
            $this->emit('fail', 'configuration: '.$e->getMessage());
            $this->line('');

            return;
        }

        $extensionUsable = $this->checkExtension($probe);
        $workerClass = $this->checkWorker($config);
        $brokerError = $this->checkBroker($compiled, $probe, $extensionUsable);
        $this->checkManagement($config);
        $this->checkTopology($compiled, $brokerError);
        $this->checkSafety($compiled);
        $this->checkHorizon($name, $workerClass, $compiled);
        $this->checkEvents();
        $this->line('');
    }

    private function checkExtension(DoctorProbe $probe): bool
    {
        if (! $probe->extensionLoaded()) {
            $this->emit('fail', 'ext-rabbit_rs is not loaded — the driver cannot connect (install the extension or check the CLI php.ini)');

            return false;
        }

        $version = $probe->extensionVersion() ?? 'unknown';
        $constraint = RabbitMqServiceProvider::EXTENSION_CONSTRAINT;
        if (! $this->satisfiesCaret($version, $constraint)) {
            $this->emit('fail', "extension version {$version} does not satisfy the composer requirement ext-rabbit_rs {$constraint}");

            return false;
        }

        $this->emit('ok', "extension {$version} satisfies ext-rabbit_rs {$constraint}");

        return true;
    }

    /**
     * @param  array<string, mixed>  $config
     */
    private function checkWorker(array $config): string
    {
        $defaults = RabbitRsConnections::packageDefaults();
        $class = RabbitMqConnector::workerClass($config, $defaults);

        if (($config['worker'] ?? null) === null && ($defaults['worker'] ?? 'default') !== 'default') {
            $this->emit('ok', "worker class: {$class} (resolved through the package defaults)");
        } else {
            $this->emit('ok', "worker class: {$class}");
        }

        return $class;
    }

    /**
     * @param  array<string, mixed>  $compiled
     * @return string|null null when the broker is reachable, the probe error,
     *                     or the BROKER_SKIPPED sentinel
     */
    private function checkBroker(array $compiled, DoctorProbe $probe, bool $extensionUsable): ?string
    {
        $broker = $compiled['native']['brokers'][0] ?? [];
        $endpoint = $broker['hosts'][0] ?? null;
        $label = is_array($endpoint)
            ? sprintf('%s:%d (vhost %s)', $endpoint['host'], $endpoint['port'], $broker['vhost'] ?? '/')
            : 'broker';

        if (! $extensionUsable) {
            $this->emit('warn', "broker {$label}: check skipped, ext-rabbit_rs is not usable");

            return self::BROKER_SKIPPED;
        }

        $error = $probe->broker($compiled['native']);
        if ($error === null) {
            $this->emit('ok', "broker {$label} reachable");

            return null;
        }

        $this->emit('fail', "broker {$label}: {$error}");

        return $error;
    }

    /**
     * Optional management API check: reachable when configured, advisory
     * only (the API is not needed by the driver itself).
     *
     * @param  array<string, mixed>  $config
     */
    private function checkManagement(array $config): void
    {
        $url = $config['management_url'] ?? null;
        if (! is_string($url) || trim($url) === '') {
            return;
        }

        $username = is_string($config['username'] ?? null) ? $config['username'] : 'guest';
        $password = is_string($config['password'] ?? null) ? $config['password'] : 'guest';

        try {
            $response = Http::withBasicAuth($username, $password)
                ->timeout(5)
                ->acceptJson()
                ->get(rtrim(trim($url), '/').'/api/overview');
        } catch (\Throwable $e) {
            $this->emit('warn', 'management api unreachable: '.$e->getMessage());

            return;
        }

        if (! $response->successful()) {
            $this->emit('warn', 'management api returned HTTP '.$response->status());

            return;
        }

        $this->emit('ok', 'management api reachable');
    }

    /**
     * @param  array<string, mixed>  $compiled
     */
    private function checkTopology(array $compiled, ?string $brokerError): void
    {
        $queues = array_column($compiled['native']['workers'][0]['subscriptions'] ?? [], 'queue');
        $route = $compiled['routes']['default'];
        $exchange = $route['exchange'];

        foreach ($queues as $queue) {
            $routingKey = str_replace('{queue}', $queue, $route['routing_key']);
            if ($exchange === '' && $routingKey !== $queue) {
                $this->emit(
                    'warn',
                    "publisher routing misaligned for queue '{$queue}': the default exchange routes on the "
                    ."queue name but the effective routing key is '{$routingKey}' — set routing_key to '{queue}' or null",
                );
            }
        }

        if ($brokerError === self::BROKER_SKIPPED) {
            $this->emit('warn', 'queue existence not verified: ext-rabbit_rs is not usable');
        } elseif ($brokerError !== null) {
            $this->emit('warn', 'queue existence not verified: broker unreachable');
        } elseif ($compiled['native']['topology_mode'] === 'external') {
            $this->emit('warn', 'topology_mode=external: queues are externally managed, their existence cannot be verified');
        } else {
            $this->emit('ok', sprintf('topology %s: %s', $compiled['native']['topology_mode'], implode(', ', $queues)));
        }

        $deadLetter = $compiled['topology']['dead_letter'];
        if ($deadLetter !== null) {
            if (in_array($deadLetter['queue'], $queues, true)) {
                $this->emit(
                    'warn',
                    "dead-letter queue '{$deadLetter['queue']}' is also a subscription queue — "
                    .'rejected messages loop back into the queue they came from',
                );
            } else {
                $this->emit('ok', "dead-letter wiring: {$deadLetter['exchange']} -> {$deadLetter['queue']}");
            }
        } elseif ($compiled['topology']['queue']['delivery_limit'] !== null) {
            $this->emit('fail', 'delivery_limit is set without dead_letter — rejected poison messages are dropped silently');
        } else {
            $this->emit('warn', 'no dead_letter configured: a worker crash before settlement redelivers the message forever');
        }
    }

    /**
     * @param  array<string, mixed>  $compiled
     */
    private function checkSafety(array $compiled): void
    {
        $publisher = $compiled['publisher'];
        $heartbeat = $compiled['native']['brokers'][0]['heartbeat'] ?? null;
        $message = sprintf(
            'safety: %s, confirm_timeout: %dms, heartbeat: %s',
            $publisher['safety'],
            $publisher['confirm_timeout'],
            is_int($heartbeat) ? $heartbeat.'s' : 'unknown',
        );

        if ($publisher['safety'] === 'safe') {
            $this->emit('ok', $message);

            return;
        }
        if ($publisher['safety'] === 'unsafe') {
            $this->emit('warn', $message.' — publishes without publisher confirms');

            return;
        }
        $this->emit('warn', $message.' — fire-and-forget: silent message loss is possible');
    }

    /**
     * @param  array<string, mixed>  $compiled
     */
    private function checkHorizon(string $name, string $workerClass, array $compiled): void
    {
        $horizonConfig = config('horizon');
        $installed = class_exists(Horizon::class);

        if ($workerClass !== HorizonRabbitMqQueue::class
            && ! ($installed && is_array($horizonConfig))
        ) {
            $this->emit('ok', 'horizon: not in use for this connection');

            return;
        }

        if (! $installed) {
            $this->emit('warn', 'worker=horizon but laravel/horizon is not installed — composer require laravel/horizon');
        }

        $environment = $this->laravel->environment();
        $environments = is_array($horizonConfig) ? ($horizonConfig['environments'] ?? []) : [];
        $envSupervisors = is_array($environments) ? ($environments[$environment] ?? []) : [];
        if (! is_array($envSupervisors) || $envSupervisors === []) {
            $this->emit('warn', "no supervisors configured for the {$environment} environment in config/horizon.php");

            return;
        }

        $queues = array_column($compiled['native']['workers'][0]['subscriptions'] ?? [], 'queue');
        ['matched' => $matched, 'ok' => $ok] = $this->auditSupervisors($name, $envSupervisors, $queues);

        if (! $matched) {
            $this->emit('warn', sprintf("no Horizon supervisor consumes this connection's queues (%s)", implode(', ', $queues)));

            return;
        }
        if ($ok) {
            $this->emit('ok', 'horizon supervisors aligned with the connection subscriptions');
        }
    }

    /**
     * Audits the Horizon supervisors of the current environment against the
     * connection's subscription queues: a supervisor counts as consuming
     * this connection when it targets it and shares at least one queue.
     *
     * @param  array<array-key, mixed>  $envSupervisors
     * @param  list<string>  $queues
     * @return array{matched: bool, ok: bool} whether a supervisor consumes
     *                                        the connection and whether the wiring is aligned
     */
    private function auditSupervisors(string $name, array $envSupervisors, array $queues): array
    {
        $matched = false;
        $ok = true;

        foreach ($envSupervisors as $supervisorName => $supervisor) {
            if (! is_array($supervisor) || ($supervisor['connection'] ?? null) !== $name) {
                continue;
            }
            $supervisorQueues = $supervisor['queue'] ?? [];
            $supervisorQueues = is_string($supervisorQueues)
                ? array_map('trim', explode(',', $supervisorQueues))
                : (array) $supervisorQueues;
            if (array_intersect($supervisorQueues, $queues) === []) {
                continue;
            }
            $matched = true;
            $label = is_string($supervisorName) ? $supervisorName : '(unnamed)';

            $ok = $this->auditSupervisor($supervisor, $supervisorQueues, $label, $queues) && $ok;
        }

        return ['matched' => $matched, 'ok' => $ok];
    }

    /**
     * Audits one consuming supervisor: queues it lists beyond the
     * connection's subscriptions, and auto-scaling modes that sample the
     * native connection.
     *
     * @param  array<string, mixed>  $supervisor
     * @param  list<string>  $supervisorQueues
     * @param  list<string>  $queues
     */
    private function auditSupervisor(array $supervisor, array $supervisorQueues, string $label, array $queues): bool
    {
        $ok = true;

        $unknownQueues = array_diff($supervisorQueues, $queues);
        if ($unknownQueues !== []) {
            $ok = false;
            $this->emit(
                'warn',
                sprintf(
                    'supervisor %s lists queue(s) %s that are not subscriptions of this connection — jobs for them will never be consumed by its worker profiles',
                    $label,
                    implode(', ', $unknownQueues),
                ),
            );
        }

        $balance = $supervisor['balance'] ?? false;
        if (in_array($balance, ['auto', 'container'], true)) {
            $ok = false;
            $this->emit(
                'warn',
                sprintf(
                    'supervisor %s uses balance=%s: auto-scaling samples readyNow() on the queue connection, available since rabbit-rs-laravel 0.1.2',
                    $label,
                    $balance,
                ),
            );
        }

        return $ok;
    }

    private function checkEvents(): void
    {
        $dispatcher = $this->laravel->make('events');

        foreach ([BackpressureDetected::class, ConnectionStateChanged::class] as $event) {
            $short = substr($event, (int) strrpos($event, '\\') + 1);
            if ($dispatcher->hasListeners($event)) {
                $this->emit('ok', "event listener registered for {$short}");
            } else {
                $this->emit('warn', "no listener registered for {$short} — the event is dispatched but unobserved");
            }
        }
    }

    /**
     * Composer caret constraint check, limited to the ^major.minor[.patch]
     * shape the package pins (ext-rabbit_rs ^0.2.3): on 0.x the caret admits
     * only the declared minor. Unknown shapes pass — the doctor reports the
     * version instead of guessing.
     */
    private function satisfiesCaret(string $version, string $constraint): bool
    {
        if (preg_match('/^\^(\d+)\.(\d+)(?:\.(\d+))?$/', $constraint, $matches) !== 1) {
            return true;
        }

        $major = (int) $matches[1];
        $minor = (int) $matches[2];
        $floor = sprintf('%d.%d.%d', $major, $minor, (int) ($matches[3] ?? 0));
        $ceiling = $major > 0
            ? sprintf('%d.0.0', $major + 1)
            : sprintf('0.%d.0', $minor + 1);

        return version_compare($version, $floor, '>=')
            && version_compare($version, $ceiling, '<');
    }

    /**
     * @param  'ok'|'warn'|'fail'  $status
     */
    private function emit(string $status, string $message): void
    {
        if ($status === 'fail') {
            $this->failures++;
        } elseif ($status === 'warn') {
            $this->warnings++;
        }

        $this->line(sprintf('  [%s] %s', str_pad($status, 4), $message));
    }
}
