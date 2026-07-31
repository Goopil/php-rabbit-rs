<?php

declare(strict_types=1);

namespace Goopil\RabbitRs;

class Exception extends \Exception
{
}

final class BackpressureException extends Exception
{
}

final class ConnectionException extends Exception
{
}

final class Pool
{
    public function __construct(array $config)
    {
    }

    public function publish(array $message): string
    {
    }

    public function publishBatch(array $messages): array
    {
    }

    public function consumer(string $profile): Consumer
    {
    }

    public function stats(): array
    {
    }

    public function close(): void
    {
    }
}

final class Consumer
{
    public function next(int $timeoutMs): ?Delivery
    {
    }

    public function close(): void
    {
    }
}

final class Delivery
{
    public function payload(): string
    {
    }

    public function metadata(): array
    {
    }

    public function ack(): void
    {
    }

    public function release(int $delayMs = 0): void
    {
    }

    public function reject(bool $requeue = false): void
    {
    }
}
