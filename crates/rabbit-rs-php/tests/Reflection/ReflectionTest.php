<?php

declare(strict_types=1);

/**
 * @param list<array{name: string, type: string, optional: bool, default?: mixed}> $expectedParameters
 */
function expectMethod(string $class, string $method, array $expectedParameters, ?string $expectedReturnType): void
{
    $reflection = new \ReflectionMethod($class, $method);
    expect($reflection->isPublic())->toBeTrue("{$class}::{$method} must be public");
    expect($reflection->isStatic())->toBeFalse("{$class}::{$method} must be an instance method");

    $returnType = $reflection->getReturnType();
    $returnTypeString = $returnType === null ? null : (string) $returnType;
    expect($returnTypeString)->toBe($expectedReturnType, "{$class}::{$method} has an unexpected return type");

    $parameters = $reflection->getParameters();
    expect(count($parameters))->toBe(count($expectedParameters), "{$class}::{$method} has an unexpected parameter count");

    foreach ($expectedParameters as $index => $expected) {
        $parameter = $parameters[$index];
        $type = $parameter->getType();
        expect($parameter->getName())->toBe($expected['name'], "{$class}::{$method} parameter name");
        expect($type === null ? null : (string) $type)->toBe($expected['type'], "{$class}::{$method} parameter type");
        expect($parameter->isOptional())->toBe($expected['optional'], "{$class}::{$method} parameter optionality");

        $hasDefault = array_key_exists('default', $expected);
        expect($parameter->isDefaultValueAvailable())->toBe($hasDefault, "{$class}::{$method} parameter default availability");
        if ($hasDefault) {
            expect($parameter->getDefaultValue())->toBe($expected['default'], "{$class}::{$method} parameter default value");
        }
    }
}

function expectNotConstructible(string $class): void
{
    try {
        new $class();
    } catch (\Throwable) {
        return;
    }

    expect(false)->toBeTrue("{$class} must reject direct construction");
}

describe('class reflection contract', function () {
    it('makes Pool, Consumer, and Delivery final', function () {
        foreach ([
            \Goopil\RabbitRs\Pool::class,
            \Goopil\RabbitRs\Consumer::class,
            \Goopil\RabbitRs\Delivery::class,
        ] as $class) {
            $reflection = new \ReflectionClass($class);
            expect($reflection->isFinal())->toBeTrue("{$class} must be final");
        }
    });

    it('rejects direct construction of Consumer and Delivery', function () {
        expectNotConstructible(\Goopil\RabbitRs\Consumer::class);
        expectNotConstructible(\Goopil\RabbitRs\Delivery::class);
    });
});

describe('Pool method signatures', function () {
    it('has correct __construct', function () {
        expectMethod(\Goopil\RabbitRs\Pool::class, '__construct', [
            ['name' => 'config', 'type' => 'array', 'optional' => false],
        ], null);
    });

    it('has correct publish', function () {
        expectMethod(\Goopil\RabbitRs\Pool::class, 'publish', [
            ['name' => 'message', 'type' => 'array', 'optional' => false],
        ], 'string');
    });

    it('has correct publishBatch', function () {
        expectMethod(\Goopil\RabbitRs\Pool::class, 'publishBatch', [
            ['name' => 'messages', 'type' => 'array', 'optional' => false],
        ], 'array');
    });

    it('has correct consumer', function () {
        expectMethod(\Goopil\RabbitRs\Pool::class, 'consumer', [
            ['name' => 'profile', 'type' => 'string', 'optional' => false],
        ], \Goopil\RabbitRs\Consumer::class);
    });

    it('has correct stats', function () {
        expectMethod(\Goopil\RabbitRs\Pool::class, 'stats', [], 'array');
    });

    it('has correct close', function () {
        expectMethod(\Goopil\RabbitRs\Pool::class, 'close', [], 'void');
    });
});

describe('Consumer method signatures', function () {
    it('has correct next', function () {
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'next', [
            ['name' => 'timeoutMs', 'type' => 'int', 'optional' => false],
        ], '?' . \Goopil\RabbitRs\Delivery::class);
    });

    it('has correct tryNext', function () {
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'tryNext', [], '?' . \Goopil\RabbitRs\Delivery::class);
    });

    it('has correct nextBatch', function () {
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'nextBatch', [
            ['name' => 'max', 'type' => 'int', 'optional' => false],
            ['name' => 'timeoutMs', 'type' => 'int', 'optional' => false],
        ], 'array');
    });

    it('has correct ackThrough', function () {
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'ackThrough', [
            ['name' => 'delivery', 'type' => \Goopil\RabbitRs\Delivery::class, 'optional' => false],
        ], 'void');
    });

    it('has correct ackBatch', function () {
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'ackBatch', [
            ['name' => 'deliveries', 'type' => 'array', 'optional' => false],
        ], 'void');
    });

    it('has correct drainErrors', function () {
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'drainErrors', [], 'array');
    });

    it('has correct close', function () {
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'close', [], 'void');
    });
});

describe('Delivery method signatures', function () {
    it('has correct payload', function () {
        expectMethod(\Goopil\RabbitRs\Delivery::class, 'payload', [], 'string');
    });

    it('has correct metadata', function () {
        expectMethod(\Goopil\RabbitRs\Delivery::class, 'metadata', [], 'array');
    });

    it('has correct ack', function () {
        expectMethod(\Goopil\RabbitRs\Delivery::class, 'ack', [], 'void');
    });

    it('has correct release', function () {
        expectMethod(\Goopil\RabbitRs\Delivery::class, 'release', [
            ['name' => 'delayMs', 'type' => 'int', 'optional' => true, 'default' => 0],
        ], 'void');
    });

    it('has correct reject', function () {
        expectMethod(\Goopil\RabbitRs\Delivery::class, 'reject', [
            ['name' => 'requeue', 'type' => 'bool', 'optional' => true, 'default' => false],
        ], 'void');
    });
});

describe('Pool construction error', function () {
    it('throws the stable base exception without exposing secrets', function () {
        $secrets = [
            'amqp://native-user:native-password@rabbit.internal/private-vhost',
            'native-password',
            'PRIVATE-KEY-MATERIAL',
        ];

        expect(fn () => new \Goopil\RabbitRs\Pool([
            'uri' => $secrets[0],
            'password' => $secrets[1],
            'private_key' => $secrets[2],
        ]))->toThrow(function (\Goopil\RabbitRs\Exception $e) use ($secrets): void {
            expect($e::class)->toBe(\Goopil\RabbitRs\Exception::class);
            expect($e->getMessage())->not->toBeEmpty();
            foreach ($secrets as $secret) {
                expect($e->getMessage())->not->toContain($secret);
            }
        });
    });
});
