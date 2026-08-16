<?php

declare(strict_types=1);

namespace Tests;

use Drivers\BenchmarkDriver;
use Drivers\DatabaseDriver;
use PHPUnit\Framework\TestCase;

abstract class DriverContractTestCase extends TestCase
{
    /**
     * @return array<string, BenchmarkDriver>
     */
    abstract protected function drivers(): array;

    public function testDriversReturnArray(): void
    {
        $this->assertNotEmpty($this->drivers());
    }

    public function testDriversHaveCorrectKeys(): void
    {
        $drivers = $this->drivers();
        foreach ($drivers as $name => $driver) {
            $this->assertIsString($name, "Driver key must be a string, got: " . get_debug_type($name));
            $this->assertInstanceOf(
                BenchmarkDriver::class,
                $driver,
                "Driver '{$name}' must implement BenchmarkDriver",
            );
        }
    }

    public function testAllExpectedDriversPresent(): void
    {
        $drivers = $this->drivers();
        $expected = [
            'rabbit-rs',
            'php-amqplib',
            'vyuldashev',
            'redis',
            'database',
        ];
        foreach ($expected as $name) {
            $this->assertArrayHasKey(
                $name,
                $drivers,
                "Expected driver '{$name}' is not registered",
            );
        }
    }

    public function testMetricsShape(): void
    {
        $drivers = $this->drivers();
        foreach ($drivers as $name => $driver) {
            $metrics = $driver->metrics();
            $this->assertIsArray($metrics, "Driver '{$name}' metrics must be an array");
            $requiredKeys = [
                'throughput', 'p50', 'p95', 'p99',
                'cpu_seconds', 'rss_kb', 'connections', 'channels',
                'duplicates', 'losses',
            ];
            foreach ($requiredKeys as $key) {
                $this->assertArrayHasKey(
                    $key,
                    $metrics,
                    "Driver '{$name}' is missing metric '{$key}'",
                );
            }
        }
    }

    public function testDatabaseDriverEndToEndRoundTrip(): void
    {
        $driver = new DatabaseDriver([
            'connection' => 'sqlite',
            'database' => ':memory:',
        ]);

        $driver->setup();
        $driver->reset();

        $messages = [];
        for ($i = 0; $i < 5; $i++) {
            $messages[] = json_encode([
                'id' => 'msg-' . $i,
                'seq' => $i,
                'payload' => 'x',
            ]);
        }
        $driver->publish($messages);
        $driver->consume(5);

        $metrics = $driver->metrics();
        $this->assertSame(0, $metrics['duplicates'], 'database driver should report no duplicates');
        $this->assertSame(0, $metrics['losses'], 'database driver should report zero losses');
        $this->assertGreaterThan(0, $metrics['throughput'], 'throughput should be positive after consuming messages');

        $driver->reset();
        $metricsAfterReset = $driver->metrics();
        $this->assertSame(0, $metricsAfterReset['losses'], 'reset should clear losses');
        $this->assertSame(0, $metricsAfterReset['duplicates'], 'reset should clear duplicates');
    }
}
