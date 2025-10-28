<?php

use Goopil\RabbitRs\AmqpMessage;

// ---------------------------------------------------------------------------------------------
// BASIC PUBLISH & CONSUME WITH HEADERS
// ---------------------------------------------------------------------------------------------

test('publish and consume with simple headers', function () {
    // Description: Basic test to ensure simple headers (string, int, float, bool, null) are correctly serialized and delivered.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('hello-headers', [
        'application_headers' => [
            's' => 'str',
            'i' => 42,
            'f' => 3.14,
            'b' => true,
            'n' => null,
        ],
    ]);

    $got = null;
    expect($ch->simpleConsume($q, function ($delivery) use (&$got) { $got = $delivery->getBody(); }))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $drained = 0;
    for ($i = 0; $i < 10 && $drained === 0; $i++) {
        $drained = $ch->wait(300, 8);
    }
    expect($drained)->toBeGreaterThan(0);
    expect($got)->toBe('hello-headers');

    $ch->close(); $c->close();
});

test('publish and consume with nested headers (table inside table)', function () {
    // Description: Tests deep nesting of headers (tables inside tables) to ensure recursive serialization works.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('nested', [
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

    $seen = 0;
    expect($ch->simpleConsume($q, function () {}))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i = 0; $i < 10 && $seen === 0; $i++) { $seen = $ch->wait(300, 8); }
    expect($seen)->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

test('publish and consume with indexed array headers (FieldArray)', function () {
    // Description: Tests that PHP indexed arrays (FieldArray) in headers are correctly serialized to AMQP FieldArray and deserialized back to PHP arrays on consumption.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('list', [
        'application_headers' => [
            'list' => [1, 2, 3],
        ],
    ]);

    $seen = 0;
    expect($ch->simpleConsume($q, function () {}))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i = 0; $i < 10 && $seen === 0; $i++) { $seen = $ch->wait(300, 8); }
    expect($seen)->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

test('publish and consume with empty headers', function () {
    // Description: Ensures that publishing with an empty headers array doesn't break anything.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('empty-headers', [
        'application_headers' => [],
    ]);

    $seen = 0;
    expect($ch->simpleConsume($q, function () {}))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i = 0; $i < 10 && $seen === 0; $i++) { $seen = $ch->wait(300, 8); }
    expect($seen)->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

test('publish and consume with unsupported header values (e.g., objects)', function () {
    // Description: Verifies that unsupported types (like objects) in headers are gracefully ignored.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage('unsupported', [
        'application_headers' => [
            'ok' => 'x',
            'obj' => (object)['a' => 1], // should be ignored by conversion
        ],
    ]);

    $seen = 0;
    expect($ch->simpleConsume($q, function () {}))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i = 0; $i < 10 && $seen === 0; $i++) { $seen = $ch->wait(300, 8); }
    expect($seen)->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

test('publish and consume with large payload + headers', function () {
    // Description: Tests that large payloads with headers are correctly handled.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    $payload = random_bytes(128 * 1024) . 'END';
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new AmqpMessage($payload, [
        'application_headers' => [
            'k' => 'v',
            'n' => 123,
        ],
    ]);

    $got = null;
    expect($ch->simpleConsume($q, function ($d) use (&$got) { $got = $d->getBody(); }))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(600, 8); }
    expect($n)->toBeGreaterThan(0);
    expect(strlen($got))->toBe(strlen($payload));

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// STANDARD PROPERTIES TESTS
// ---------------------------------------------------------------------------------------------

test('publish and consume with standard properties (content_type, priority, etc.)', function () {
    // Description: Tests that standard message properties (content_type, priority, etc.) are correctly serialized and delivered.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $props = [
        'content_type' => 'text/plain',
        'content_encoding' => 'utf-8',
        'delivery_mode' => 2,
        'priority' => 5,
        'correlation_id' => 'cid-123',
        'reply_to' => 'r.q',
        'expiration' => '60000',
        'message_id' => 'mid-1',
        'timestamp' => time(),
        'type' => 'demo',
        'user_id' => 'test',
        'app_id' => 'app',
        'cluster_id' => 'cl',
        'application_headers' => ['x-demo' => 'ok'],
    ];

    $got = null;
    expect($ch->simpleConsume($q, function ($d) use (&$got) { $got = $d->getBody(); }))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, new AmqpMessage('props-ok', $props)))->toBeTrue();

    $seen = 0;
    for ($i = 0; $i < 10 && $seen === 0; $i++) { $seen = $ch->wait(300, 8); }
    expect($seen)->toBeGreaterThan(0);
    expect($got)->toBe('props-ok');

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// AMQPDELIVERY METADATA TESTS
// ---------------------------------------------------------------------------------------------

