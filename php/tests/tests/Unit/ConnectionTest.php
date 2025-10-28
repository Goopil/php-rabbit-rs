<?php

use Goopil\RabbitRs\PhpClient;

// ---------------------------------------------------------------------------------------------
// HELPERS
// ---------------------------------------------------------------------------------------------

function mq_get_connections(): array {
    $url = 'http://127.0.0.1:15672/api/connections';
    $auth = 'test:test';

    if (function_exists('curl_init')) {
        $ch = curl_init($url);
        curl_setopt_array($ch, [
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_USERPWD => $auth,
            CURLOPT_HTTPAUTH => CURLAUTH_BASIC,
            CURLOPT_TIMEOUT => 3,
        ]);
        $res = curl_exec($ch);
        if ($res === false) {
            return [];
        }
        $code = curl_getinfo($ch, CURLINFO_HTTP_CODE);
        curl_close($ch);
        if ($code !== 200) {
            return [];
        }
        $json = json_decode($res, true);
        return is_array($json) ? $json : [];
    }

    $opts = [
        'http' => [
            'header' => 'Authorization: Basic ' . base64_encode($auth) . "\r\n",
            'timeout' => 3,
        ],
    ];
    $ctx = stream_context_create($opts);
    $res = @file_get_contents($url, false, $ctx);
    if ($res === false) {
        return [];
    }
    $json = json_decode($res, true);
    return is_array($json) ? $json : [];
}

function mq_count_connections_for(string $user = 'test', string $vhost = '/'): int {
    $conns = mq_get_connections();
    $count = 0;
    foreach ($conns as $c) {
        if (($c['user'] ?? null) === $user && ($c['vhost'] ?? null) === $vhost) {
            $count++;
        }
    }
    return $count;
}

function mq_wait_for_count(int $expected, int $timeoutMs = 4000): bool {
    $start = hrtime(true);
    do {
        if (mq_count_connections_for('test', '/') === $expected) {
            return true;
        }
        usleep(100_000); // 100ms
    } while (((hrtime(true) - $start) / 1_000_000) < $timeoutMs);
    return mq_count_connections_for('test', '/') === $expected;
}

function mq_close_first_connection(): bool {
    $conns = mq_get_connections();
    if (!$conns || !is_array($conns)) return false;

    $first = $conns[0] ?? null;
    if (!$first || !isset($first['name'])) return false;

    $name = rawurlencode($first['name']);

    $ch = curl_init("http://127.0.0.1:15672/api/connections/{$name}");
    curl_setopt_array($ch, [
        CURLOPT_CUSTOMREQUEST => 'DELETE',
        CURLOPT_RETURNTRANSFER => true,
        CURLOPT_USERPWD => 'test:test',
        CURLOPT_HTTPAUTH => CURLAUTH_BASIC,
        CURLOPT_TIMEOUT => 4,
    ]);
    curl_exec($ch);
    $code = curl_getinfo($ch, CURLINFO_HTTP_CODE);
    curl_close($ch);

    return in_array($code, [200, 204], true);
}

// ---------------------------------------------------------------------------------------------
// BASIC CONNECTION TESTS
// ---------------------------------------------------------------------------------------------

test('PhpClient class exists', function () {
    // Description: Basic test to ensure the PhpClient class exists.
    expect('Goopil\RabbitRs\PhpClient')
        ->toBeClasses();
});

test('PhpClient can be instantiated with valid args', function () {
    // Description: Tests that PhpClient can be instantiated with valid arguments.
    $conn = new PhpClient([
        'host' => 'rabbit.local',
        'user' => 'guest',
        'password' => 'guest',
        'vhost' => '/',
    ]);

    expect($conn)->toBeInstanceOf(PhpClient::class);
});

test("PhpClient throws if it can't connect", function () {
    // Description: Tests that PhpClient throws an exception if it can't connect.
    expect(function () {
        $connection = new PhpClient([
            'host' => 'rabbit.local',
            'user' => 'guest',
            'password' => 'guest',
            'vhost' => '/',
        ]);
        $connection->connect();
    })
        ->toThrow(Exception::class);
});

