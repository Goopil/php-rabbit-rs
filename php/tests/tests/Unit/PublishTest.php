<?php

use Goopil\RabbitRs\AmqpMessage;

// ---------------------------------------------------------------------------------------------
// HEADER TYPES TESTS
// ---------------------------------------------------------------------------------------------

test('basic publish accepts application_headers and succeeds', function () {
    // Description: Basic test to ensure headers are accepted and publishing works.
    $c = mq_client();
    $ch = $c->openChannel();

    $msg = new AmqpMessage('hello-headers', [
        'application_headers' => [
            'x-foo' => 'bar',
        ],
    ]);

    expect($ch->basicPublish('', mh_tmp_queue(), $msg))->toBeTrue();

    $ch->close();
    $c->close();
});

test('basic publish with multiple header types (string, int, float, bool, null)', function () {
    // Description: Verifies that all scalar types (string, int, float, bool, null) are correctly serialized and delivered.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('multi-types', [
        'application_headers' => [
            's' => 'str',
            'i' => 42,
            'f' => 3.14,
            'b' => true,
            'n' => null,
        ],
    ]);

    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $got = null;
    expect($ch->simpleConsume($q, function ($delivery) use (&$got) { $got = $delivery->getBody(); }))->toBeString();
    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(300, 8); }
    expect($drained)->toBeGreaterThan(0);

    $ch->close();
    $c->close();
});

test('basic publish with nested headers (table inside table)', function () {
    // Description: Tests deep nesting of headers (tables inside tables) to ensure recursive serialization works.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('nested-table', [
        'application_headers' => [
            'outer' => [
                'k1' => 'v1',
                'k2' => 7,
                'inner' => [
                    'x' => true,
                    'y' => null,
                ],
            ],
        ],
    ]);

    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $seen = 0;
    expect($ch->simpleConsume($q, function () {}))->toBeString();
    for ($i=0; $i<10 && $seen===0; $i++) { $seen = $ch->wait(300, 8); }
    expect($seen)->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

test('basic publish with empty headers', function () {
    // Description: Ensures that publishing with an empty headers array doesn't break anything.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('empty-headers', [
        'application_headers' => [],
    ]);

    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $ch->close(); $c->close();
});

