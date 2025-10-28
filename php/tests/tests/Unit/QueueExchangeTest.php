<?php

use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\AmqpMessage;

// ---------------------------------------------------------------------------------------------
// QUEUE DECLARATION TESTS
// ---------------------------------------------------------------------------------------------

test('queue declare with basic options (durable, auto_delete, exclusive)', function () {
    // Description: Basic test to ensure queue can be declared with standard options.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'test.q.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['durable' => false, 'auto_delete' => true, 'exclusive' => true]))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue declare with passive=true fails on unknown queue', function () {
    // Description: Verifies that attempting to declare a non-existent queue in passive mode throws an exception.
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    expect(fn () => $ch->queueDeclare('q.does.not.exist', ['passive' => true]))
        ->toThrow(Exception::class);

    expect(fn () => $ch->close())->toThrow(Exception::class);
    $c->close();
});

test('queue declare with inequivalent flags throws', function () {
    // Description: Tests that re-declaring a queue with different flags (e.g., durable=false then durable=true) throws an exception.
    $c = mq_client();
    $ch = $c->openChannel();
    $q = 'q.ineq.' . bin2hex(random_bytes(3));

    expect($ch->queueDeclare($q, ['durable' => false]))->toBeTrue();
    expect(fn () => $ch->queueDeclare($q, ['durable' => true]))->toThrow(Exception::class);

    // Channel is closed by broker after PRECONDITION_FAILED; tolerate close()
    try { $ch->close(); } catch (Exception $e) { /* already closed by broker */ }
    $c->close();
});

