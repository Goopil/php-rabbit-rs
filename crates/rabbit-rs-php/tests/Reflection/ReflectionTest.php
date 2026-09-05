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
    expect(fn () => new $class())->toThrow(\Exception::class, 'You cannot instantiate this class from PHP.');
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

describe('method signatures', function () {
    it('exposes stable signatures for every public method', function () {
        $delivery = \Goopil\RabbitRs\Delivery::class;
        foreach ([
            \Goopil\RabbitRs\Pool::class => [
                '__construct' => [[
                    ['name' => 'config', 'type' => 'array', 'optional' => false],
                ], null],
                'publish' => [[
                    ['name' => 'message', 'type' => 'array', 'optional' => false],
                ], 'string'],
                'publishBatch' => [[
                    ['name' => 'messages', 'type' => 'array', 'optional' => false],
                ], 'array'],
                'consumer' => [[
                    ['name' => 'profile', 'type' => 'string', 'optional' => false],
                ], \Goopil\RabbitRs\Consumer::class],
                'stats' => [[], 'array'],
                'close' => [[], 'void'],
            ],
            \Goopil\RabbitRs\Consumer::class => [
                'next' => [[
                    ['name' => 'timeoutMs', 'type' => 'int', 'optional' => false],
                ], '?' . $delivery],
                'tryNext' => [[], '?' . $delivery],
                'nextBatch' => [[
                    ['name' => 'max', 'type' => 'int', 'optional' => false],
                    ['name' => 'timeoutMs', 'type' => 'int', 'optional' => false],
                ], 'array'],
                'ackThrough' => [[
                    ['name' => 'delivery', 'type' => $delivery, 'optional' => false],
                ], 'void'],
                'ackBatch' => [[
                    ['name' => 'deliveries', 'type' => 'array', 'optional' => false],
                ], 'void'],
                'drainErrors' => [[], 'array'],
                'close' => [[], 'void'],
            ],
            $delivery => [
                'payload' => [[], 'string'],
                'metadata' => [[], 'array'],
                'ack' => [[], 'void'],
                'release' => [[
                    ['name' => 'delayMs', 'type' => 'int', 'optional' => true, 'default' => 0],
                ], 'void'],
                'reject' => [[
                    ['name' => 'requeue', 'type' => 'bool', 'optional' => true, 'default' => false],
                ], 'void'],
            ],
        ] as $class => $methods) {
            foreach ($methods as $method => [$parameters, $returnType]) {
                expectMethod($class, $method, $parameters, $returnType);
            }
        }
    });
});

describe('Pool construction error', function () {
    it('throws the stable base exception without exposing secrets', function () {
        $secrets = [
            // Dummy test fixture: this URI is never used to open a connection and the
            // credentials are fictional. S5332 (plaintext AMQP) and S2068 (hardcoded
            // credentials) are false positives in this context.
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
