<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Console\Command;
use Illuminate\Support\Arr;
use Illuminate\Support\Facades\Http;

final class RabbitMqStatusCommand extends Command
{
    protected $signature = 'rabbit-rs:status {--format=human : Output format (human or json)}';

    protected $description = 'Display Rabbit RS native pool diagnostics';

    public function handle(NativePoolFactory $pools): int
    {
        $format = $this->option('format');

        $stats = $this->collectStats($pools);

        if ($stats === false) {
            return self::FAILURE;
        }

        $queueStats = $this->collectQueueStats();

        if ($format === 'json') {
            $stats['queue_stats'] = $queueStats;
            $json = json_encode($stats, JSON_PRETTY_PRINT);
            foreach (explode("\n", $json) as $line) {
                $this->line($line);
            }

            return self::SUCCESS;
        }

        $this->displayHuman($stats, $queueStats);

        return self::SUCCESS;
    }

    /**
     * Native pool metrics per rabbit-rs connection, keyed by connection
     * name. Same-process only: each connection owns one pool.
     *
     * @return array<string, mixed>|false
     */
    private function collectStats(NativePoolFactory $pools): array|false
    {
        $connections = $this->rabbitRsConnections();

        if ($connections === []) {
            $this->error('Failed to collect stats: no rabbit-rs queue connection is configured');

            return false;
        }

        $stats = [];
        try {
            foreach ($connections as $name => $config) {
                $compiled = ConnectionCompiler::compile($name, $config, $this->packageDefaults());
                $stats[$name] = $pools->make($compiled['native'])->stats();
            }
        } catch (\Throwable $e) {
            $this->error('Failed to collect stats: '.$e->getMessage());

            return false;
        }

        return $stats;
    }

    /**
     * Cross-process queue counters from the RabbitMQ management API.
     *
     * Redeliveries are an approximate duplicate signal: they also count
     * crash requeues. Requires queue.connections.<name>.management_url.
     *
     * @return array{management_url_configured: bool, queues: list<array<string, mixed>>}
     */
    private function collectQueueStats(): array
    {
        $pairs = [];
        $hasManagementUrl = false;
        foreach ($this->rabbitRsConnections() as $name => $config) {
            $url = $config['management_url'] ?? null;
            if (! is_string($url) || trim($url) === '') {
                continue;
            }
            $hasManagementUrl = true;

            foreach ($this->definedQueues($config) as $queue) {
                $pairs[$name.'|'.$queue] = [
                    'connection' => $name,
                    'queue' => $queue,
                    'config' => $config,
                    'url' => rtrim(trim($url), '/'),
                ];
            }
        }
        ksort($pairs);

        if ($pairs === []) {
            return ['management_url_configured' => $hasManagementUrl, 'queues' => []];
        }

        $queues = [];
        foreach ($pairs as ['connection' => $name, 'queue' => $queue, 'config' => $config, 'url' => $url]) {
            $queues[] = $this->fetchQueueStats($url, $config, $name, $queue);
        }

        return ['management_url_configured' => true, 'queues' => $queues];
    }

    /**
     * Queues a connection consumes: its `queue` key plus every queue of its
     * `subscriptions` escape hatch.
     *
     * @param array<string, mixed> $config
     * @return list<string>
     */
    private function definedQueues(array $config): array
    {
        $queues = [];

        if (isset($config['queue']) && is_string($config['queue']) && $config['queue'] !== '') {
            $queues[] = $config['queue'];
        }

        foreach ($config['subscriptions'] ?? [] as $subscription) {
            if (is_array($subscription) && is_string($subscription['queue'] ?? null) && $subscription['queue'] !== '') {
                $queues[] = $subscription['queue'];
            }
        }

        return array_values(array_unique($queues));
    }

