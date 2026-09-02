<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Console\Command;
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
     * @return array<string, mixed>|false
     */
    private function collectStats(NativePoolFactory $pools): array|false
    {
        $config = $this->laravel->make('config')->get('rabbit-rs');
        if (! is_array($config)) {
            $config = [];
        }

        try {
            $normalized = ConfigNormalizer::normalize($config);
            $pool = $pools->make($normalized['native']);
            $stats = $pool->stats();
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
     * crash requeues. Requires brokers.<name>.management_url.
     *
     * @return array{management_url_configured: bool, queues: list<array<string, mixed>>}
     */
    private function collectQueueStats(): array
    {
        $config = $this->rawConfig();

        $managementUrls = [];
        foreach ($config['brokers'] ?? [] as $name => $broker) {
            $url = is_array($broker) ? ($broker['management_url'] ?? null) : null;
            if (is_string($url) && trim($url) !== '') {
                $managementUrls[(string) $name] = rtrim(trim($url), '/');
            }
        }

        if ($managementUrls === []) {
            return ['management_url_configured' => false, 'queues' => []];
        }

        $pairs = [];
        foreach ($config['workers'] ?? [] as $worker) {
            foreach (is_array($worker) ? ($worker['subscriptions'] ?? []) : [] as $subscription) {
                if (! is_array($subscription) || ($subscription['enabled'] ?? true) === false) {
                    continue;
                }
                $broker = $subscription['broker'] ?? null;
                $queue = $subscription['queue'] ?? null;
                if (is_string($broker) && is_string($queue) && isset($managementUrls[$broker])) {
                    $pairs[$broker.'|'.$queue] = ['broker' => $broker, 'queue' => $queue];
                }
            }
        }
        ksort($pairs);

        $brokers = $config['brokers'];
        $queues = [];
        foreach ($pairs as ['broker' => $broker, 'queue' => $queue]) {
            $queues[] = $this->fetchQueueStats(
                $managementUrls[$broker],
                $brokers[$broker],
                $broker,
                $queue,
            );
        }

        return ['management_url_configured' => true, 'queues' => $queues];
    }

    /**
     * @return array<string, mixed>
     */
    private function fetchQueueStats(string $baseUrl, mixed $broker, string $brokerName, string $queue): array
    {
        $entry = ['broker' => $brokerName, 'queue' => $queue];

        $credentials = is_array($broker) ? ($broker['credentials'] ?? null) : null;
        $username = is_array($credentials) ? ($credentials['username'] ?? null) : null;
        $password = is_array($credentials) ? ($credentials['password'] ?? null) : null;
        $vhost = is_array($broker) && is_string($broker['vhost'] ?? null) ? $broker['vhost'] : '/';

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
     * @return array<string, mixed>
     */
    private function rawConfig(): array
    {
        $config = $this->laravel->make('config')->get('rabbit-rs');

        return is_array($config) ? $config : [];
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
     * @param array<string, mixed> $stats
     * @param array{management_url_configured: bool, queues: list<array<string, mixed>>} $queueStats
     */
    private function displayHuman(array $stats, array $queueStats): void
    {
        $this->info('Rabbit RS Pool Status');
        $this->line('');
        $this->line("  Handle:          {$stats['handle']}");
        $this->line("  PID:             {$stats['pid']}");
        $this->line("  Closed:          " . ($stats['closed'] ? 'yes' : 'no'));
        $this->line('');
        $this->line('  Native Pool Metrics (same-process only):');
        $this->line('  Publisher Metrics:');
        $this->line("    publishes:       {$stats['publishes_total']}");
        $this->line("    confirmations:   {$stats['confirmations_total']}");
        $this->line("    returns:         {$stats['returns_total']}");
        $this->line("    backpressure:    {$stats['backpressure_total']}");
        $this->line("    reconnects:      {$stats['reconnects_total']}");
        $this->line('    duplicates:      '.($stats['duplicates_total'] ?? 0));
        $this->line('');
        $this->line('  Consumer Metrics:');
        $this->line("    deliveries:      {$stats['deliveries_total']}");
        $this->line("    acks:            {$stats['acks_total']}");
        $this->line("    rejects:         {$stats['rejects_total']}");
        $this->line('');
        $this->line('  Latency (ms):');
        $this->line("    confirmation_latency p50: {$stats['confirmation_latency_p50']} p95: {$stats['confirmation_latency_p95']} p99: {$stats['confirmation_latency_p99']}");
        $this->line("    settlement_latency p50:   {$stats['settlement_latency_p50']} p95: {$stats['settlement_latency_p95']} p99: {$stats['settlement_latency_p99']}");
        $this->line('');
        $this->line('  Queue Metrics (management API, cross-process):');
        if (! $queueStats['management_url_configured']) {
            $this->line('    management url not configured (set brokers.<name>.management_url)');

            return;
        }
        if ($queueStats['queues'] === []) {
            $this->line('    no enabled worker subscription on a broker with a management url');

            return;
        }
        foreach ($queueStats['queues'] as $queue) {
            $label = "    {$queue['broker']}/{$queue['queue']}:";
            if (isset($queue['error'])) {
                $this->line("{$label} unavailable ({$queue['error']})");

                continue;
            }
            $this->line("{$label} delivered {$queue['messages_delivered']}, acked {$queue['messages_acked']}, redelivered {$queue['messages_redelivered']}");
        }
        $this->line('    note: redelivered is an approximate duplicate signal — it also counts crash requeues');
    }
}
