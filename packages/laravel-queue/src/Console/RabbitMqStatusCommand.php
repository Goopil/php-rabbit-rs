<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Console\Command;

final class RabbitMqStatusCommand extends Command
{
    protected $signature = 'rabbit-rs:status {--format=human : Output format (human or json)}';

    protected $description = 'Display Rabbit RS native pool diagnostics';

    public function handle(NativePoolFactory $pools): int
    {
        $format = $this->option('format');

        $stats = $this->collectStats($pools);

        if ($format === 'json') {
            $this->line(json_encode($stats, JSON_PRETTY_PRINT));

            return self::SUCCESS;
        }

        $this->displayHuman($stats);

        return self::SUCCESS;
    }

    /**
     * @return array<string, mixed>
     */
    private function collectStats(NativePoolFactory $pools): array
    {
        $config = $this->laravel->make('config')->get('rabbit-rs');
        if (! is_array($config)) {
            $config = [];
        }

        try {
            $normalized = \Goopil\RabbitRs\Laravel\Config\ConfigNormalizer::normalize($config);
            $pool = $pools->make($normalized['native']);
            $stats = $pool->stats();
        } catch (\Throwable $e) {
            $stats = [
                'closed' => true,
                'pid' => 0,
                'handle' => 'unavailable',
                'publishes_total' => 0,
                'confirmations_total' => 0,
                'returns_total' => 0,
                'backpressure_total' => 0,
                'reconnects_total' => 0,
            ];
        }

        return $stats;
    }

    /**
     * @param array<string, mixed> $stats
     */
    private function displayHuman(array $stats): void
    {
        $this->info('Rabbit RS Pool Status');
        $this->line('');
        $this->line("  Handle:          {$stats['handle']}");
        $this->line("  PID:             {$stats['pid']}");
        $this->line("  Closed:          " . ($stats['closed'] ? 'yes' : 'no'));
        $this->line('');
        $this->line('  Metrics:');
        $this->line("    publishes:       {$stats['publishes_total']}");
        $this->line("    confirmations:   {$stats['confirmations_total']}");
        $this->line("    returns:         {$stats['returns_total']}");
        $this->line("    backpressure:    {$stats['backpressure_total']}");
        $this->line("    reconnects:      {$stats['reconnects_total']}");
    }
}
