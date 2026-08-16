<?php

declare(strict_types=1);

namespace App\Console\Commands;

use Drivers\DatabaseDriver;
use Drivers\PhpAmqplibDriver;
use Drivers\RabbitRsDriver;
use Drivers\RedisDriver;
use Drivers\VyuldashevDriver;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Input\InputOption;
use Symfony\Component\Console\Output\OutputInterface;

final class PublishBenchmark extends Command
{
    protected function configure(): void
    {
        $this->setName('publish')
            ->setDescription('Publish N messages to a driver queue')
            ->addOption('driver', 'd', InputOption::VALUE_REQUIRED, 'Driver name', 'rabbit-rs')
            ->addOption('count', 'c', InputOption::VALUE_REQUIRED, 'Message count', '100')
            ->addOption('payload-size', 'p', InputOption::VALUE_REQUIRED, 'Payload size in bytes', '1024')
            ->addOption('batch-size', 'b', InputOption::VALUE_REQUIRED, 'Batch size', '1')
            ->addOption('mode', 'm', InputOption::VALUE_REQUIRED, 'Execution mode: cli, fpm, octane', 'cli');
    }

    protected function execute(InputInterface $input, OutputInterface $output): int
    {
        $driverName = $input->getOption('driver');
        $count = (int) $input->getOption('count');
        $payloadSize = (int) $input->getOption('payload-size');
        $batchSize = (int) $input->getOption('batch-size');
        $mode = (string) $input->getOption('mode');

        if (! in_array($mode, ['cli', 'fpm', 'octane'], true)) {
            $output->writeln("<error>Unsupported mode: {$mode}. Allowed: cli, fpm, octane</error>");

            return Command::FAILURE;
        }
        if ($mode !== 'cli' && PHP_SAPI !== 'cli') {
            $output->writeln("<error>Mode '{$mode}' must be invoked via the corresponding runtime (artisan under cli, a web endpoint for fpm, or the Octane worker for octane).</error>");

            return Command::FAILURE;
        }

        $config = $this->loadConfig();
        $driver = $this->makeDriver($driverName, $config);

        if ($driver === null) {
            $output->writeln("<error>Unknown driver: {$driverName}</error>");

            return Command::FAILURE;
        }

        $driver->setup();
        $driver->reset();

        $payload = str_repeat('x', $payloadSize);
        $start = microtime(true);

        $published = 0;
        while ($published < $count) {
            $batch = [];
            $batchCount = min($batchSize, $count - $published);
            for ($i = 0; $i < $batchCount; $i++) {
                $batch[] = json_encode([
                    'id' => $this->uuid(),
                    'seq' => $published + $i,
                    'payload' => $payload,
                ]);
            }
            $driver->publish($batch);
            $published += $batchCount;
        }

        $elapsed = microtime(true) - $start;
        $metrics = $driver->metrics();
        $metrics['driver'] = $driverName;
        $metrics['action'] = 'publish';
        $metrics['mode'] = $mode;
        $metrics['count'] = $count;
        $metrics['payload_size'] = $payloadSize;
        $metrics['batch_size'] = $batchSize;
        $metrics['elapsed_seconds'] = round($elapsed, 4);
        $metrics['throughput'] = round($count / max($elapsed, 0.001), 2);

        $output->writeln(json_encode($metrics, JSON_PRETTY_PRINT));

        return Command::SUCCESS;
    }

    private function loadConfig(): array
    {
        $path = __DIR__ . '/../../../config/benchmark.php';
        if (file_exists($path)) {
            return require $path;
        }

        return [];
    }

    private function makeDriver(string $name, array $config): ?object
    {
        $drivers = $config['drivers'] ?? [];
        $driverConfig = $drivers[$name] ?? [];
        $rabbitRsConfig = $config['rabbit-rs-config'] ?? [];
        $allConfig = array_merge($driverConfig, ['rabbit-rs-config' => $rabbitRsConfig]);

        return match ($name) {
            'rabbit-rs' => new RabbitRsDriver($allConfig),
            'php-amqplib' => new PhpAmqplibDriver($driverConfig),
            'vyuldashev' => new VyuldashevDriver($driverConfig),
            'redis' => new RedisDriver($driverConfig),
            'database' => new DatabaseDriver($driverConfig),
            default => null,
        };
    }

    private function uuid(): string
    {
        $data = random_bytes(16);
        $data[6] = chr((ord($data[6]) & 0x0f) | 0x40);
        $data[8] = chr((ord($data[8]) & 0x3f) | 0x80);

        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($data), 4));
    }
}