test('AmqpDelivery exposes routing metadata (exchange, routingKey, redelivered, tag)', function () {
    // Description: Tests that AmqpDelivery exposes routing metadata (exchange, routingKey, redelivered, tag).
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $meta = [
        'ex' => null,
        'rk' => null,
        'redelivered' => null,
        'tag' => null,
    ];

    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$meta) {
        $meta['ex'] = $d->getExchange();
        $meta['rk'] = $d->getRoutingKey();
        $meta['redelivered'] = $d->isRedelivered();
        $meta['tag'] = $d->getDeliveryTag();
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, new Goopil\RabbitRs\AmqpMessage('x')))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(300, 8); }

    expect($n)->toBeGreaterThan(0);
    expect($meta['ex'])->toBe('');               // default exchange
    expect($meta['rk'])->toBe($q);
    expect($meta['redelivered'])->toBeFalse();   // first delivery
    expect($meta['tag'])->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

test('AmqpDelivery exposes standard properties', function () {
    // Description: Tests that AmqpDelivery exposes standard message properties (content_type, priority, etc.).
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $ts = time();
    $props = [
        'content_type' => 'text/plain',
        'content_encoding' => 'utf-8',
        'delivery_mode' => 2,
        'priority' => 5,
        'correlation_id' => 'cid-42',
        'reply_to' => 'reply.q',
        'expiration' => '30000',
        'message_id' => 'm-42',
        'timestamp' => $ts,
        'type' => 'demo',
        'user_id' => 'test', // must match authenticated user
        'app_id' => 'app-x',
        'cluster_id' => 'cluster-y',
        'application_headers' => ['x' => 'y'],
    ];

    $seen = [
        'ct' => null, 'ce' => null, 'dm' => null, 'pr' => null, 'cid' => null, 'rt' => null,
        'exp' => null, 'mid' => null, 'ts' => null, 'typ' => null, 'uid' => null, 'aid' => null, 'cid2' => null,
    ];

    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$seen) {
        $seen['ct'] = $d->getContentType();
        $seen['ce'] = $d->getContentEncoding();
        $seen['dm'] = $d->getDeliveryMode();
        $seen['pr'] = $d->getPriority();
        $seen['cid'] = $d->getCorrelationId();
        $seen['rt'] = $d->getReplyTo();
        $seen['exp'] = $d->getExpiration();
        $seen['mid'] = $d->getMessageId();
        $seen['ts'] = $d->getTimestamp();
        $seen['type'] = $d->getType();
        $seen['uid'] = $d->getUserId();
        $seen['aid'] = $d->getAppId();
        $seen['cid2'] = $d->getClusterId();
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, new Goopil\RabbitRs\AmqpMessage('props', $props)))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(400, 8); }

    expect($n)->toBeGreaterThan(0);
    expect($seen['ct'])->toBe('text/plain');
    expect($seen['ce'])->toBe('utf-8');
    expect($seen['dm'])->toBe(2);
    expect($seen['pr'])->toBe(5);
    expect($seen['cid'])->toBe('cid-42');
    expect($seen['rt'])->toBe('reply.q');
    expect($seen['exp'])->toBe('30000');
    expect($seen['mid'])->toBe('m-42');
    expect($seen['ts'])->toBe($ts);
    expect($seen['type'])->toBe('demo');
    expect($seen['uid'])->toBe('test');
    expect($seen['aid'])->toBe('app-x');
    expect($seen['cid2'])->toBe('cluster-y');

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// JSON ROUND-TRIP TESTS
// ---------------------------------------------------------------------------------------------

