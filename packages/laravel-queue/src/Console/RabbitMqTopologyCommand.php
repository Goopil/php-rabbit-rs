<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Exceptions\ManagementApiException;
use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;
use Illuminate\Console\Command;
use Illuminate\Support\Facades\Http;
use InvalidArgumentException;

/**
 * Preflight topology check: per rabbit-rs connection, verifies that the
 * broker topology the compiled config promises actually exists — subscription
 * queues through a passive native probe, and (when management_url is set)
 * exchanges, dead-letter bindings and queue arguments through the management
 * API. Exits non-zero when anything is missing.
 *
 * With --fix the missing topology is declared by opening a transient consumer
 * on the worker profile (full declare in declare mode); in verify/external
 * mode --fix is refused unless --force is passed.
 */
final class RabbitMqTopologyCommand extends Command
{
    protected $signature = 'rabbit-rs:topology
        {--connection=* : Connections to check (default: all rabbit-rs connections)}
        {--fix : Declare missing queues/exchanges/bindings via a transient consumer}
        {--force : Allow --fix outside declare mode}';

    protected $description = 'Verify (and optionally declare) the broker topology of Rabbit RS connections';

    public function handle(DoctorProbe $probe): int
    {
        try {
            $connections = RabbitRsConnections::targeted((array) $this->option('connection'));
        } catch (InvalidArgumentException $e) {
            $this->error($e->getMessage());

            return self::FAILURE;
        }

        if ($connections === []) {
            $this->error('No rabbit-rs queue connection is configured in queue.connections.');

            return self::FAILURE;
        }

        $failed = false;
        foreach ($connections as $name => $config) {
            if (! $this->checkConnection($name, $config, $probe)) {
                $failed = true;
            }
        }

        return $failed ? self::FAILURE : self::SUCCESS;
    }

    /**
     * @param  array<string, mixed>  $config
     */
    private function checkConnection(string $name, array $config, DoctorProbe $probe): bool
    {
        $this->info($name);

        try {
            $compiled = ConnectionCompiler::compile($name, $config, RabbitRsConnections::packageDefaults());
        } catch (InvalidArgumentException $e) {
            // The compiler's message is "<config path>: <problem>"; splitting
            // keeps the actionable path on its own console line.
            $parts = explode(': ', $e->getMessage(), 2);
            $this->error($parts[1] ?? $e->getMessage());
            $this->error('check '.$parts[0]);

            return false;
        }

        if (! $probe->extensionLoaded()) {
            $this->error('ext-rabbit_rs is not loaded — topology checks require the native extension');

            return false;
        }

        $ok = $this->verifyQueues($name, $compiled, $probe);
        $ok = $this->verifyManagement($name, $config, $compiled) && $ok;

        // A successful declare resolves the missing items verify reported:
        // report the connection as fixed instead of carrying pre-fix
        // failures (the bootstrap scenario --fix exists for, issue #195).
        return ((bool) $this->option('fix')) ? $this->applyFix($name, $compiled, $probe) : $ok;
    }

    /**
     * Passive queue-existence probe per subscription queue; a NOT-FOUND error
     * is a missing topology item, anything else leaves existence unverifiable
     * (broker unreachable) and only warns.
     *
     * @param  array<string, mixed>  $compiled
     */
    private function verifyQueues(string $name, array $compiled, DoctorProbe $probe): bool
    {
        $broker = (string) ($compiled['native']['brokers'][0]['name'] ?? 'default');
        $ok = true;

        foreach ($compiled['native']['workers'][0]['subscriptions'] ?? [] as $subscription) {
            $queue = (string) $subscription['queue'];
            $error = $probe->queueSize($compiled['native'], $broker, $queue);

            if ($error === null) {
                $this->emit('ok', "queue '{$queue}' exists");

                continue;
            }
            if (str_contains($error, 'NOT-FOUND')) {
                $this->emit('fail', "queue '{$queue}' is missing");
                $this->emit('fail', "check queue.connections.{$name}.queue");
                $ok = false;
            } else {
                $this->emit('warn', "queue '{$queue}' probe failed: {$error}");
            }
        }

        return $ok;
    }