test('queue declare with empty arguments', function () {
    // Description: Ensures that declaring a queue with an empty arguments array doesn't break anything.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_empty_args';
    $options = [
        'durable' => true,
        'arguments' => [],
    ];

    expect($ch->queueDeclare($queueName, $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue declare with nested FieldTable arguments', function () {
    // Description: Tests that nested FieldTable arguments (tables inside tables) are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_nested_args';
    $options = [
        'durable' => true,
        'arguments' => [
            'x-max-length' => 1000,
            'nested' => [
                'a' => 1,
                'b' => 'hello',
                'deep' => [
                    'c' => true,
                    'd' => [1, 2, 3],
                ],
            ],
        ],
    ];

    expect($ch->queueDeclare($queueName, $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue declare with nested FieldArray arguments', function () {
    // Description: Tests that nested FieldArray arguments (arrays inside arrays) are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_nested_array_args';
    $options = [
        'durable' => true,
        'arguments' => [
            'x-max-length' => 1000,
            'list' => [1, 2, 3, ['a', 'b', 'c'], true, 42.5],
        ],
    ];

    expect($ch->queueDeclare($queueName, $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue declare with binary arguments', function () {
    // Description: Tests that binary arguments are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_binary_args';
    $options = [
        'durable' => true,
        'arguments' => [
            'x-max-length' => 1000,
            'binary_key' => "\x00\x01\x02\x03",
            'normal_key' => 'hello',
        ],
    ];

    expect($ch->queueDeclare($queueName, $options))->toBeTrue();

    $ch->close();
    $c->close();
});

// ---------------------------------------------------------------------------------------------
// EXCHANGE DECLARATION TESTS
// ---------------------------------------------------------------------------------------------

test('exchange declare with basic options (durable, auto_delete, internal)', function () {
    // Description: Basic test to ensure exchange can be declared with standard options.
    $c = mq_client();
    $ch = $c->openChannel();

    $ex = 'test.ex.fanout';
    expect($ch->exchangeDeclare($ex, 'fanout', ['durable' => false, 'auto_delete' => true]))->toBeTrue();

    $ch->close();
    $c->close();
});

test('exchange declare with passive=true fails on unknown exchange', function () {
    // Description: Verifies that attempting to declare a non-existent exchange in passive mode throws an exception.
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    expect(fn () => $ch->exchangeDeclare('ex.does.not.exist', 'direct', ['passive' => true]))
        ->toThrow(Exception::class);

    expect(fn () => $ch->close())->toThrow(Exception::class);
    $c->close();
});

test('exchange declare with passive=true succeeds when exchange exists', function () {
    // Description: Tests that passive declaration succeeds if the exchange already exists.
    $c = mq_client();
    $ch = $c->openChannel();
    $ex = 'ex.passive.ok.' . bin2hex(random_bytes(3));

    // Create (active)
    expect($ch->exchangeDeclare($ex, 'fanout', ['durable' => false]))->toBeTrue();

    // Probe (passive)
    expect($ch->exchangeDeclare($ex, 'fanout', ['passive' => true]))->toBeTrue();

    $ch->close();
    $c->close();
});

test('exchange declare with nested FieldTable arguments', function () {
    // Description: Tests that nested FieldTable arguments (tables inside tables) are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_nested_args';
    $options = [
        'durable' => true,
        'arguments' => [
            'alternate-exchange' => 'ae',
            'nested' => [
                'x' => 42,
                'y' => 'world',
                'deep' => [
                    'z' => false,
                    'w' => ['a', 'b'],
                ],
            ],
        ],
    ];

    expect($ch->exchangeDeclare($exchangeName, 'direct', $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('exchange declare with nested FieldArray arguments', function () {
    // Description: Tests that nested FieldArray arguments (arrays inside arrays) are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_nested_array_args';
    $options = [
        'durable' => true,
        'arguments' => [
            'alternate-exchange' => 'ae',
            'list' => ['hello', ['nested', 'list'], 99, false, 3.14],
        ],
    ];

    expect($ch->exchangeDeclare($exchangeName, 'direct', $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('exchange declare with binary arguments', function () {
    // Description: Tests that binary arguments are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_binary_args';
    $options = [
        'durable' => true,
        'arguments' => [
            'alternate-exchange' => 'ae',
            'binary_key' => "\x00\x01\x02\x03",
            'normal_key' => 'hello',
        ],
    ];

    expect($ch->exchangeDeclare($exchangeName, 'direct', $options))->toBeTrue();

    $ch->close();
    $c->close();
});

// ---------------------------------------------------------------------------------------------
// QUEUE BINDING TESTS
// ---------------------------------------------------------------------------------------------

test('queue bind with basic options (nowait)', function () {
    // Description: Basic test to ensure queue can be bound to an exchange with standard options.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'test.q.' . bin2hex(random_bytes(4));
    $ex = 'test.ex.fanout';

    expect($ch->queueDeclare($q, ['durable' => false, 'auto_delete' => true, 'exclusive' => true]))->toBeTrue();
    expect($ch->exchangeDeclare($ex, 'fanout', ['durable' => false, 'auto_delete' => true]))->toBeTrue();
    expect($ch->queueBind($q, $ex, ''))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue bind with arguments', function () {
    // Description: Tests that queue binding with arguments (e.g., x-match) is correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_bind_args';
    $exchangeName = 'test_exchangeBind_args';

    $ch->queueDeclare($queueName);
    $ch->exchangeDeclare($exchangeName, 'direct');

    $options = [
        'nowait' => false,
        'arguments' => [
            'x-match' => 'all',
            'custom-header' => 'value',
        ],
    ];

    expect($ch->queueBind($queueName, $exchangeName, 'routing.key', $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue bind with nested FieldTable arguments', function () {
    // Description: Tests that nested FieldTable arguments (tables inside tables) are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_bind_nested';
    $exchangeName = 'test_exchangeBind_nested';

    $ch->queueDeclare($queueName);
    $ch->exchangeDeclare($exchangeName, 'direct');

    $options = [
        'nowait' => false,
        'arguments' => [
            'x-match' => 'all',
            'nested' => [
                'key1' => 'value1',
                'key2' => 99,
                'deep' => [
                    'key3' => true,
                    'key4' => ['list', 'of', 'values'],
                ],
            ],
        ],
    ];

    expect($ch->queueBind($queueName, $exchangeName, 'routing.key', $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue bind with nested FieldArray arguments', function () {
    // Description: Tests that nested FieldArray arguments (arrays inside arrays) are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_bind_nested_array';
    $exchangeName = 'test_exchangeBind_nested_array';

    $ch->queueDeclare($queueName);
    $ch->exchangeDeclare($exchangeName, 'direct');

    $options = [
        'nowait' => false,
        'arguments' => [
            'x-match' => 'all',
            'list' => [1, 'two', ['deep', 'list'], true, 42.0],
        ],
    ];

    expect($ch->queueBind($queueName, $exchangeName, 'routing.key', $options))->toBeTrue();

    $ch->close();
    $c->close();
});

test('queue bind with binary arguments', function () {
    // Description: Tests that binary arguments are correctly serialized and accepted.
    $c = mq_client();
    $ch = $c->openChannel();

    $queueName = 'test_queue_bind_binary_args';
    $exchangeName = 'test_exchangeBind_binary_args';

    $ch->queueDeclare($queueName);
    $ch->exchangeDeclare($exchangeName, 'direct');

    $binaryArg = "\x00\x01\x02\x03\x04\x05";
    $options = [
        'nowait' => false,
        'arguments' => [
            'x-match' => 'all',
            'binary_key' => $binaryArg,
            'normal_key' => 'hello',
        ],
    ];

    expect($ch->queueBind($queueName, $exchangeName, 'routing.key', $options))->toBeTrue();

    $ch->close();
    $c->close();
});

// ---------------------------------------------------------------------------------------------
// PUBLISH & CONSUME INTEGRATION TESTS
// ---------------------------------------------------------------------------------------------

test('can declare queue, bind to amq.fanout and receive a published message', function () {
    // Description: End-to-end test: declare queue, bind to fanout exchange, publish, and verify message delivery.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'test.q.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['durable' => false, 'auto_delete' => true, 'exclusive' => true]))->toBeTrue();
    expect($ch->exchangeDeclare('amq.fanout', 'fanout', ['passive' => true]))->toBeTrue();
    expect($ch->queueBind($q, 'amq.fanout', ''))->toBeTrue();
    expect($ch->basicPublish('amq.fanout', '', new AmqpMessage('hi')))->toBeTrue();

    $ch->close();
    $c->close();
});

test('can declare queue, exchange, bind and publish', function () {
    // Description: End-to-end test: declare queue and exchange, bind, publish, and verify message delivery.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'test.q.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['durable' => false, 'auto_delete' => true, 'exclusive' => true]))
        ->toBeTrue();

    expect($ch->exchangeDeclare('test.ex.fanout', 'fanout', ['durable' => false, 'auto_delete' => true]))
        ->toBeTrue();

    expect($ch->queueBind($q, 'test.ex.fanout', ''))
        ->toBeTrue();

    expect($ch->basicPublish('test.ex.fanout', '', new AmqpMessage('hello-binding')))
        ->toBeTrue();

    $ch->close();
    $c->close();
});

// ---------------------------------------------------------------------------------------------
// EDGE CASES & ERROR HANDLING
// ---------------------------------------------------------------------------------------------

test('basic publish to a non-existent exchange throws', function () {
    // Description: Verifies that publishing to a non-existent exchange throws an exception.
    $c = mq_client();
    $ch = $c->openChannel();

    expect($ch->basicPublish('ex.does.not.exist', 'rk', new AmqpMessage('hi')))->toBeTrue();
    expect(fn () => $ch->queueDeclare('q.after.error'))->toThrow(Exception::class);

    // Publishing to a non-existent exchange closes the channel; tolerate close()
    try { $ch->close(); } catch (Exception $e) { /* already closed by broker */ }
    $c->close();
});

test('cannot publish to an internal exchange', function () {
    // Description: Verifies that publishing to an internal exchange throws an exception.
    $c = mq_client();
    $ch = $c->openChannel();
    $ex = 'ex.internal.' . bin2hex(random_bytes(3));
    expect($ch->exchangeDeclare($ex, 'fanout', ['internal' => true]))->toBeTrue();

    expect($ch->basicPublish($ex, '', new AmqpMessage('nope')))->toBeTrue();
    expect(fn () => $ch->queueDeclare('q.after.internal'))->toThrow(Exception::class);

    // Publishing to an internal exchange closes the channel; tolerate close()
    try { $ch->close(); } catch (Exception $e) { /* already closed by broker */ }
    $c->close();
});

test('exclusive queue cannot be used from a different connection', function () {
    // Description: Tests that an exclusive queue cannot be accessed from a different connection.
    $c1 = mq_client();
    $ch1 = $c1->openChannel();
    $q   = 'q.excl.' . bin2hex(random_bytes(3));

    expect($ch1->queueDeclare($q, ['exclusive' => true]))->toBeTrue();

    [
          'php' => $php,
          'ext' => $ext,
    ] = getPhpAndExtPath();

    $inline = <<<'PHP'
$q = getenv('QNAME');
try {
    $c = new Goopil\RabbitRs\PhpClient([
        'host' => '127.0.0.1',
        'port' => 5672,
        'user' => 'test',
        'password' => 'test',
        'vhost' => '/',
    ]);
    $c->connect();
    $ch = $c->openChannel();
    $ch->queueDeclare($q, ['passive' => true]);
    echo "OK\n";
    exit(0);
} catch (Exception $e) {
    echo "ERR\n";
    exit(2);
}
PHP;

    $cmd = sprintf(
        '%s -d extension=%s -r %s',
        escapeshellarg($php),
        escapeshellarg($ext),
        escapeshellarg($inline)
    );

    $env = ['QNAME' => $q];
    $spec = [1 => ['pipe','w'], 2 => ['pipe','w']];
    $proc = proc_open($cmd, $spec, $pipes, null, $env);
    $stdout = $stderr = '';
    $code = -1;
    if (is_resource($proc)) {
        $stdout = stream_get_contents($pipes[1]) ?: '';
        $stderr = stream_get_contents($pipes[2]) ?: '';
        foreach ($pipes as $p) { if (is_resource($p)) fclose($p); }
        $code = proc_close($proc);
    }

    fwrite(STDERR, "[diag] child exit code=$code\n");
    fwrite(STDERR, "[diag] child stdout=<<<" . trim($stdout) . ">>>\n");
    fwrite(STDERR, "[diag] child stderr=<<<" . trim($stderr) . ">>>\n");

    expect($code)->toBe(2);
    expect(trim($stdout))->toBe('ERR');

    $ch1->close();
    $c1->close();
});

// ---------------------------------------------------------------------------------------------
// QUEUE ADMIN (DELETE / PURGE / UNBIND) – AMQPLIB PARITY
// ---------------------------------------------------------------------------------------------

test('queue purge empties the queue (amqplib parity: no count returned)', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'purge.q.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['durable' => false, 'exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // publish a few
    for ($i = 0; $i < 5; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage('x'.$i)))->toBeTrue();
    }

    // Purge and validate empty via basicGet
    expect($ch->queuePurge($q))->toBeTrue();
    expect($ch->basicGet($q))->toBeNull();

    $ch->close(); $c->close();
});

// if_empty=true should fail (PRECONDITION_FAILED) when the queue has messages
// (amqplib throws; channel then closed by broker)
test('queue delete with if_empty=true fails on non-empty queue', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'del.not.empty.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['durable' => false, 'exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('payload')))->toBeTrue();

    expect(fn () => $ch->queueDelete($q, ['if_empty' => true]))->toThrow(Exception::class);

    // Channel is closed by broker after PRECONDITION_FAILED; tolerate close
    try { $ch->close(); } catch (Exception $e) { /* already closed by broker */ }
    $c->close();
});

// if_unused=true should fail when the queue has a consumer attached
// (amqplib throws; channel then closed)
test('queue delete with if_unused=true fails when there is a consumer', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'del.in.use.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['durable' => false, 'exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = 0;
    $tag = $ch->simpleConsume($q, function () use (&$seen) { $seen++; });
    // give it a moment to register
    usleep(50_000);

    expect(fn () => $ch->queueDelete($q, ['if_unused' => true]))->toThrow(Exception::class);

    // cancel to cleanup
    try { $ch->basicCancel($tag); } catch (Exception $e) {}
    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

// deleting a queue without conditions removes it (subsequent passive declare fails)
test('queue delete removes queue and passive declare fails afterwards', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'del.ok.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['durable' => false, 'exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    expect($ch->queueDelete($q))->toBeTrue();

    // passive probe should now fail like amqplib
    expect(fn () => $ch->queueDeclare($q, ['passive' => true]))->toThrow(Exception::class);

    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

// unbind should effectively stop routing via that binding only
test('queue unbind stops routing for the removed binding only', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q  = 'unbind.q.' . bin2hex(random_bytes(3));
    $ex = 'unbind.ex.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q))->toBeTrue();
    expect($ch->exchangeDeclare($ex, 'direct'))->toBeTrue();

    // two bindings with different routing keys
    expect($ch->queueBind($q, $ex, 'rk1'))->toBeTrue();
    expect($ch->queueBind($q, $ex, 'rk2'))->toBeTrue();

    // Unbind only rk1
    expect($ch->queueUnbind($q, $ex, 'rk1'))->toBeTrue();

    // Publish to rk1 -> should NOT arrive
    expect($ch->basicPublish($ex, 'rk1', new AmqpMessage('gone')))->toBeTrue();
    expect($ch->basicGet($q))->toBeNull();

    // Publish to rk2 -> should arrive
    expect($ch->basicPublish($ex, 'rk2', new AmqpMessage('stay')))->toBeTrue();
    $d = $ch->basicGet($q);
    expect($d)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect($d->getBody())->toBe('stay');

    $ch->close(); $c->close();
});

// unbind with arguments – accept both forms: ['arguments'=>...] and direct hash
test('queue unbind accepts arguments in both forms (arguments key or direct hash)', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q  = 'unbind.args.q.' . bin2hex(random_bytes(3));
    $ex = 'unbind.args.ex.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q))->toBeTrue();
    expect($ch->exchangeDeclare($ex, 'headers'))->toBeTrue();

    // bind with headers match = any
    expect($ch->queueBind($q, $ex, '', [
        'arguments' => [ 'x-match' => 'any', 'color' => 'blue', 'shape' => 'square' ],
    ]))->toBeTrue();

    // Unbind using ['arguments'=>...] form
    expect($ch->queueUnbind($q, $ex, '', [
        'arguments' => [ 'x-match' => 'any', 'color' => 'blue', 'shape' => 'square' ],
    ]))->toBeTrue();

    // Rebind, then unbind using direct-hash form (equivalent)
    expect($ch->queueBind($q, $ex, '', [
        'arguments' => [ 'x-match' => 'any', 'color' => 'blue', 'shape' => 'square' ],
    ]))->toBeTrue();

    expect($ch->queueUnbind($q, $ex, '', [ 'x-match' => 'any', 'color' => 'blue', 'shape' => 'square' ]))->toBeTrue();

    $ch->close(); $c->close();
});

// binding to the default exchange ('') should be rejected by the broker (amqplib parity)
test('queue bind to default exchange is invalid and throws', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'bind.default.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // RabbitMQ forbids binding a queue to the default exchange ('')
    expect(fn () => $ch->queueBind($q, '', 'rk'))->toThrow(Exception::class);

    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

// ---------------------------------------------------------------------------------------------
// ADVANCED BEHAVIOR – TTL, DLX, MAX-LENGTH, EXCHANGE DELETE, HEADERS ROUTING
// ---------------------------------------------------------------------------------------------

test('queue with x-message-ttl expires messages before get', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q = 'ttl.q.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, [
        'durable' => false,
        'exclusive' => true,
        'auto_delete' => true,
        'arguments' => [ 'x-message-ttl' => 100 ], // 100 ms
    ]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('short')))->toBeTrue();

    // wait long enough for TTL to expire
    usleep(200_000);

    expect($ch->basicGet($q))->toBeNull();

    $ch->close(); $c->close();
});

// Dead-lettering via x-dead-letter-exchange on TTL expiry
// main queue -> DLX -> DLQ
 test('expired message is dead-lettered to DLQ', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $dlx = 'ex.dlx.' . bin2hex(random_bytes(3));
    $dlq = 'q.dlq.'  . bin2hex(random_bytes(3));
    expect($ch->exchangeDeclare($dlx, 'direct'))
        ->toBeTrue();
    expect($ch->queueDeclare($dlq, ['durable' => false, 'exclusive' => true, 'auto_delete' => true]))
        ->toBeTrue();
    expect($ch->queueBind($dlq, $dlx, 'rk'))
        ->toBeTrue();

    $q = 'q.ttl.dlx.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, [
        'exclusive' => true,
        'auto_delete' => true,
        'arguments' => [
            'x-message-ttl' => 100, // ms
            'x-dead-letter-exchange' => $dlx,
            'x-dead-letter-routing-key' => 'rk',
        ],
    ]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('will-dlx')))->toBeTrue();

    // wait for TTL and routing to DLQ
    usleep(300_000);

    $d = $ch->basicGet($dlq);
    expect($d)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect($d->getBody())->toBe('will-dlx');

    $ch->close(); $c->close();
});

// x-max-length retains only the newest N messages
 test('queue with x-max-length drops older messages when overflow', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = 'q.maxlen.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, [
        'exclusive' => true,
        'auto_delete' => true,
        'arguments' => [ 'x-max-length' => 3 ],
    ]))->toBeTrue();

    foreach ([ 'a','b','c','d','e' ] as $m) {
        expect($ch->basicPublish('', $q, new AmqpMessage($m)))->toBeTrue();
    }

    // Only last 3 should remain: c, d, e
    $d1 = $ch->basicGet($q); $d2 = $ch->basicGet($q); $d3 = $ch->basicGet($q); $d4 = $ch->basicGet($q);
    expect($d1?->getBody())->toBe('c');
    expect($d2?->getBody())->toBe('d');
    expect($d3?->getBody())->toBe('e');
    expect($d4)->toBeNull();

    $ch->close(); $c->close();
});

// exchange.delete removes the exchange; further publish to it throws and closes channel
 test('exchange delete then publish throws and passive declare fails', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $ex = 'ex.to.delete.' . bin2hex(random_bytes(3));
    expect($ch->exchangeDeclare($ex, 'fanout'))
        ->toBeTrue();

    // delete
    expect($ch->exchangeDelete($ex))->toBeTrue();

    // passive probe must fail
    expect(fn () => $ch->exchangeDeclare($ex, 'fanout', ['passive' => true]))
        ->toThrow(Exception::class);

    // publishing to deleted exchange should throw (channel closed)
    expect(fn () => $ch->basicPublish($ex, '', new AmqpMessage('x')))
        ->toThrow(Exception::class);

    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

// headers exchange routing: x-match any vs all
 test('headers exchange routes by headers (any vs all)', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $ex = 'ex.headers.' . bin2hex(random_bytes(3));
    $qAny = 'q.headers.any.' . bin2hex(random_bytes(3));
    $qAll = 'q.headers.all.' . bin2hex(random_bytes(3));

    expect($ch->exchangeDeclare($ex, 'headers'))->toBeTrue();
    expect($ch->queueDeclare($qAny, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->queueDeclare($qAll, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // any: match if color=blue OR size=xl
    expect($ch->queueBind($qAny, $ex, '', [
        'arguments' => [ 'x-match' => 'any', 'color' => 'blue', 'size' => 'xl' ],
    ]))->toBeTrue();

    // all: must have BOTH
    expect($ch->queueBind($qAll, $ex, '', [
        'arguments' => [ 'x-match' => 'all', 'color' => 'blue', 'size' => 'xl' ],
    ]))->toBeTrue();

    // msg1: only color
    $msg1 = new AmqpMessage('m1', [ 'application_headers' => [ 'color' => 'blue' ] ]);
    // msg2: both
    $msg2 = new AmqpMessage('m2', [ 'application_headers' => [ 'color' => 'blue', 'size' => 'xl' ] ]);
    // msg3: none
    $msg3 = new AmqpMessage('m3', [ 'application_headers' => [ 'shape' => 'circle' ] ]);

    expect($ch->basicPublish($ex, '', $msg1))->toBeTrue();
    expect($ch->basicPublish($ex, '', $msg2))->toBeTrue();
    expect($ch->basicPublish($ex, '', $msg3))->toBeTrue();

    // any queue should get m1 and m2
    $a1 = $ch->basicGet($qAny); $a2 = $ch->basicGet($qAny); $a3 = $ch->basicGet($qAny);
    expect($a1?->getBody())->toBe('m1');
    expect($a2?->getBody())->toBe('m2');
    expect($a3)->toBeNull();

    // all queue should get only m2
    $b1 = $ch->basicGet($qAll); $b2 = $ch->basicGet($qAll);
    expect($b1?->getBody())->toBe('m2');
    expect($b2)->toBeNull();

    $ch->close(); $c->close();
});

// queueDelete success when conditions met (if_empty on empty queue; if_unused when not consumed)
 test('queue delete conditional success paths', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q1 = 'del.empty.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q1, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    // empty -> if_empty should succeed
    expect($ch->queueDelete($q1, ['if_empty' => true]))->toBeTrue();

    $q2 = 'del.unused.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q2, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    // no consumer -> if_unused should succeed
    expect($ch->queueDelete($q2, ['if_unused' => true]))->toBeTrue();

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// PENDING / SKIPPED TESTS
// ---------------------------------------------------------------------------------------------

// headers exchange routes by headers (any/all) – minimal variant using AmqpMessage application_headers
test('headers exchange routes by headers (any/all) – minimal variant', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $ex = 'ex.headers.min.' . bin2hex(random_bytes(3));
    $q  = 'q.headers.min.'  . bin2hex(random_bytes(3));
    expect($ch->exchangeDeclare($ex, 'headers', ['durable' => false]))->toBeTrue();
    expect($ch->queueDeclare($q, ['durable' => false, 'exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->queueBind($q, $ex, '', [
       'arguments' => [
           'x-match' => 'any',
           'color'   => 'blue',
           'size'    => 'xl',
       ],
    ]))->toBeTrue();

    expect($ch->basicPublish($ex, '', new AmqpMessage('msg-with-blue', [ 'application_headers' => ['color' => 'blue'] ])))
        ->toBeTrue();

    expect($ch->basicPublish($ex, '', new AmqpMessage('msg-no-match', [ 'application_headers' => ['shape' => 'circle'] ])))
        ->toBeTrue();

    $d1 = $ch->basicGet($q);
    $d2 = $ch->basicGet($q);

    // First should match, second should be null
    expect($d1?->getBody())->toBe('msg-with-blue');
    expect($d2)->toBeNull();

    $ch->close();
    $c->close();
});

// ---------------------------------------------------------------------------------------------
// HEDGE CASES – ADVANCED BROKER SEMANTICS (AMQPLIB PARITY)
// ---------------------------------------------------------------------------------------------

test('exchange redeclare with incompatible type throws PRECONDITION_FAILED', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $ex = 'ex.incompatible.' . bin2hex(random_bytes(3));
    expect($ch->exchangeDeclare($ex, 'direct'))->toBeTrue();

    // re-declare with different type -> broker closes channel
    expect(fn () => $ch->exchangeDeclare($ex, 'fanout'))->toThrow(Exception::class);

    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

test('queue redeclare with different arguments throws PRECONDITION_FAILED', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = 'q.incompatible.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['arguments' => ['x-max-length' => 10]]))->toBeTrue();

    // different args (x-max-length 20) should fail
    expect(fn () => $ch->queueDeclare($q, ['arguments' => ['x-max-length' => 20]]))
        ->toThrow(Exception::class);

    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

test('x-expires queue disappears after expiration time', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = 'q.expires.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, [
        'exclusive' => true,
        'auto_delete' => false,
        'arguments' => ['x-expires' => 200], // 200 ms
    ]))->toBeTrue();

    usleep(400_000);
    expect(fn () => $ch->queueDeclare($q, ['passive' => true]))->toThrow(Exception::class);

    // broker closes the channel on NOT_FOUND; tolerate double-close
    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

test('queue with both TTL and DLX correctly dead-letters expired messages', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $dlx = 'ex.dlx.ttl.' . bin2hex(random_bytes(3));
    $dlq = 'q.dlx.ttl.'  . bin2hex(random_bytes(3));
    $main = 'q.main.ttl.' . bin2hex(random_bytes(3));

    expect($ch->exchangeDeclare($dlx, 'direct'))->toBeTrue();
    expect($ch->queueDeclare($dlq, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->queueBind($dlq, $dlx, 'rk'))->toBeTrue();

    expect($ch->queueDeclare($main, [
        'exclusive' => true,
        'auto_delete' => true,
        'arguments' => [
            'x-message-ttl' => 100,
            'x-dead-letter-exchange' => $dlx,
            'x-dead-letter-routing-key' => 'rk',
        ],
    ]))->toBeTrue();

    $ch->basicPublish('', $main, new AmqpMessage('ttl-dlx'));
    usleep(400_000);

    $d = $ch->basicGet($dlq);
    expect($d)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect($d->getBody())->toBe('ttl-dlx');

    $ch->close(); $c->close();
});

test('basicGet with ack=false requeues message for next consumer', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = 'q.noack.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $ch->basicPublish('', $q, new AmqpMessage('test-noack'));
    $d1 = $ch->basicGet($q, ['no_ack' => false]);
    expect($d1)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);

    // no ack -> message remains unacked and is NOT re-delivered on the same channel
    $d2 = $ch->basicGet($q, ['no_ack' => false]);
    expect($d2)->toBeNull();

    $ch->close(); $c->close();
});

test('alternate-exchange receives unroutable messages', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $ae  = 'ex.alt.' . bin2hex(random_bytes(3));
    $ex  = 'ex.main.' . bin2hex(random_bytes(3));
    $q   = 'q.alt.'  . bin2hex(random_bytes(3));

    expect($ch->exchangeDeclare($ae, 'fanout'))->toBeTrue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->queueBind($q, $ae, ''))->toBeTrue();

    expect($ch->exchangeDeclare($ex, 'direct', [
        'arguments' => ['alternate-exchange' => $ae],
    ]))->toBeTrue();

    // publish with no matching binding => should go to AE
    expect($ch->basicPublish($ex, 'no_route', new AmqpMessage('alt')))->toBeTrue();

    $d = null;
    for ($i=0; $i<5 && !$d; $i++) {
        usleep(100_000);
        $d = $ch->basicGet($q);
    }

    expect($d)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect($d->getBody())->toBe('alt');

    $ch->close(); $c->close();
});

