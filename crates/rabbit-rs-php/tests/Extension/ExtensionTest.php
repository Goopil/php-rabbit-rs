<?php

declare(strict_types=1);

describe('extension metadata', function () {
    it('is loaded', function () {
        expect(extension_loaded('rabbit_rs'))->toBeTrue();
    });

    it('reports a version matching Cargo', function () {
        expect(phpversion('rabbit_rs'))
            ->toBe(getenv('RABBIT_RS_EXPECTED_VERSION'));
    });
});

describe('exception hierarchy', function () {
    it('has Exception extending the base PHP Exception', function () {
        expect(is_subclass_of(\Goopil\RabbitRs\Exception::class, \Exception::class))
            ->toBeTrue();
    });

    it('has Exception implementing Throwable', function () {
        expect(is_subclass_of(\Goopil\RabbitRs\Exception::class, \Throwable::class))
            ->toBeTrue();
    });

    it('has BackpressureException extending Exception', function () {
        expect(is_subclass_of(\Goopil\RabbitRs\BackpressureException::class, \Goopil\RabbitRs\Exception::class))
            ->toBeTrue();
    });

    it('has ConnectionException extending Exception', function () {
        expect(is_subclass_of(\Goopil\RabbitRs\ConnectionException::class, \Goopil\RabbitRs\Exception::class))
            ->toBeTrue();
    });

    it('has final BackpressureException', function () {
        expect((new \ReflectionClass(\Goopil\RabbitRs\BackpressureException::class))->isFinal())
            ->toBeTrue();
    });

    it('has final ConnectionException', function () {
        expect((new \ReflectionClass(\Goopil\RabbitRs\ConnectionException::class))->isFinal())
            ->toBeTrue();
    });
});
