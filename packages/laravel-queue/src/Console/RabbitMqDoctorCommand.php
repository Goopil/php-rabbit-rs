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
        $this->checkWorkerCapacity($compiled);
        $brokerError = $this->checkBroker($compiled, $probe, $extensionUsable);
        $managementUsable = $this->checkManagement($config);
        $this->checkPublishOutcomes($compiled, $config, $managementUsable);
        $this->checkTopology($compiled, $brokerError);
        $this->checkDeadLetterCanary($name, $compiled, $config, $probe, $brokerError, $managementUsable);
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
     * Reports the AMQP connection math per supervisor: every `queue:work`
     * child owns one pool per broker of its connection (sockets are not
     * fork-safe, so cross-process connection sharing is impossible by
     * design), which makes the per-worker FD cost visible to operators.
     * The expected fleet is the `--workers` default (1 per connection;
     * there is no workers config key) — each extra `--workers` multiplies
     * the count. The broker count comes from the compiled native config.
     *
     * @param  array<string, mixed>  $compiled
     */
    private function checkWorkerCapacity(array $compiled): void
    {
        $brokers = is_array($compiled['native']['brokers'] ?? null) ? count($compiled['native']['brokers']) : 0;

        $this->emit('ok', sprintf(
            'capacity: 1 worker(s) × %d broker(s) → %d AMQP connection(s) per supervisor',
            $brokers,
            $brokers,
        ));
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
     * only (the API is not needed by the driver itself). Returns whether a
     * configured management API answered, so outcome checks that read broker
     * truth from it can skip cleanly when it is absent or unreachable.
     *
     * @param  array<string, mixed>  $config
     */
    private function checkManagement(array $config): bool
    {
        $url = $config['management_url'] ?? null;
        if (! is_string($url) || trim($url) === '') {
            return false;
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

            return false;
        }

        if (! $response->successful()) {
            $this->emit('warn', 'management api returned HTTP '.$response->status());

            return false;
        }

        $this->emit('ok', 'management api reachable');

        return true;
    }

    /**
     * Reports broker-truth unroutable publishes on the connection's publish
     * exchange, read from the management API. The pool's process-local
     * `returns_total`/`dropped_publications_total` counters cannot answer
     * this in a one-shot CLI (its own pool publishes nothing), and a
     * short-lived publishing process takes them to the grave; the exchange
     * counter is cross-process and survives process exit. Severity follows
     * the compiled safety mode: under safe, an unroutable publish that
     * surfaced nowhere is a contract break.
     *
     * @param  array<string, mixed>  $compiled
     * @param  array<string, mixed>  $config
     */
    private function checkPublishOutcomes(array $compiled, array $config, bool $managementUsable): void
    {
        if (! $managementUsable) {
            return;
        }

        $exchange = $compiled['routes']['default']['exchange'] ?? '';
        if (! is_string($exchange) || $exchange === '') {
            return; // the default exchange has no management-api counter of its own
        }

        $vhost = $compiled['native']['brokers'][0]['vhost'] ?? '/';
        $username = is_string($config['username'] ?? null) ? $config['username'] : 'guest';
        $password = is_string($config['password'] ?? null) ? $config['password'] : 'guest';

        try {
            $response = Http::withBasicAuth($username, $password)
                ->timeout(5)
                ->acceptJson()
                ->get(rtrim(trim((string) $config['management_url']), '/').'/api/exchanges/'.rawurlencode((string) $vhost).'/'.rawurlencode($exchange));
        } catch (\Throwable $e) {
            $this->emit('warn', 'publish outcomes not verified: management api unreachable — '.$e->getMessage());

            return;
        }

        if (! $response->successful()) {
            return; // a missing exchange is the topology check's finding, not an outcome
        }

        $returned = (int) ($response->json('message_stats.return_unroutable') ?? 0);
        if ($returned === 0) {
            $this->emit('ok', "no unroutable publishes on exchange '{$exchange}'");

            return;
        }

        $message = sprintf("%d unroutable publish(es) returned by the broker on exchange '%s'", $returned, $exchange);
        if (($compiled['publisher']['safety'] ?? 'safe') === 'safe') {
            $this->emit('fail', $message.' — safe mode published them as lost; fix the exchange→queue binding');

            return;
        }

        $this->emit('warn', $message.' — the safety mode is fire-and-forget: returns are silent by contract');
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
     * Behavioral dead-letter probe: publishes, terminally rejects, and
     * asserts DLQ delivery — the only check that exercises the whole chain
     * (queue args → DLX → binding → DLQ) instead of inspecting its parts.
     * Skipped without a reachable broker, without a usable management API
     * (DLQ delivery is verified through it), or when no dead_letter topology
     * is configured (checkTopology already warns about that gap).
     *
     * @param  array<string, mixed>  $compiled
     * @param  array<string, mixed>  $config
     */
    private function checkDeadLetterCanary(
        string $name,
        array $compiled,
        array $config,
        DoctorProbe $probe,
        ?string $brokerError,
        bool $managementUsable,
    ): void {
        $deadLetter = $compiled['topology']['dead_letter'] ?? null;
        if (! is_array($deadLetter) || $brokerError !== null || ! $managementUsable) {
            return;
        }

        $broker = $compiled['native']['brokers'][0]['name'] ?? 'default';
        $route = $compiled['routes']['default'];
        $queue = $compiled['native']['workers'][0]['subscriptions'][0]['queue'] ?? null;
        $workerProfile = (string) ($compiled['native']['workers'][0]['name'] ?? $name);
        if ($queue === null) {
            return;
        }

        $error = $probe->deadLetterCanary(
            $compiled['native'],
            (string) $broker,
            (string) ($route['exchange'] ?? ''),
            str_replace('{queue}', (string) $queue, (string) ($route['routing_key'] ?? '{queue}')),
            (string) $deadLetter['queue'],
            $workerProfile,
            $config,
            $this->horizonConsumersConfigured($name, $compiled),
        );

        if ($error !== null) {
            if ($error instanceof CanaryInconclusiveException) {
                $this->emit('warn', "dead-letter canary inconclusive: {$error->getMessage()}");

                return;
            }

            $this->emit('fail', "dead-letter canary failed: {$error->getMessage()} — dead-lettered messages would vanish (real broker traffic was produced)");

            return;
        }

        $this->emit('ok', 'dead-letter canary: delivered, rejected, and received on the configured DLQ');
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
     * Whether any Horizon supervisor of the current environment is configured
     * to consume this connection's queues. Running consumers can claim the
     * canary probe before the doctor's own consumer — knowledge the canary
     * needs to report an inconclusive run instead of a dead-letter failure.
     *
     * @param  array<string, mixed>  $compiled
     */
    private function horizonConsumersConfigured(string $name, array $compiled): bool
    {
        if (! class_exists(Horizon::class)) {
            return false;
        }

        $horizonConfig = config('horizon');
        if (! is_array($horizonConfig)) {
            return false;
        }

        $environment = $this->laravel->environment();
        $environments = $horizonConfig['environments'] ?? [];
        $envSupervisors = is_array($environments) ? ($environments[$environment] ?? []) : [];
        if (! is_array($envSupervisors) || $envSupervisors === []) {
            return false;
        }

        $queues = array_column($compiled['native']['workers'][0]['subscriptions'] ?? [], 'queue');

        return $this->auditSupervisors($name, $envSupervisors, $queues)['matched'];
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
     * shape the package pins (ext-rabbit_rs ^0.3.7): on 0.x the caret admits
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