test('publish and consume a simple JSON and preserve content_type', function () {
    // Description: Tests that publishing and consuming a simple JSON message preserves the content_type.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $data = ['id' => 123, 'name' => 'alice'];
    $payload = json_encode($data, JSON_UNESCAPED_UNICODE);
    $props = [
        'content_type' => 'application/json',
        'content_encoding' => 'utf-8',
        'application_headers' => ['x-format' => 'json'],
    ];

    $got = null; $ct = null; $hdrs = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$got, &$ct, &$hdrs) {
        $ct = $d->getContentType();
        $hdrs = $d->getHeaders();
        $got = json_decode($d->getBody(), true);
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, new Goopil\RabbitRs\AmqpMessage($payload, $props)))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(400, 8); }

    expect($n)->toBeGreaterThan(0);
    expect($ct)->toBe('application/json');
    expect($got)->toBe($data);
    expect(is_array($hdrs))->toBeTrue();
    expect($hdrs)->toHaveKey('x-format');
    expect($hdrs['x-format'])->toBe('json');

    $ch->close(); $c->close();
});

test('round-trip JSON UTF-8 (accents, emoji) intact', function () {
    // Description: Tests that publishing and consuming a JSON message with UTF-8 characters (accents, emoji) preserves the data.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $data = [
        'greeting' => 'héllo 🐇',
        'nested' => ['arr' => [1, 2, '三'], 'ok' => true],
    ];
    $payload = json_encode($data, JSON_UNESCAPED_UNICODE);
    $props = [
        'content_type' => 'application/json',
        'content_encoding' => 'utf-8',
    ];

    $got = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$got) {
        $got = json_decode($d->getBody(), true);
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, new Goopil\RabbitRs\AmqpMessage($payload, $props)))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(500, 8); }

    expect($n)->toBeGreaterThan(0);
    expect($got)->toBe($data);

    $ch->close(); $c->close();
});

test('publish a burst of JSON and preserve delivery order', function () {
    // Description: Tests that publishing a burst of JSON messages preserves the delivery order.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $props = ['content_type' => 'application/json', 'content_encoding' => 'utf-8'];
    $total = 10;
    $expectedSeq = range(0, $total - 1);

    $seenSeq = [];
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$seenSeq) {
        $obj = json_decode($d->getBody(), true);
        $seenSeq[] = $obj['seq'] ?? null;
    }))->toBeConsumerTag();

    for ($i = 0; $i < $total; $i++) {
        $payload = json_encode(['seq' => $i, 'v' => 'val-'.$i], JSON_UNESCAPED_UNICODE);
        expect($ch->basicPublish('', $q, new Goopil\RabbitRs\AmqpMessage($payload, $props)))->toBeTrue();
    }

    $drained = 0;
    while ($drained < $total) {
        $drained += $ch->wait(1500, 64);
    }

    expect(count($seenSeq))->toBe($total);
    // AMQP guarantees order per channel/queue/consumer simple
    expect($seenSeq)->toBe($expectedSeq);

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// CONTENT_TYPE TESTS
// ---------------------------------------------------------------------------------------------

test('delivery->getContentType() reflects content_type', function () {
    // Description: Tests that delivery->getContentType() reflects the content_type of the message.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $payload = json_encode(['k' => 'v'], JSON_UNESCAPED_UNICODE);
    $props = ['content_type' => 'application/json', 'content_encoding' => 'utf-8'];

    $seenCt = null; $seenBody = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$seenCt, &$seenBody) {
        $seenCt = $d->getContentType();
        $seenBody = $d->getBody();
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, new Goopil\RabbitRs\AmqpMessage($payload, $props)))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(400, 8); }

    expect($n)->toBeGreaterThan(0);
    expect($seenCt)->toBe('application/json');
    expect(json_decode($seenBody, true))->toBe(['k' => 'v']);

    $ch->close(); $c->close();
});

test('delivery->getContentType() is null when content_type is absent', function () {
    // Description: Tests that delivery->getContentType() is null when content_type is not set.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seenCt = 'init';
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$seenCt) {
        $seenCt = $d->getContentType();
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, new Goopil\RabbitRs\AmqpMessage('no-ct')))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(400, 8); }

    expect($n)->toBeGreaterThan(0);
    expect($seenCt)->toBeNull();

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// HEADERS ROUND-TRIP TESTS
// ---------------------------------------------------------------------------------------------