    /**
     * @return array<string, mixed>
     */
    private function fetchQueueStats(string $baseUrl, array $connection, string $connectionName, string $queue): array
    {
        $entry = ['connection' => $connectionName, 'queue' => $queue];

        $username = $connection['username'] ?? '';
        $password = $connection['password'] ?? '';
        $vhost = is_string($connection['vhost'] ?? null) ? $connection['vhost'] : '/';

        $url = $baseUrl.'/api/queues/'.rawurlencode($vhost).'/'.rawurlencode($queue);

        try {
            $response = Http::withBasicAuth(
                is_string($username) ? $username : '',
                is_string($password) ? $password : '',
            )
                ->timeout(5)
                ->acceptJson()
                ->get($url);
        } catch (\Throwable $e) {
            return $entry + ['error' => $e->getMessage()];
        }

        if (! $response->successful()) {
            return $entry + ['error' => 'management api returned HTTP '.$response->status()];
        }

        $body = $response->json();

        return $entry + [
            'messages_delivered' => self::counter($body, 'messages_delivered'),
            'messages_acked' => self::counter($body, 'messages_acked'),
            'messages_redelivered' => self::counter($body, 'messages_redelivered'),
        ];
    }

    /**
     * @return array<string, array<string, mixed>>
     */
    private function rabbitRsConnections(): array
    {
        $connections = $this->laravel->make('config')->get('queue.connections');
        if (! is_array($connections)) {
            return [];
        }

        $rabbitRs = [];
        foreach ($connections as $name => $connection) {
            if (is_array($connection) && ($connection['driver'] ?? null) === 'rabbit-rs') {
                $rabbitRs[(string) $name] = $connection;
            }
        }

        return $rabbitRs;
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
     * @param array<string, mixed>|null $body
     */
    private static function counter(?array $body, string $key): int
    {
        $value = $body[$key] ?? null;

        return is_numeric($value) ? (int) $value : 0;
    }

    /**
     * @param array<string, mixed> $stats pools keyed by connection name
     * @param array{management_url_configured: bool, queues: list<array<string, mixed>>} $queueStats
     */
    private function displayHuman(array $stats, array $queueStats): void
    {
        $this->info('Rabbit RS Pool Status');
        $this->line('');

        foreach ($stats as $name => $poolStats) {
            $this->line("  Connection:       {$name}");
            $this->line("  Handle:          {$poolStats['handle']}");
            $this->line("  PID:             {$poolStats['pid']}");
            $this->line("  Closed:          " . ($poolStats['closed'] ? 'yes' : 'no'));
            $this->line('');
            $this->line('  Native Pool Metrics (same-process only):');
            $this->line('  Publisher Metrics:');
            $this->line("    publishes:       {$poolStats['publishes_total']}");
            $this->line("    confirmations:   {$poolStats['confirmations_total']}");
            $this->line("    returns:         {$poolStats['returns_total']}");
            $this->line("    backpressure:    {$poolStats['backpressure_total']}");
            $this->line("    reconnects:      {$poolStats['reconnects_total']}");
            $this->line('    duplicates:      '.($poolStats['duplicates_total'] ?? 0));
            $this->line('');
            $this->line('  Consumer Metrics:');
            $this->line("    deliveries:      {$poolStats['deliveries_total']}");
            $this->line("    acks:            {$poolStats['acks_total']}");
            $this->line("    rejects:         {$poolStats['rejects_total']}");
            $this->line('');
            $this->line('  Latency (ms):');
            $this->line("    confirmation_latency p50: {$poolStats['confirmation_latency_p50']} p95: {$poolStats['confirmation_latency_p95']} p99: {$poolStats['confirmation_latency_p99']}");
            $this->line("    settlement_latency p50:   {$poolStats['settlement_latency_p50']} p95: {$poolStats['settlement_latency_p95']} p99: {$poolStats['settlement_latency_p99']}");
            $this->line('');
        }

        $this->line('  Queue Metrics (management API, cross-process):');
        if (! $queueStats['management_url_configured']) {
            $this->line('    management url not configured (set queue.connections.<name>.management_url)');

            return;
        }
        if ($queueStats['queues'] === []) {
            $this->line('    no queue defined on a connection with a management url');

            return;
        }
        foreach ($queueStats['queues'] as $queue) {
            $label = "    {$queue['connection']}/{$queue['queue']}:";
            if (isset($queue['error'])) {
                $this->line("{$label} unavailable ({$queue['error']})");

                continue;
            }
            $this->line("{$label} delivered {$queue['messages_delivered']}, acked {$queue['messages_acked']}, redelivered {$queue['messages_redelivered']}");
        }
        $this->line('    note: redelivered is an approximate duplicate signal — it also counts crash requeues');
    }
}