    /**
     * Management-API verification: exchanges, dead-letter bindings, and
     * subscription-queue arguments. Advisory when the API is unreachable —
     * only actual mismatches fail the command.
     *
     * @param  array<string, mixed>  $config
     * @param  array<string, mixed>  $compiled
     */
    private function verifyManagement(string $name, array $config, array $compiled): bool
    {
        $url = $config['management_url'] ?? null;
        if (! is_string($url) || trim($url) === '') {
            $this->emit('warn', 'management api not configured: exchanges, bindings and queue arguments were not verified');

            return true;
        }

        $username = is_string($config['username'] ?? null) ? $config['username'] : 'guest';
        $password = is_string($config['password'] ?? null) ? $config['password'] : 'guest';
        $base = rtrim(trim($url), '/');
        $vhost = rawurlencode((string) ($compiled['native']['brokers'][0]['vhost'] ?? '/'));

        try {
            $exchanges = $this->managementEntries($base."/api/exchanges/{$vhost}", $username, $password);
            $queues = $this->managementEntries($base."/api/queues/{$vhost}", $username, $password);
            $bindings = $this->managementEntries($base."/api/bindings/{$vhost}", $username, $password);
        } catch (\Throwable $e) {
            $this->emit('warn', 'management api unreachable: '.$e->getMessage());

            return true;
        }

        $ok = $this->verifyQueueTypes($name, $queues, $compiled);
        $ok = $this->verifyRouteTopology($name, $exchanges, $bindings, $compiled) && $ok;

        return $this->verifyDeadLetterWiring($name, $exchanges, $bindings, $compiled) && $ok;
    }

    /**
     * Publish-route verification: the route exchange exists and each
     * subscription on the route's broker is bound to it with the resolved
     * routing key. Mirrors what the topology plan declares in declare mode
     * (issue #205): a deleted binding silently unrouted publishes while
     * verify stayed green. The default exchange (empty route exchange) needs
     * neither declaration nor binding.
     *
     * @param  list<array<string, mixed>>  $exchanges
     * @param  list<array<string, mixed>>  $bindings
     * @param  array<string, mixed>  $compiled
     */
    private function verifyRouteTopology(
        string $name,
        array $exchanges,
        array $bindings,
        array $compiled,
    ): bool {
        $routes = $compiled['native']['routes'] ?? null;
        if (! is_array($routes)) {
            return true;
        }

        $ok = true;
        foreach ($routes as $route) {
            if (! is_array($route)) {
                continue;
            }
            $exchange = (string) ($route['exchange'] ?? '');
            if ($exchange === '') {
                continue;
            }

            $broker = (string) ($route['broker'] ?? '');
            $subscriptions = array_filter(
                $compiled['native']['workers'][0]['subscriptions'] ?? [],
                static fn (array $subscription): bool => (string) ($subscription['broker'] ?? '') === $broker,
            );

            if ($this->findByName($exchanges, $exchange) === null) {
                $this->emit('fail', "exchange '{$exchange}' is missing");
                $this->emit('fail', "check queue.connections.{$name}.exchange");
                $ok = false;
            } else {
                $this->emit('ok', "exchange '{$exchange}' declared");
            }

            foreach ($subscriptions as $subscription) {
                $queue = (string) $subscription['queue'];
                $routingKey = str_replace('{queue}', $queue, (string) ($route['routing_key'] ?? ''));
                if ($this->routeBindingExists($bindings, $exchange, $queue, $routingKey)) {
                    $this->emit('ok', "route binding '{$exchange}' -> '{$queue}' declared");

                    continue;
                }
                $this->emit('fail', "binding '{$exchange}' -> '{$queue}' (routing key '{$routingKey}') is missing");
                $this->emit('fail', "check queue.connections.{$name}.exchange");
                $ok = false;
            }
        }

        return $ok;
    }

    /**
     * Route-binding existence including the routing key: a direct exchange
     * with the wrong key silently unroutables every publish, so the check
     * must match the exact (source, destination, routing_key) triple.
     *
     * @param  list<array<string, mixed>>  $bindings
     */
    private function routeBindingExists(array $bindings, string $source, string $destination, string $routingKey): bool
    {
        foreach ($bindings as $entry) {
            if (($entry['source'] ?? null) === $source
                && ($entry['destination'] ?? null) === $destination
                && ($entry['destination_type'] ?? null) === 'queue'
                && ($entry['routing_key'] ?? null) === $routingKey
            ) {
                return true;
            }
        }

        return false;
    }

    /**
     * Per-queue x-queue-type verification against the management listing; a
     * queue absent from the listing is only warned (its arguments unknown).
     *
     * @param  list<array<string, mixed>>  $queues
     * @param  array<string, mixed>  $compiled
     */
    private function verifyQueueTypes(string $name, array $queues, array $compiled): bool
    {
        $expectedType = (string) ($compiled['native']['queue_type'] ?? 'quorum');
        $ok = true;

        foreach ($compiled['native']['workers'][0]['subscriptions'] ?? [] as $subscription) {
            $queue = (string) $subscription['queue'];
            $entry = $this->findByName($queues, $queue);
            if ($entry === null) {
                $this->emit('warn', "management api: queue '{$queue}' not listed — arguments not verified");

                continue;
            }
            $actualType = (string) ($entry['arguments']['x-queue-type'] ?? 'classic');
            if ($actualType !== $expectedType) {
                $this->emit('fail', "queue '{$queue}' has x-queue-type={$actualType}, expected {$expectedType}");
                $this->emit('fail', "check queue.connections.{$name}.queue_type");
                $ok = false;
            }
        }

        return $ok;
    }