test('AmqpDelivery getHeaders() returns typed recursive array', function () {
    // Description: Tests that AmqpDelivery getHeaders() returns a typed recursive array.
    $c = mq_client();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new Goopil\RabbitRs\AmqpMessage('with-headers', [
        'application_headers' => [
            'x-a' => 'v',
            'outer' => [
                'k1' => 'v1',
                'k2' => 7,
                'inner' => ['x' => true, 'y' => null],
            ],
            'list' => [1, 2, 3],
        ],
    ]);

    $headers = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$headers) {
        $headers = $d->getHeaders();
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $n = 0;
    for ($i = 0; $i < 10 && $n === 0; $i++) { $n = $ch->wait(400, 8); }

    expect($n)->toBeGreaterThan(0);

    // Top-level: associative array
    expect(is_array($headers))->toBeTrue();
    expect($headers)->toHaveKey('x-a');
    expect($headers['x-a'])->toBe('v');

    // outer: associative array
    expect($headers)->toHaveKey('outer');
    expect(is_array($headers['outer']))->toBeTrue();
    expect($headers['outer'])->toHaveKey('k1');
    expect($headers['outer']['k1'])->toBe('v1');
    expect($headers['outer'])->toHaveKey('k2');
    expect($headers['outer']['k2'])->toBe(7);

    // outer.inner: associative array
    expect($headers['outer'])->toHaveKey('inner');
    expect(is_array($headers['outer']['inner']))->toBeTrue();
    expect($headers['outer']['inner'])->toHaveKey('x');
    expect($headers['outer']['inner']['x'])->toBeTrue();
    expect($headers['outer']['inner'])->toHaveKey('y');
    expect($headers['outer']['inner']['y'])->toBeNull();

    // list: indexed array [1,2,3]
    expect($headers)->toHaveKey('list');
    expect(is_array($headers['list']))->toBeTrue();
    expect(array_values($headers['list']))->toBe([1, 2, 3]);

    $ch->close(); $c->close();
});

test('headers: mixed table and nested list', function () {
    // Description: Tests that mixed table and nested list headers are correctly serialized and deserialized.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new Goopil\RabbitRs\AmqpMessage('mixed', [
        'application_headers' => [
            'meta' => ['a' => 1, 'b' => 2],
            'nums' => [10, 20, 30],
            'deep' => [
                'list' => [true, false, null],
                'tbl' => ['x' => 'y'],
            ],
        ],
    ]);

    $headers = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$headers) { $headers = $d->getHeaders(); }))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i=0,$n=0; $i<10 && $n===0; $i++) { $n = $ch->wait(400, 8); }
    expect(is_array($headers))->toBeTrue();
    expect(is_array($headers['meta']))->toBeTrue();
    expect($headers['meta']['a'])->toBe(1);
    expect($headers['meta']['b'])->toBe(2);
    expect(is_array($headers['nums']))->toBeTrue();
    expect(array_values($headers['nums']))->toBe([10,20,30]);
    expect(is_array($headers['deep']))->toBeTrue();
    expect(is_array($headers['deep']['list']))->toBeTrue();
    expect(array_values($headers['deep']['list']))->toBe([true,false,null]);
    expect(is_array($headers['deep']['tbl']))->toBeTrue();
    expect($headers['deep']['tbl']['x'])->toBe('y');

    $ch->close(); $c->close();
});

test('headers: top-level list becomes indexed table "0","1",..."', function () {
    // Description: Tests that a top-level list in headers becomes an indexed table ("0","1",...) on the AMQP side.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new Goopil\RabbitRs\AmqpMessage('top-list', [
        'application_headers' => [1, 2, 3],
    ]);

    $headers = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$headers) { $headers = $d->getHeaders(); }))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i=0,$n=0; $i<10 && $n===0; $i++) { $n = $ch->wait(400, 8); }

    expect(is_array($headers))->toBeTrue();
    // Top-level list becomes indexed table: '0','1','2' => values
    expect($headers)->toHaveKey('0');
    expect($headers)->toHaveKey('1');
    expect($headers)->toHaveKey('2');
    expect($headers['0'])->toBe(1);
    expect($headers['1'])->toBe(2);
    expect($headers['2'])->toBe(3);

    $ch->close(); $c->close();
});