test("PhpClient can connect", function () {
    // Description: Tests that PhpClient can connect to the broker.
    $connection = new PhpClient([
        'host' => '127.0.0.1',
        'user' => 'test',
        'password' => 'test',
    ]);

    $connection->connect();

    expect(true)->toBeTrue();
});

test("PhpClient can connect (with port as string)", function () {
    // Description: Tests that PhpClient can connect with the port specified as a string.
    $connection = new PhpClient([
        'host' => '127.0.0.1',
        'user' => 'test',
        'password' => 'test',
        'port' => '5672',
    ]);
    $connection->connect();

    expect(true)->toBeTrue();
});

test('PhpClient::connect is idempotent on the same instance', function () {
    // Description: Tests that PhpClient::connect is idempotent on the same instance.
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ]);
    $c->connect();
    $c->connect();
    $c->ping();

    $ok = mq_wait_for_count(1);
    if (!$ok) {
        fwrite(STDERR, "\n[diag] connections=" . json_encode(mq_get_connections()) . "\n");
    }
    expect($ok)->toBeTrue();
});

test('PhpClient ping performs a round-trip', function () {
    // Description: Tests that PhpClient ping performs a round-trip.
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ]);
    $c->connect();

    expect($c->ping())->toBeTrue();
});

// ---------------------------------------------------------------------------------------------
// CONNECTION POOLING TESTS
// ---------------------------------------------------------------------------------------------

test('Two PhpClient instances reuse the same underlying TCP connection (pool)', function () {
    // Description: Tests that two PhpClient instances reuse the same underlying TCP connection.
    $opts = [
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ];

    $c1 = new PhpClient($opts);
    $c1->connect();
    $c2 = new PhpClient($opts);
    $c2->connect();

    $c1->ping();
    $c2->ping();

    $id1 = $c1->getId();
    $id2 = $c2->getId();
    expect($id2)->toBe($id1);

    $ok = mq_wait_for_count(1, 8000);
    if (!$ok) {
        fwrite(STDERR, "\n[diag] connections=" . json_encode(mq_get_connections()) . "\n");
    }
    expect($ok)->toBeTrue();
});

test('Closing one client handle does not drop the shared connection', function () {
    // Description: Tests that closing one client handle does not drop the shared connection.
    $opts = [
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ];

    $c1 = new PhpClient($opts);
    $c1->connect();

    $c2 = new PhpClient($opts);
    $c2->connect();

    $ok = mq_wait_for_count(1, 8000);
    if (!$ok) {
        fwrite(STDERR, "\n[diag] connections=" . json_encode(mq_get_connections()) . "\n");
    }

    expect($ok)->toBeTrue();
    $c1->close();

    $ok = mq_wait_for_count(1);
    if (!$ok) {
        fwrite(STDERR, "\n[diag] connections=" . json_encode(mq_get_connections()) . "\n");
    }
    expect($ok)->toBeTrue();
});

test('Dropping all handles eventually closes the TCP connection (pool uses weak refs)', function () {
    // Description: Tests that dropping all handles eventually closes the TCP connection.
    $opts = [
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ];

    $c1 = new PhpClient($opts);
    $c1->connect();
    $c2 = new PhpClient($opts);
    $c2->connect();

    $c1->ping();
    $c2->ping();

    $ok1 = mq_wait_for_count(1, 8000);
    if (!$ok1) {
        fwrite(STDERR, "\n[diag] connections(before drop)=" . json_encode(mq_get_connections()) . "\n");
    }
    expect($ok1)->toBeTrue();

    $c1->close();
    $c2->close();
    unset($c1, $c2);
    gc_collect_cycles();
    gc_collect_cycles();

    expect(mq_wait_for_count(0, 20000))->toBeTrue();
});

test('PhpClient getId returns same id for pooled connections', function () {
    // Description: Tests that PhpClient getId returns the same id for pooled connections.
    $opts = [
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ];

    $c1 = new PhpClient($opts);
    $c1->connect();
    $id1 = $c1->getId();

    $c2 = new PhpClient($opts);
    $c2->connect();
    $id2 = $c2->getId();

    expect($id1)->toBeString();
    expect($id2)->toBe($id1);
});

// ---------------------------------------------------------------------------------------------
// RECONNECT & SHUTDOWN TESTS
// ---------------------------------------------------------------------------------------------