    /**
     * Dead-letter wiring verification: the dead-letter exchange exists and
     * the exchange -> dead-letter-queue binding is declared. No-op without
     * a configured dead_letter.
     *
     * @param  list<array<string, mixed>>  $exchanges
     * @param  list<array<string, mixed>>  $bindings
     * @param  array<string, mixed>  $compiled
     */
    private function verifyDeadLetterWiring(string $name, array $exchanges, array $bindings, array $compiled): bool
    {
        $deadLetter = $compiled['topology']['dead_letter'];
        if (! is_array($deadLetter)) {
            return true;
        }

        $ok = true;
        $exchange = (string) $deadLetter['exchange'];
        if ($this->findByName($exchanges, $exchange) === null) {
            $this->emit('fail', "exchange '{$exchange}' is missing");
            $this->emit('fail', "check queue.connections.{$name}.dead_letter.exchange");
            $ok = false;
        } else {
            $this->emit('ok', "exchange '{$exchange}' declared");
        }

        $dlq = (string) $deadLetter['queue'];
        if (! $this->bindingExists($bindings, $exchange, $dlq)) {
            $this->emit('fail', "binding '{$exchange}' -> '{$dlq}' is missing");
            $this->emit('fail', "check queue.connections.{$name}.dead_letter");
            $ok = false;
        } else {
            $this->emit('ok', "dead-letter binding '{$exchange}' -> '{$dlq}' declared");
        }

        return $ok;
    }

    /**
     * @param  array<string, mixed>  $compiled
     */
    private function applyFix(string $name, array $compiled, DoctorProbe $probe): bool
    {
        $mode = (string) $compiled['native']['topology_mode'];
        if ($mode !== 'declare' && ! (bool) $this->option('force')) {
            $this->error('--fix refused');
            $this->error("topology_mode={$mode} manages topology externally");
            $this->error('re-run with --force to declare anyway');

            return false;
        }

        $workerProfile = (string) ($compiled['native']['workers'][0]['name'] ?? $name);
        $error = $probe->declareTopology($compiled['native'], $workerProfile);
        if ($error !== null && ! $this->isConsumerReadinessTimeout($error)) {
            $this->error('declaration failed');
            $this->error($error);

            return false;
        }

        if ($error !== null) {
            // The declare probe rides a transient consumer: the recovery
            // generation declares the topology before consumer channels
            // start, so a readiness timeout (e.g. no worker running for the
            // profile) leaves the declaration successful with readiness
            // unconfirmed. Warn instead of failing the bootstrap scenario.
            $this->warn("topology declared (worker profile '{$workerProfile}'); consumer readiness not confirmed: {$error}");
        } else {
            $this->info("topology declared (worker profile '{$workerProfile}')");
        }

        return true;
    }

    /**
     * The declare probe only opens a consumer, so the native readiness
     * timeout ("consumer profile '...' did not become ready within ...") is
     * the one error that means declared-but-readiness-unconfirmed rather
     * than a declaration failure.
     */
    private function isConsumerReadinessTimeout(string $error): bool
    {
        return str_contains($error, 'did not become ready within');
    }

    /**
     * Fetches one management-API collection, throwing on transport failure or
     * a non-successful status so the caller can warn instead of fail.
     *
     * @return list<array<string, mixed>>
     */
    private function managementEntries(string $url, string $username, string $password): array
    {
        $response = Http::withBasicAuth($username, $password)
            ->timeout(5)
            ->acceptJson()
            ->get($url);

        if (! $response->successful()) {
            throw new ManagementApiException('management api returned HTTP '.$response->status());
        }

        $entries = $response->json();

        return is_array($entries) ? array_values(array_filter($entries, 'is_array')) : [];
    }

    /**
     * @param  list<array<string, mixed>>  $entries
     * @return array<string, mixed>|null
     */
    private function findByName(array $entries, string $name): ?array
    {
        foreach ($entries as $entry) {
            if (($entry['name'] ?? null) === $name) {
                return $entry;
            }
        }

        return null;
    }

    /**
     * @param  list<array<string, mixed>>  $bindings
     */
    private function bindingExists(array $bindings, string $source, string $destination): bool
    {
        foreach ($bindings as $entry) {
            if (($entry['source'] ?? null) === $source
                && ($entry['destination'] ?? null) === $destination
                && ($entry['destination_type'] ?? null) === 'queue'
            ) {
                return true;
            }
        }

        return false;
    }

    /**
     * @param  'ok'|'warn'|'fail'  $status
     */
    private function emit(string $status, string $message): void
    {
        $this->line(sprintf('  [%s] %s', str_pad($status, 4), $message));
    }
}
