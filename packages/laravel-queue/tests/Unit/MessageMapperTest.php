<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Laravel\Support\MessageMapper;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Illuminate\Support\Str;

final class MessageMapperTest extends TestCase
{
    public function testMapIncludesTimeoutMsFromPublisherConfigByDefault(): void
    {
        $mapper = new MessageMapper(['confirm_timeout' => 5000]);

        $message = $mapper->map(
            '{"job":"App\\\\Jobs\\\\Example"}',
            $this->route(),
            'orders',
        );

        self::assertSame(5000, $message['timeout_ms']);
    }

    public function testMapUsesExplicitTimeoutMsOverConfigDefault(): void
    {
        $mapper = new MessageMapper(['confirm_timeout' => 5000]);

        $message = $mapper->map(
            '{"job":"App\\\\Jobs\\\\Example"}',
            $this->route(),
            'orders',
            ['timeout_ms' => 12000],
        );

        self::assertSame(12000, $message['timeout_ms']);
    }

    public function testMapOmitsTimeoutMsWhenConfigHasNoConfirmTimeout(): void
    {
        $mapper = new MessageMapper([]);

        $message = $mapper->map(
            '{"job":"App\\\\Jobs\\\\Example"}',
            $this->route(),
            'orders',
        );

        self::assertArrayNotHasKey('timeout_ms', $message);
    }

    public function testMapPreservesAllOtherFields(): void
    {
        $mapper = new MessageMapper(['confirm_timeout' => 30000]);

        $message = $mapper->map(
            'payload',
            $this->route(),
            'orders',
            ['content_type' => 'application/json', 'headers' => ['x-foo' => 'bar']],
            5000,
        );

        self::assertSame('default', $message['broker']);
        self::assertSame('laravel.jobs', $message['exchange']);
        self::assertSame('orders', $message['routing_key']);
        self::assertSame('payload', $message['payload']);
        self::assertTrue(Str::isUuid($message['message_id']));
        self::assertSame('application/json', $message['content_type']);
        self::assertSame(['x-foo' => 'bar'], $message['headers']);
        self::assertSame(5000, $message['delay_ms']);
        self::assertSame(30000, $message['timeout_ms']);
    }

    /**
     * @return array{broker: string, exchange: string, routing_key: string}
     */
    private function route(): array
    {
        return [
            'broker' => 'default',
            'exchange' => 'laravel.jobs',
            'routing_key' => '{queue}',
        ];
    }
}