test('basic publish ignores unsupported header values (e.g., objects)', function () {
    // Description: Verifies that unsupported types (like objects) in headers are gracefully ignored.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $obj = (object) ['a' => 1];
    $msg = new AmqpMessage('unsupported-ok', [
        'application_headers' => [
            'ok' => 'x',
            'obj' => $obj, // should be ignored by conversion
        ],
    ]);

    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $got = null;
    expect($ch->simpleConsume($q, function ($d) use (&$got) { $got = $d->getBody(); }))->toBeString();
    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(300, 8); }
    expect($drained)->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// FIELDARRAY TESTS
// ---------------------------------------------------------------------------------------------

test('basic publish with FieldArray headers and receive them correctly', function () {
    // Description: Tests that PHP indexed arrays (FieldArray) in headers are correctly serialized to AMQP FieldArray
    // and deserialized back to PHP arrays on consumption.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_fieldarray_headers';
    $queueName = 'test_queue_fieldarray_headers';
    $routingKey = 'test';

    $ch->exchangeDeclare($exchangeName, 'direct');
    $ch->queueDeclare($queueName);
    $ch->queueBind($queueName, $exchangeName, $routingKey);

    $fieldArrayHeaders = [
        'list_simple' => [1, 2, 3],
        'list_nested' => [1, ['a', 'b'], true, 42.5],
        'list_mixed' => ['hello', 99, false, ['deep', 'list']],
    ];

    $msg = new AmqpMessage('hello with FieldArray headers', [
        'application_headers' => $fieldArrayHeaders,
    ]);

    expect($ch->basicPublish($exchangeName, $routingKey, $msg))->toBeTrue();

    $receivedHeaders = null;
    expect($ch->simpleConsume($queueName, function ($delivery) use (&$receivedHeaders) {
        $receivedHeaders = $delivery->getHeaders();
    }))->toBeString();

    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(300, 8); }
    expect($drained)->toBeGreaterThan(0);

    // Verify FieldArray is correctly received
    expect($receivedHeaders)->toBeArray();
    expect($receivedHeaders['list_simple'])->toBeArray();
    expect($receivedHeaders['list_simple'][0])->toBe(1);
    expect($receivedHeaders['list_simple'][1])->toBe(2);
    expect($receivedHeaders['list_simple'][2])->toBe(3);

    expect($receivedHeaders['list_nested'])->toBeArray();
    expect($receivedHeaders['list_nested'][0])->toBe(1);
    expect($receivedHeaders['list_nested'][1])->toBeArray();
    expect($receivedHeaders['list_nested'][1][0])->toBe('a');
    expect($receivedHeaders['list_nested'][1][1])->toBe('b');
    expect($receivedHeaders['list_nested'][2])->toBeTrue();
    expect($receivedHeaders['list_nested'][3])->toBe(42.5);

    expect($receivedHeaders['list_mixed'])->toBeArray();
    expect($receivedHeaders['list_mixed'][0])->toBe('hello');
    expect($receivedHeaders['list_mixed'][1])->toBe(99);
    expect($receivedHeaders['list_mixed'][2])->toBeFalse();
    expect($receivedHeaders['list_mixed'][3])->toBeArray();
    expect($receivedHeaders['list_mixed'][3][0])->toBe('deep');
    expect($receivedHeaders['list_mixed'][3][1])->toBe('list');

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// BYTEARRAY TESTS
// ---------------------------------------------------------------------------------------------

test('basic publish with ByteArray headers and receive them correctly', function () {
    // Description: Tests that binary strings (ByteArray) in headers are correctly serialized and deserialized.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_bytearray_headers';
    $queueName = 'test_queue_bytearray_headers';
    $routingKey = 'test';

    $ch->exchangeDeclare($exchangeName, 'direct');
    $ch->queueDeclare($queueName);
    $ch->queueBind($queueName, $exchangeName, $routingKey);

    $binaryHeader = "\x00\x01\x02\x03\x04\x05";
    $msg = new AmqpMessage('hello with binary headers', [
        'application_headers' => [
            'binary' => $binaryHeader,
            'text' => 'normal',
        ],
    ]);

    expect($ch->basicPublish($exchangeName, $routingKey, $msg))->toBeTrue();

    $receivedHeaders = null;
    expect($ch->simpleConsume($queueName, function ($delivery) use (&$receivedHeaders) {
        $receivedHeaders = $delivery->getHeaders();
    }))->toBeString();

    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(300, 8); }
    expect($drained)->toBeGreaterThan(0);

    // Verify ByteArray is correctly received as binary string
    expect($receivedHeaders)->toBeArray();
    expect($receivedHeaders['binary'])->toBeString();
    expect($receivedHeaders['binary'])->toBe($binaryHeader);
    expect($receivedHeaders['text'])->toBe('normal');

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// NULL / VOID TESTS
// ---------------------------------------------------------------------------------------------

test('basic publish with null and void headers and receive them correctly', function () {
    // Description: Tests that `null` values in headers are correctly serialized as AMQP Void and deserialized back to `null`.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_null_void_headers';
    $queueName = 'test_queue_null_void_headers';
    $routingKey = 'test';

    $ch->exchangeDeclare($exchangeName, 'direct');
    $ch->queueDeclare($queueName);
    $ch->queueBind($queueName, $exchangeName, $routingKey);

    $nullVoidHeaders = [
        'null_value' => null,
        'void_value' => null, // PHP has no "void", so we use null to represent "absent"
        'mixed' => [null, 'hello', null],
    ];

    $msg = new AmqpMessage('hello with null and void headers', [
        'application_headers' => $nullVoidHeaders,
    ]);

    expect($ch->basicPublish($exchangeName, $routingKey, $msg))->toBeTrue();

    $receivedHeaders = null;
    expect($ch->simpleConsume($queueName, function ($delivery) use (&$receivedHeaders) {
        $receivedHeaders = $delivery->getHeaders();
    }))->toBeString();

    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(300, 8); }
    expect($drained)->toBeGreaterThan(0);

    // Verify null values are correctly received
    expect($receivedHeaders)->toBeArray();
    expect($receivedHeaders['null_value'])->toBeNull();
    expect($receivedHeaders['void_value'])->toBeNull();
    expect($receivedHeaders['mixed'])->toBeArray();
    expect($receivedHeaders['mixed'][0])->toBeNull();
    expect($receivedHeaders['mixed'][1])->toBe('hello');
    expect($receivedHeaders['mixed'][2])->toBeNull();

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// OTHER PUBLISH TESTS (REMAINING)
// ---------------------------------------------------------------------------------------------

test('basic publish with mandatory flag', function () {
    // Description: Tests that publishing with mandatory=true throws an exception if the exchange doesn't exist.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'non_existent_exchange';
    $routingKey = 'test';

    $options = [
        'mandatory' => true,
    ];

    $this->expectException(\Exception::class);
    $this->expectExceptionMessage('Publish failed');

    $msg = new AmqpMessage('hello');
    $ch->basicPublish($exchangeName, $routingKey, $msg, $options);
    $ch->close(); $c->close();
});

test('basic publish without mandatory flag', function () {
    // Description: Tests that publishing with mandatory=false silently drops the message if the exchange doesn't exist.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'non_existent_exchange';
    $routingKey = 'test';

    $ch->exchangeDeclare($exchangeName, 'direct');

    $options = [
        'mandatory' => false,
    ];

    $msg = new AmqpMessage('hello');
    expect($ch->basicPublish($exchangeName, $routingKey, $msg, $options))->toBeTrue();

    $ch->close(); $c->close();
});

test('basic publish with binary body', function () {
    // Description: Tests that binary message bodies are correctly handled.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_binary';
    $routingKey = 'test';

    $ch->exchangeDeclare($exchangeName, 'direct');

    $binaryBody = "\x00\x01\x02\x03\x04\x05";
    $msg = new AmqpMessage($binaryBody);

    expect($ch->basicPublish($exchangeName, $routingKey, $msg))->toBeTrue();

    $ch->close(); $c->close();
});

test('basic publish with nested headers and receive them', function () {
    // Description: Tests complex nested headers and ensures they are preserved through publish/consume.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_nested_headers';
    $queueName = 'test_queue_nested_headers';
    $routingKey = 'test';

    $ch->exchangeDeclare($exchangeName, 'direct');
    $ch->queueDeclare($queueName);
    $ch->queueBind($queueName, $exchangeName, $routingKey);

    $nestedHeaders = [
        'level1' => [
            'level2' => [
                'level3' => 'deep value',
                'number' => 42,
                'bool' => true,
                'list' => ['a', 'b', 'c'],
            ],
        ],
    ];

    $msg = new AmqpMessage('hello with nested headers', [
        'application_headers' => $nestedHeaders,
    ]);

    expect($ch->basicPublish($exchangeName, $routingKey, $msg))->toBeTrue();

    $receivedHeaders = null;
    expect($ch->simpleConsume($queueName, function ($delivery) use (&$receivedHeaders) {
        $receivedHeaders = $delivery->getHeaders();
    }))->toBeString();

    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(300, 8); }
    expect($drained)->toBeGreaterThan(0);

    expect($receivedHeaders)->toBeArray();
    expect($receivedHeaders['level1']['level2']['level3'])->toBe('deep value');
    expect($receivedHeaders['level1']['level2']['number'])->toBe(42);
    expect($receivedHeaders['level1']['level2']['bool'])->toBeTrue();
    expect($receivedHeaders['level1']['level2']['list'])->toBeArray();
    expect($receivedHeaders['level1']['level2']['list'][0])->toBe('a');

    $ch->close(); $c->close();
});

test('basic publish with typed headers and receive them correctly', function () {
    // Description: Comprehensive test covering all scalar types and nested structures in headers.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $exchangeName = 'test_exchange_typed_headers';
    $queueName = 'test_queue_typed_headers';
    $routingKey = 'test';

    $ch->exchangeDeclare($exchangeName, 'direct');
    $ch->queueDeclare($queueName);
    $ch->queueBind($queueName, $exchangeName, $routingKey);

    $typedHeaders = [
        'bool_true' => true,
        'bool_false' => false,
        'int_positive' => 42,
        'int_negative' => -7,
        'float_positive' => 3.14,
        'float_negative' => -2.718,
        'string' => 'hello',
        'null' => null,
        'list' => [1, 2, 3],
        'table' => ['key' => 'value'],
    ];

    $msg = new AmqpMessage('hello with typed headers', [
        'application_headers' => $typedHeaders,
    ]);

    expect($ch->basicPublish($exchangeName, $routingKey, $msg))->toBeTrue();

    $receivedHeaders = null;
    expect($ch->simpleConsume($queueName, function ($delivery) use (&$receivedHeaders) {
        $receivedHeaders = $delivery->getHeaders();
    }))->toBeString();

    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(300, 8); }
    expect($drained)->toBeGreaterThan(0);

    expect($receivedHeaders)->toBeArray();
    expect($receivedHeaders['bool_true'])->toBeTrue();
    expect($receivedHeaders['bool_false'])->toBeFalse();
    expect($receivedHeaders['int_positive'])->toBe(42);
    expect($receivedHeaders['int_negative'])->toBe(-7);
    expect($receivedHeaders['float_positive'])->toBe(3.14);
    expect($receivedHeaders['float_negative'])->toBe(-2.718);
    expect($receivedHeaders['string'])->toBe('hello');
    expect($receivedHeaders['null'])->toBeNull();
    expect($receivedHeaders['list'])->toBeArray();
    expect($receivedHeaders['list'][0])->toBe(1);
    expect($receivedHeaders['table'])->toBeArray();
    expect($receivedHeaders['table']['key'])->toBe('value');

    $ch->close(); $c->close();
});


test('publish auto-recovers channel after broker restart (if enabled)', function () {
    // Requires docker-compose to restart the broker. Opt-in via env var.
    if ((getenv('E2E_CAN_RESTART_BROKER') ?: '0') !== '1') {
        $this->markTestSkipped('Set E2E_CAN_RESTART_BROKER=1 to run publish auto-recover test.');
    }
    $service = getenv('E2E_RABBIT_SERVICE') ?: 'rabbitmq';

    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // restart broker to invalidate the underlying channel
    shell_exec("docker compose restart {$service} 2>/dev/null");
    sleep(4);

    // publish should transparently recreate a fresh channel and succeed
    $msg = new AmqpMessage('post-restart');
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $got = null;
    expect($ch->simpleConsume($q, function ($d) use (&$got) { $got = $d->getBody(); }))->toBeString();

    $drained = 0; for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(800, 8); }
    expect($drained)->toBeGreaterThan(0);
    expect($got)->toBe('post-restart');

    $ch->close(); $c->close();
});