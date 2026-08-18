<?php

declare(strict_types=1);

namespace Bench\Drivers;

use RuntimeException;

class DriverUnavailableException extends RuntimeException
{
}

interface Driver
{
    public function setup(): void;
    public function publish(array $messages, string $safety = 'safest'): void;
    public function consume(int $count): void;
    public function reset(): void;
    public function teardown(): void;
    public function metrics(): array;
    public function name(): string;
}
