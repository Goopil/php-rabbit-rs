--TEST--
Rabbit RS operational class reflection contract
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

/**
 * @param list<array{name: string, type: string, optional: bool, default?: mixed}> $expectedParameters
 */
function expect_method(
    string $class,
    string $method,
    array $expectedParameters,
    ?string $expectedReturnType,
): void {
    $reflection = new ReflectionMethod($class, $method);
    expect_true($reflection->isPublic(), "{$class}::{$method} must be public");
    expect_true(!$reflection->isStatic(), "{$class}::{$method} must be an instance method");

    $returnType = $reflection->getReturnType();
    expect_true(
        ($returnType === null ? null : (string) $returnType) === $expectedReturnType,
        "{$class}::{$method} has an unexpected return type",
    );

    $parameters = $reflection->getParameters();
    expect_true(
        count($parameters) === count($expectedParameters),
        "{$class}::{$method} has an unexpected parameter count",
    );

    foreach ($expectedParameters as $index => $expected) {
        $parameter = $parameters[$index];
        $type = $parameter->getType();
        expect_true($parameter->getName() === $expected['name'], "{$class}::{$method} parameter name");
        expect_true(
            ($type === null ? null : (string) $type) === $expected['type'],
            "{$class}::{$method} parameter type",
        );
        expect_true(
            $parameter->isOptional() === $expected['optional'],
            "{$class}::{$method} parameter optionality",
        );

        $hasDefault = array_key_exists('default', $expected);
        expect_true(
            $parameter->isDefaultValueAvailable() === $hasDefault,
            "{$class}::{$method} parameter default availability",
        );
        if ($hasDefault) {
            expect_true(
                $parameter->getDefaultValue() === $expected['default'],
                "{$class}::{$method} parameter default value",
            );
        }
    }
}

function expect_not_constructible(string $class): void {
    try {
        new $class();
    } catch (Throwable) {
        return;
    }

    throw new Exception("{$class} must reject direct construction");
}

foreach ([
    Goopil\RabbitRs\Pool::class,
    Goopil\RabbitRs\Consumer::class,
    Goopil\RabbitRs\Delivery::class,
] as $class) {
    $reflection = new ReflectionClass($class);
    expect_true($reflection->isFinal(), "{$class} must be final");
}

expect_not_constructible(Goopil\RabbitRs\Consumer::class);
expect_not_constructible(Goopil\RabbitRs\Delivery::class);

expect_method(Goopil\RabbitRs\Pool::class, '__construct', [
    ['name' => 'config', 'type' => 'array', 'optional' => false],
], null);
expect_method(Goopil\RabbitRs\Pool::class, 'publish', [
    ['name' => 'message', 'type' => 'array', 'optional' => false],
], 'string');
expect_method(Goopil\RabbitRs\Pool::class, 'publishBatch', [
    ['name' => 'messages', 'type' => 'array', 'optional' => false],
], 'array');
expect_method(Goopil\RabbitRs\Pool::class, 'consumer', [
    ['name' => 'profile', 'type' => 'string', 'optional' => false],
], Goopil\RabbitRs\Consumer::class);
expect_method(Goopil\RabbitRs\Pool::class, 'stats', [], 'array');
expect_method(Goopil\RabbitRs\Pool::class, 'close', [], 'void');

expect_method(Goopil\RabbitRs\Consumer::class, 'next', [
    ['name' => 'timeoutMs', 'type' => 'int', 'optional' => false],
], '?' . Goopil\RabbitRs\Delivery::class);
expect_method(Goopil\RabbitRs\Consumer::class, 'close', [], 'void');

expect_method(Goopil\RabbitRs\Delivery::class, 'payload', [], 'string');
expect_method(Goopil\RabbitRs\Delivery::class, 'metadata', [], 'array');
expect_method(Goopil\RabbitRs\Delivery::class, 'ack', [], 'void');
expect_method(Goopil\RabbitRs\Delivery::class, 'release', [
    ['name' => 'delayMs', 'type' => 'int', 'optional' => true, 'default' => 0],
], 'void');
expect_method(Goopil\RabbitRs\Delivery::class, 'reject', [
    ['name' => 'requeue', 'type' => 'bool', 'optional' => true, 'default' => false],
], 'void');

$secrets = [
    'amqp://native-user:native-password@rabbit.internal/private-vhost',
    'native-password',
    'PRIVATE-KEY-MATERIAL',
];

try {
    new Goopil\RabbitRs\Pool([
        'uri' => $secrets[0],
        'password' => $secrets[1],
        'private_key' => $secrets[2],
    ]);
    throw new Exception('Pool construction must be unavailable');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(
        $exception::class === Goopil\RabbitRs\Exception::class,
        'Pool construction must throw the stable base exception',
    );
    expect_true($exception->getMessage() !== '', 'Pool exception message must not be empty');
    foreach ($secrets as $secret) {
        expect_true(
            !str_contains($exception->getMessage(), $secret),
            'Pool exception message must not expose secrets',
        );
    }
}

echo "OK\n";
?>
--EXPECT--
OK
