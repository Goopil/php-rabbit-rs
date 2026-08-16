<?php

declare(strict_types=1);

namespace Tests;

use Drivers\BenchmarkDriver;
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
                'cpu_percent', 'rss_kb', 'connections', 'channels',
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
}