test('Module shutdown closes pooled connections on process exit (live child)', function () {
    // Description: Tests that the module shutdown closes pooled connections on process exit.
    mq_wait_for_count(0, 4000);
    [
        'php' => $php,
        'ext' => $ext,
    ] = getPhpAndExtPath();
    expect(is_file($ext))->toBeTrue();
    expect(is_executable($php))->toBeTrue();

    $childCode = <<<'PHP'
$c = new Goopil\RabbitRs\PhpClient([
    'host' => '127.0.0.1',
    'port' => 5672,
    'user' => 'test',
    'password' => 'test',
    'vhost' => '/',
]);
$c->connect();
$pid = getmypid();
// Emit debug markers for parent sync
fwrite(STDOUT, "CONNECT_OK pid=$pid\n");
$c->ping();
$id = $c->getId();
fwrite(STDOUT, "READY id=$id pid=$pid\n");
fflush(STDOUT);
// Keep process alive long enough for management to index the connection
sleep(6);
PHP;

    $cmd = sprintf('%s -d display_errors=1 -d error_reporting=-1 -d extension=%s -r %s',
        escapeshellarg($php),
        escapeshellarg($ext),
        escapeshellarg($childCode)
    );

    $descriptors = [
        0 => ['pipe', 'r'],
        1 => ['pipe', 'w'],
        2 => ['pipe', 'w'],
    ];
    $proc = proc_open($cmd, $descriptors, $pipes, null, null, ['bypass_shell' => true]);
    expect(is_resource($proc))->toBeTrue();

    $ready = false;
    $deadline = microtime(true) + 8.0;
    stream_set_blocking($pipes[1], false);
    stream_set_blocking($pipes[2], false);
    $stdout = '';
    $stderr = '';
    while (microtime(true) < $deadline) {
        $chunkOut = stream_get_contents($pipes[1]);
        if ($chunkOut !== false && $chunkOut !== '') { $stdout .= $chunkOut; }
        $chunkErr = stream_get_contents($pipes[2]);
        if ($chunkErr !== false && $chunkErr !== '') { $stderr .= $chunkErr; }
        if (str_contains($stdout, "READY")) { $ready = true; break; }
        usleep(50_000);
    }

    expect($ready)->toBeTrue();

    $one = mq_wait_for_count(1, 12000);

    if (!$one) {
        usleep(1_000_000);
        $one = mq_wait_for_count(1, 2000);
    }

    expect($one)->toBeTrue();

    $stdout .= stream_get_contents($pipes[1]) ?: '';
    $stderr .= stream_get_contents($pipes[2]) ?: '';

    fclose($pipes[0]); fclose($pipes[1]); fclose($pipes[2]);
    proc_close($proc);

    expect(mq_wait_for_count(0, 12000))->toBeTrue();
});

test('Auto-reconnect when broker closes the TCP connection (on-demand ensure)', function () {
    // Description: Tests that auto-reconnect works when the broker closes the TCP connection.
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'reconnect' => [
            'enabled' => true,
            'max_retries' => 6,
            'initial_delay_ms' => 100,
            'max_delay_ms' => 400,
            'total_timeout_ms' => 4000,
            'jitter' => 0.2,
        ],
    ]);
    $c->connect();
    $c->ping();

    expect(mq_wait_for_count(1, 8000))->toBeTrue();

    $killed = mq_close_first_connection();
    expect($killed)->toBeTrue();

    expect($c->ping())->toBeTrue();

    expect(mq_wait_for_count(1, 8000))->toBeTrue();
});

test('Reconnect disabled: ping after broker close throws', function () {
    // Description: Tests that ping after broker close throws when reconnect is disabled.
    $c = new PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
        'reconnect' => [
            'enabled' => false,
        ],
    ]);
    $c->connect();
    $c->ping();

    expect(mq_wait_for_count(1, 8000))->toBeTrue();

    $killed = mq_close_first_connection();
    expect($killed)->toBeTrue();

    expect(mq_wait_for_count(0, 8000))->toBeTrue();
    usleep(150_000);

    expect(fn () => var_dump($c->ping()))->toThrow(Exception::class);
});
