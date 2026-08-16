<?php

declare(strict_types=1);

namespace Tests;

use Drivers\DatabaseDriver;
use Drivers\PhpAmqplibDriver;
use Drivers\RabbitRsDriver;
use Drivers\RedisDriver;
use Drivers\VyuldashevDriver;

class DriverRegistryTest extends DriverContractTestCase
{
    protected function drivers(): array
    {
        return [
            'rabbit-rs' => new RabbitRsDriver(),
            'php-amqplib' => new PhpAmqplibDriver(),
            'vyuldashev' => new VyuldashevDriver(),
            'redis' => new RedisDriver(),
            'database' => new DatabaseDriver(),
        ];
    }
}
