<?php

use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\AmqpMessage;
use Goopil\RabbitRs\PhpChannel;

/*
|--------------------------------------------------------------------------
| Test Case
|--------------------------------------------------------------------------
|
| The closure you provide to your test functions is always bound to a specific PHPUnit test
| case class. By default, that class is "PHPUnit\Framework\TestCase". Of course, you may
| need to change it using the "pest()" function to bind a different classes or traits.
|
*/

pest()->extend(Tests\TestCase::class)->in('Feature');

/*
|--------------------------------------------------------------------------
| Expectations
|--------------------------------------------------------------------------
|
| When you're writing tests, you often need to check that values meet certain conditions. The
| "expect()" function gives you access to a set of "expectations" methods that you can use
| to assert different things. Of course, you may extend the Expectation API at any time.
|
*/

expect()->extend('toBeOne', function () {
    return $this->toBe(1);
});

// Assert a non-empty string (handy for tags, ids, etc.)
expect()->extend('toBeNonEmptyString', function () {
    $val = $this->value;
    expect(is_string($val))->toBeTrue();
    expect($val)->not->toBe('');
    return $this;
});

// Assert a plausible AMQP consumer tag returned by simpleConsume().
// Accepts common prefixes used by various clients (php-amqplib, lapin, custom),
// and a conservative character set.
expect()->extend('toBeConsumerTag', function () {
    $val = $this->value;

    // must be a non-empty string
    expect($val)->toBeNonEmptyString();

    // allow typical prefixes and safe chars: letters, digits, dash, underscore, dot, colon
    $pattern = '/^(?:simple-consumer|consumer|amq\\.ctag|ctag)[-_:A-Za-z0-9\\.]+$/';

    // matches the pattern
    expect((bool) preg_match($pattern, $val))->toBeTrue();

    // length guard to catch trivially bogus tags
    expect(strlen($val))->toBeGreaterThan(5);

    return $this;
});

/*
|--------------------------------------------------------------------------
| Functions
|--------------------------------------------------------------------------
|
| While Pest is very powerful out-of-the-box, you may have some testing code specific to your
| project that you don't want to repeat in every file. Here you can also expose helpers as
| global functions to help you to reduce the number of lines of code in your test files.
|
*/

function mq_client(array $opts = []) {
    $opts = [
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ];

    $c = new PhpClient($opts);
    $c->connect();
   return $c;
}

function getPhpAndExtPath() {
    $ext = realpath(__DIR__ . '/../../../target/debug/librabbit_rs.' . (
        PHP_OS_FAMILY === 'Darwin' ? 'dylib' : (str_starts_with(strtolower(PHP_OS), 'win') ? 'dll' : 'so')
    ));
    $php = trim(shell_exec('which php'));

    return [
      'php' => $php,
      'ext' => $ext,
    ];
}

function sc_expect_success(callable $fn) {
    try {
        return $fn();
    } catch (Exception $e) {
        throw new Exception('Expected success, got: ' . $e->getMessage(), 0, $e);
    }
}

function mh_tmp_queue(string $prefix = 'mh.q.'): string {
    return $prefix . bin2hex(random_bytes(4));
}

// Helpers
function sc_tmp_queue(string $prefix = 'sc.q.'): string {
    return $prefix . bin2hex(random_bytes(4));
}

function sc_publish_many(PhpChannel $ch, string $q, int $n, string $prefix = 'm'): void {
    for ($i = 0; $i < $n; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage($prefix.$i)))->toBeTrue();
    }
}