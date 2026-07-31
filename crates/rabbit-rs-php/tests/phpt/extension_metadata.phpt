--TEST--
Rabbit RS extension metadata and exception hierarchy
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

expect_true(extension_loaded('rabbit_rs'), 'rabbit_rs is not loaded');
expect_true(
    phpversion('rabbit_rs') === getenv('RABBIT_RS_EXPECTED_VERSION'),
    'extension version does not match Cargo'
);
expect_true(is_subclass_of(Goopil\RabbitRs\Exception::class, Exception::class), 'base exception');
expect_true(is_subclass_of(Goopil\RabbitRs\Exception::class, Throwable::class), 'base throwable');
expect_true(is_subclass_of(Goopil\RabbitRs\BackpressureException::class, Goopil\RabbitRs\Exception::class), 'backpressure exception');
expect_true(is_subclass_of(Goopil\RabbitRs\ConnectionException::class, Goopil\RabbitRs\Exception::class), 'connection exception');
expect_true((new ReflectionClass(Goopil\RabbitRs\BackpressureException::class))->isFinal(), 'backpressure final');
expect_true((new ReflectionClass(Goopil\RabbitRs\ConnectionException::class))->isFinal(), 'connection final');
echo "OK\n";
?>
--EXPECT--
OK
