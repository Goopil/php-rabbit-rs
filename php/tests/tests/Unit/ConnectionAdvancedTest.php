<?php

use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\AmqpMessage;

// ---------------------------------------------------------------------------------------------
// HEARTBEAT TESTS
// ---------------------------------------------------------------------------------------------

test('connection accepts heartbeat option and connects successfully', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'heartbeat' => 30,
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $c->close();
});

test('connection with heartbeat=0 disables heartbeats', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'heartbeat' => 0,
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $c->close();
});

test('connection with very long heartbeat (300s) connects successfully', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'heartbeat' => 300,
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $c->close();
});

test('heartbeat option with invalid negative value throws exception', function () {
    expect(fn () => new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'heartbeat' => -1,
    ]))->toThrow(Exception::class);
});

test('heartbeat option with value > 65535 throws exception', function () {
    expect(fn () => new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'heartbeat' => 70000,
    ]))->toThrow(Exception::class);
});

// ---------------------------------------------------------------------------------------------
// FRAME_MAX TESTS
// ---------------------------------------------------------------------------------------------

test('connection accepts frame_max option and connects successfully', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'frame_max' => 131072, // 128 KB
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $c->close();
});

test('connection with large frame_max (1048576) can send large messages', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'frame_max' => 1048576, // 1 MB
    ]);

    expect($c->connect())->toBeTrue();

    $ch = $c->openChannel();
    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Large message
    $payload = random_bytes(512 * 1024); // 512 KB
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $d = $ch->basicGet($q);
    expect($d)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect(strlen($d->getBody()))->toBe(strlen($payload));

    $ch->close();
    $c->close();
});

test('frame_max=0 uses server default', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'frame_max' => 0,
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $c->close();
});

test('frame_max with negative value throws exception', function () {
    expect(fn () => new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'frame_max' => -1,
    ]))->toThrow(Exception::class);
});

// ---------------------------------------------------------------------------------------------
// CONNECTION_TIMEOUT TESTS
// ---------------------------------------------------------------------------------------------

test('connection accepts connection_timeout option and connects successfully', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'connection_timeout' => 10000, // 10 seconds
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $c->close();
});

test('connection timeout with zero or negative value throws exception', function () {
    expect(fn () => new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'connection_timeout' => 0,
    ]))->toThrow(Exception::class);
});

// ---------------------------------------------------------------------------------------------
// COMBINED OPTIONS TESTS
// ---------------------------------------------------------------------------------------------

test('connection with heartbeat + frame_max + timeout works correctly', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'heartbeat' => 60,
        'frame_max' => 262144, // 256 KB
        'connection_timeout' => 10000,
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    // Test actual functionality
    $ch = $c->openChannel();
    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $payload = random_bytes(128 * 1024); // 128 KB
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $d = $ch->basicGet($q);
    expect($d)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect(strlen($d->getBody()))->toBe(strlen($payload));

    $ch->close();
    $c->close();
});

test('connection with all advanced options operates normally', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'heartbeat' => 60,
        'frame_max' => 131072,
        'connection_timeout' => 10000,
        'reconnect' => [
            'enabled' => true,
            'max_retries' => 3,
        ],
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $ch = $c->openChannel();
    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    for ($i = 0; $i < 10; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage("msg-$i")))->toBeTrue();
    }

    $seen = 0;
    $tag = $ch->simpleConsume($q, function () use (&$seen) { $seen++; });

    $n = 0;
    while ($n < 10 && $seen < 10) {
        $n += $ch->wait(500, 10);
    }

    expect($seen)->toBe(10);

    $ch->basicCancel($tag);
    $ch->close();
    $c->close();
});

test('connection with default values when no advanced options specified', function () {
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ]);

    expect($c->connect())->toBeTrue();
    expect($c->ping())->toBeTrue();

    $c->close();
});