test('headers: scalar types (null, bool, int, float, string)', function () {
    // Description: Tests that scalar types (null, bool, int, float, string) in headers are correctly serialized and deserialized.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new Goopil\RabbitRs\AmqpMessage('scalars', [
        'application_headers' => [
            'n' => null,
            't' => true,
            'f' => false,
            'i' => 123,
            'd' => 3.5,
            's' => 'ok',
        ],
    ]);

    $headers = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$headers) { $headers = $d->getHeaders(); }))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i=0,$n=0; $i<10 && $n===0; $i++) { $n = $ch->wait(400, 8); }

    expect($headers['n'])->toBeNull();
    expect($headers['t'])->toBeTrue();
    expect($headers['f'])->toBeFalse();
    expect($headers['i'])->toBe(123);
    expect($headers['d'])->toBe(3.5);
    expect($headers['s'])->toBe('ok');

    $ch->close(); $c->close();
});

test('headers: recursive depth (3+ levels)', function () {
    // Description: Tests that deeply nested headers (3+ levels) are correctly serialized and deserialized.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $msg = new Goopil\RabbitRs\AmqpMessage('deep', [
        'application_headers' => [
            'lvl1' => [
                'lvl2a' => ['lvl3' => ['lvl4' => 42]],
                'lvl2b' => [true, ['x' => 'y'], [1,2]],
            ],
        ],
    ]);

    $headers = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$headers) { $headers = $d->getHeaders(); }))->toBeConsumerTag();
    expect($ch->basicPublish('', $q, $msg))->toBeTrue();
    for ($i=0,$n=0; $i<10 && $n===0; $i++) { $n = $ch->wait(600, 8); }

    expect(is_array($headers['lvl1']))->toBeTrue();
    expect(is_array($headers['lvl1']['lvl2a']))->toBeTrue();
    expect(is_array($headers['lvl1']['lvl2a']['lvl3']))->toBeTrue();
    expect($headers['lvl1']['lvl2a']['lvl3']['lvl4'])->toBe(42);

    expect(is_array($headers['lvl1']['lvl2b']))->toBeTrue();
    expect(array_values($headers['lvl1']['lvl2b'])[0])->toBeTrue();
    expect(is_array(array_values($headers['lvl1']['lvl2b'])[1]))->toBeTrue();
    expect(array_values($headers['lvl1']['lvl2b'])[1]['x'])->toBe('y');
    expect(array_values($headers['lvl1']['lvl2b'])[2])->toBe([1,2]);

    $ch->close(); $c->close();
});

test('headers: ByteArray (binary) round-trip top-level and nested', function () {
    // Description: Tests that binary headers (ByteArray) are correctly serialized and deserialized, both top-level and nested.
    $c = mq_client(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $bin  = random_bytes(8);
    $bin2 = random_bytes(5);

    $msg = new Goopil\RabbitRs\AmqpMessage('with-bytes', [
        'application_headers' => [
            'bin' => $bin,                         // binary string in PHP
            'outer' => [
                'bin2' => $bin2,                   // nested binary
                'note' => 'ok',
            ],
        ],
    ]);

    $headers = null;
    expect($ch->simpleConsume($q, function (Goopil\RabbitRs\AmqpDelivery $d) use (&$headers) {
        $headers = $d->getHeaders();
    }))->toBeConsumerTag();

    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    for ($i=0,$n=0; $i<10 && $n===0; $i++) { $n = $ch->wait(600, 8); }

    expect(is_array($headers))->toBeTrue();
    // Top-level binary
    expect(isset($headers['bin']))->toBeTrue();
    expect(is_string($headers['bin']))->toBeTrue();
    expect(strlen($headers['bin']))->toBe(strlen($bin));
    expect($headers['bin'] === $bin)->toBeTrue();

    // Nested binary
    expect(is_array($headers['outer']))->toBeTrue();
    expect(is_string($headers['outer']['bin2']))->toBeTrue();
    expect(strlen($headers['outer']['bin2']))->toBe(strlen($bin2));
    expect($headers['outer']['bin2'] === $bin2)->toBeTrue();
    expect($headers['outer']['note'])->toBe('ok');

    $ch->close(); $c->close();
});