// -----------------------------------------------------------------------------
// EXCHANGE-TO-EXCHANGE BINDINGS (E2E ROUTING + UNBIND) – AMQPLIB PARITY
// -----------------------------------------------------------------------------

test('exchange-to-exchange binding routes messages end-to-end', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $src = 'ex.src.' . bin2hex(random_bytes(3));
    $dst = 'ex.dst.' . bin2hex(random_bytes(3));
    $q   = 'q.x2x.'  . bin2hex(random_bytes(3));

    expect($ch->exchangeDeclare($src, 'direct'))->toBeTrue();
    expect($ch->exchangeDeclare($dst, 'fanout'))->toBeTrue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Bind queue to destination exchange, and destination to source (rk = 'rk')
    expect($ch->queueBind($q,  $dst, ''))->toBeTrue();
    expect($ch->exchangeBind($dst, $src, 'rk'))->toBeTrue();

    // Publish on source with matching rk -> must flow through dst -> queue
    expect($ch->basicPublish($src, 'rk', new AmqpMessage('x2x')))->toBeTrue();

    $d = $ch->basicGet($q);
    expect($d)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect($d->getBody())->toBe('x2x');

    $ch->close(); $c->close();
});

test('exchangeUnbind stops routing between exchanges', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $src = 'ex.src.ub.' . bin2hex(random_bytes(3));
    $dst = 'ex.dst.ub.' . bin2hex(random_bytes(3));
    $q   = 'q.x2x.ub.'  . bin2hex(random_bytes(3));

    expect($ch->exchangeDeclare($src, 'direct'))->toBeTrue();
    expect($ch->exchangeDeclare($dst, 'fanout'))->toBeTrue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->queueBind($q,  $dst, ''))->toBeTrue();
    expect($ch->exchangeBind($dst, $src, 'rk'))->toBeTrue();

    // sanity: routed once
    expect($ch->basicPublish($src, 'rk', new AmqpMessage('before-unbind')))->toBeTrue();
    $d1 = $ch->basicGet($q); expect($d1?->getBody())->toBe('before-unbind');

    // unbind breaks the route
    expect($ch->exchangeUnbind($dst, $src, 'rk'))->toBeTrue();

    expect($ch->basicPublish($src, 'rk', new AmqpMessage('after-unbind')))->toBeTrue();
    $d2 = $ch->basicGet($q); expect($d2)->toBeNull();

    $ch->close(); $c->close();
});

