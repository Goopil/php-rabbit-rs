<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests;

use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Orchestra\Testbench\TestCase as OrchestraTestCase;

abstract class TestCase extends OrchestraTestCase
{
    /**
     * @return list<class-string>
     */
    protected function getPackageProviders($app): array
    {
        return [RabbitMqServiceProvider::class];
    }
}