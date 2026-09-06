<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;
use Illuminate\Console\Command;
use Illuminate\Support\Arr;
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
            $connections = $this->targetedConnections();
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
     * @param array<string, mixed> $config
     */
    private function checkConnection(string $name, array $config, DoctorProbe $probe): bool
    {
        $this->info($name);

        try {
            $compiled = ConnectionCompiler::compile($name, $config, $this->packageDefaults());
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

        if ((bool) $this->option('fix')) {
            $ok = $this->applyFix($name, $compiled, $probe) && $ok;
        }

        return $ok;
    }

    /**
     * Passive queue-existence probe per subscription queue; a NOT-FOUND error
     * is a missing topology item, anything else is a broker failure.
     *
     * @param array<string, mixed> $compiled
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
            } else {
                $this->emit('fail', "queue '{$queue}' probe failed: {$error}");
            }
            $ok = false;
        }

        return $ok;
    }

    /**
     * Management-API verification: exchanges, dead-letter bindings, and
     * subscription-queue arguments. Advisory when the API is unreachable —
     * only actual mismatches fail the command.
     *
     * @param array<string, mixed> $config
     * @param array<string, mixed> $compiled
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

        $ok = true;
        $expectedType = (string) ($compiled['native']['queue_type'] ?? 'quorum');

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

        $deadLetter = $compiled['topology']['dead_letter'];
        if (is_array($deadLetter)) {
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
        }

        return $ok;
    }

    /**
     * @param array<string, mixed> $compiled
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
        if ($error !== null) {
            $this->error('declaration failed');
            $this->error($error);

            return false;
        }

        $this->info("topology declared (worker profile '{$workerProfile}')");

        return true;
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
            throw new \RuntimeException('management api returned HTTP '.$response->status());
        }

        $entries = $response->json();

        return is_array($entries) ? array_values(array_filter($entries, 'is_array')) : [];
    }

    /**
     * @param list<array<string, mixed>> $entries
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
     * @param list<array<string, mixed>> $bindings
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
     * Rabbit-rs connections targeted by --connection (comma-separated values
     * allowed), in config order; all of them when the option is absent.
     *
     * @return array<string, array<string, mixed>>
     */
    private function targetedConnections(): array
    {
        $rabbitRs = RabbitRsConnections::all();

        $names = [];
        foreach ((array) $this->option('connection') as $value) {
            foreach (explode(',', (string) $value) as $item) {
                $item = trim($item);
                if ($item !== '') {
                    $names[] = $item;
                }
            }
        }

        if ($names === []) {
            return $rabbitRs;
        }

        $unknown = array_values(array_unique(array_diff($names, array_keys($rabbitRs))));
        if ($unknown !== []) {
            throw new InvalidArgumentException(sprintf(
                'Unknown rabbit-rs connection(s): %s. Available rabbit-rs connections: %s',
                implode(', ', $unknown),
                implode(', ', array_keys($rabbitRs)),
            ));
        }

        return array_intersect_key($rabbitRs, array_flip($names));
    }

    /**
     * @return array<string, mixed>
     */
    private function packageDefaults(): array
    {
        $config = $this->laravel->make('config')->get('rabbit-rs');

        return Arr::except(is_array($config) ? $config : [], ['brokers', 'routes', 'workers']);
    }

    /**
     * @param 'ok'|'warn'|'fail' $status
     */
    private function emit(string $status, string $message): void
    {
        $this->line(sprintf('  [%s] %s', str_pad($status, 4), $message));
    }
}