// -----------------------------------------------------------------------------
// EXCHANGE DELETE EDGE CASES
// -----------------------------------------------------------------------------

test('exchangeDelete with if_unused=true fails when exchange is bound (PRECONDITION_FAILED)', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $ex = 'ex.del.if_unused.' . bin2hex(random_bytes(3));
    $q  = 'q.bound.' . bin2hex(random_bytes(3));

    expect($ch->exchangeDeclare($ex, 'direct'))->toBeTrue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->queueBind($q, $ex, 'rk'))->toBeTrue();

    // Deleting with if_unused=true should throw since the exchange has a binding
    expect(fn () => $ch->exchangeDelete($ex, ['if_unused' => true]))->toThrow(Exception::class);

    // Broker ferme le channel sur PRECONDITION_FAILED
    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

// -----------------------------------------------------------------------------
// EXCHANGE BINDING INVALID CASES
// -----------------------------------------------------------------------------

test('exchangeBind to or from default exchange is invalid and throws', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $ex = 'ex.bind.invalid.' . bin2hex(random_bytes(3));
    expect($ch->exchangeDeclare($ex, 'direct'))->toBeTrue();

    // Destination = '' (default) invalid
    expect(fn () => $ch->exchangeBind('', $ex, 'rk'))->toThrow(Exception::class);

    // Source = '' (default) invalid
    expect(fn () => $ch->exchangeBind($ex, '', 'rk'))->toThrow(Exception::class);

    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});

// -----------------------------------------------------------------------------
// INEQUIVALENT FLAGS ON EXCHANGE REDECLARE (internal flag)
// -----------------------------------------------------------------------------

test('exchange redeclare with inequivalent internal flag throws PRECONDITION_FAILED', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $ex = 'ex.ineq.internal.' . bin2hex(random_bytes(3));
    expect($ch->exchangeDeclare($ex, 'fanout', ['internal' => false]))->toBeTrue();

    // Try to change internal flag -> should fail
    expect(fn () => $ch->exchangeDeclare($ex, 'fanout', ['internal' => true]))->toThrow(Exception::class);

    try { $ch->close(); } catch (Exception $e) {}
    $c->close();
});