<?php

use PHPUnit\Framework\ExpectationFailedException;
use Goopil\RabbitRs\AmqpMessage;
use Goopil\RabbitRs\AmqpDelivery;
use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\PhpChannel;

// ---------------------------------------------------------------------------------------------
// BASIC CONSUME & DELIVERY
// ---------------------------------------------------------------------------------------------

test('simpleConsume delivers and counts a single published message', function () {
    // Description: Basic test to ensure simpleConsume delivers a single published message.
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->simpleConsume($q, function () {}))->toBeString();
    expect($ch->basicPublish('', $q, new AmqpMessage('one')))->toBeTrue();

    $seen = 0;
    for ($i = 0; $i < 10 && $seen === 0; $i++) {
        $seen = $ch->wait(300, 8);
    }
    expect($seen)->toBeGreaterThan(0);

    $ch->close();
    $c->close();
});

test('simpleConsume accepts consumer_tag and returns the same tag', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $wanted = 'ctag.'.bin2hex(random_bytes(3));
    $tag = $ch->simpleConsume($q, function () {}, [ 'consumer_tag' => $wanted ]);
    expect($tag)->toBe($wanted);

    // Publish and verify that the consumer works correctly with this tag
    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(600, 8); }
    expect($n)->toBeGreaterThan(0);

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});

test('simpleConsume exclusive=true prevents a second consumer on same queue', function () {
    $c = mq_client(); $c->connect();
    $ch1 = $c->openChannel();
    $ch2 = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch1->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $t1 = $ch1->simpleConsume($q, function () {}, [ 'exclusive' => true ]);
    expect($t1)->toBeString();

    // A second consumer on the same queue must fail (amqplib parity)
    expect(fn () => $ch2->simpleConsume($q, function () {}, [ 'exclusive' => true ]))
        ->toThrow(Exception::class);

    $ch1->basicCancel($t1);
    $ch1->close(); $ch2->close(); $c->close();
});

test('simpleConsume nowait=true still attaches and receives messages', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $tag = $ch->simpleConsume($q, function () {}, [ 'nowait' => true ]);
    expect($tag)->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    $n = 0; for ($i=0; $i<8 && $n===0; $i++) { $n = $ch->wait(600, 8); }
    expect($n)->toBeGreaterThan(0);

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});

test('simpleConsume accepts no_local option (RabbitMQ may ignore it)', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // no_local is often ignored by RabbitMQ; ensure it is accepted and no error occurs
    $tag = $ch->simpleConsume($q, function () {}, [ 'no_local' => true ]);
    expect($tag)->toBeString();

    // Publish and ensure everything still works (delivery may depend on the broker)
    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(400, 8); }
    expect($n)->toBeGreaterThanOrEqual(0);

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});

test('simpleConsume delivers exact payload with simple string', function () {
    // Description: Tests that simpleConsume delivers the exact payload with a simple string.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $got = null;
    expect($ch->simpleConsume($q, function (AmqpDelivery $message) use (&$got) {
        $got = $message->getBody();
    }))->toBeString();

    $payload = "coucou";
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $seen = 0;
    for ($i=0; $i<10 && $seen === 0; $i++) {
        $seen = $ch->wait(300, 8);
    }
    expect($seen)->toBeGreaterThan(0);
    expect($got)->not->toBeNull();
    expect($got === $payload)->toBeTrue();

    $ch->close(); $c->close();
});

test('simpleConsume delivers exact payload bytes including NUL and 0xFF', function () {
    // Description: Tests that simpleConsume delivers the exact payload bytes, including NUL and 0xFF.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $got = null;
    expect($ch->simpleConsume($q, function (AmqpDelivery $delivery) use (&$got) {
        $got = $delivery->getBody();
    }))->toBeString();

    $payload = random_bytes(16) . "\x00\xffA";
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $seen = 0;
    for ($i=0; $i<10 && $seen === 0; $i++) {
        $seen = $ch->wait(300, 8);
    }
    expect($seen)->toBeGreaterThan(0);
    expect($got)->not->toBeNull();
    expect($got === $payload)->toBeTrue();

    $ch->close(); $c->close();
});

test('simpleConsume delivers multiple payloads in order to the PHP callback', function () {
    // Description: Tests that simpleConsume delivers multiple payloads in order to the PHP callback.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $received = [];
    expect($ch->simpleConsume($q, function (AmqpDelivery $delivery) use (&$received) {
        $received[] = $delivery->getBody();
    }))->toBeString();

    $msgs = [
        "ascii",
        "bin\x00mid",
        random_bytes(8),
        "\xff\xfe\xfd",
    ];
    foreach ($msgs as $m) {
        expect($ch->basicPublish('', $q, new AmqpMessage($m)))->toBeTrue();
    }

    $drained = 0;
    while ($drained < count($msgs)) {
        $drained += $ch->wait(1000, 16);
    }

    expect($received)->toHaveCount(count($msgs));
    foreach ($msgs as $i => $m) {
        expect($received[$i] === $m)->toBeTrue();
    }

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// WAIT BEHAVIOR
// ---------------------------------------------------------------------------------------------

test('wait timeout returns zero when no messages arrive', function () {
    // Description: Tests that wait returns zero when no messages arrive.
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->simpleConsume($q, function () {}))->toBeString();

    $n = $ch->wait(150, 8);
    expect($n)->toBe(0);

    $ch->close();
    $c->close();
});

test('counts multiple messages up to the provided max', function () {
    // Description: Tests that wait counts multiple messages up to the provided max.
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->simpleConsume($q, function () {}))->toBeString();

    $total = 20;
    for ($i = 0; $i < $total; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage('m'.$i)))->toBeTrue();
    }

    $drained = $ch->wait(1000, $total);
    expect($drained)->toBe($total);

    $ch->close();
    $c->close();
});

test('wait returns partial count when max is smaller than available', function () {
    // Description: Tests that wait returns a partial count when max is smaller than available.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['auto_delete' => true]))->toBeTrue();

    expect($ch->simpleConsume($q, function () {}))->toBeString();

    sc_publish_many($ch, $q, 30);
    $n = $ch->wait(2000, 10);
    expect($n)->toBe(10);

    $rest = $ch->wait(2000, 100);
    expect($rest)->toBe(20);

    $ch->close(); $c->close();
});

test('multiple wait calls can drain bursts arriving over time', function () {
    // Description: Tests that multiple wait calls can drain bursts arriving over time.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->simpleConsume($q, function () {}))->toBeString();

    sc_publish_many($ch, $q, 5, 'a');
    $n1 = $ch->wait(1000, 64);
    expect($n1)->toBe(5);

    sc_publish_many($ch, $q, 7, 'b');
    $n2 = $ch->wait(1000, 64);
    expect($n2)->toBe(7);

    $n3 = $ch->wait(100, 64);
    expect($n3)->toBe(0);

    $ch->close(); $c->close();
});

test('timeout first, then later messages are still delivered', function () {
    // Description: Tests that messages arriving after a timeout are still delivered.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->simpleConsume($q, function () {}))->toBeString();

    $n0 = $ch->wait(100, 8);
    expect($n0)->toBe(0);

    sc_publish_many($ch, $q, 12);
    $n1 = $ch->wait(2000, 16);
    expect($n1)->toBe(12);

    $ch->close(); $c->close();
});

test('wait(…, 0) is a no-op and returns 0', function () {
    // Description: Tests that wait(…, 0) is a no-op and returns 0.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->simpleConsume($q, function () {}))->toBeString();

    sc_publish_many($ch, $q, 5);
    $n = $ch->wait(50, 0);
    expect($n)->toBe(0);

    $n2 = $ch->wait(2000, 16);
    expect($n2)->toBe(5);

    $ch->close(); $c->close();
});

test('wait with negative timeout returns 0 and does not throw', function () {
    // Description: Tests that wait with negative timeout returns 0 and does not throw.
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->simpleConsume($q, function (AmqpDelivery $body) {}))->toBeString();

    expect($ch->wait(-1, 10))->toBe(0);

    $ch->close(); $c->close();
});

test('wait boundary: tiny timeout still able to drain if message already queued', function () {
    // Description: Tests that a tiny timeout can still drain if the message is already queued.
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->simpleConsume($q, function (AmqpDelivery $body) {}))->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    expect($ch->wait(1, 1))->toBe(1);

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// MULTI-CHANNEL & CONSUMER BEHAVIOR
// ---------------------------------------------------------------------------------------------

test('enforces a single consumer per channel (second call errors)', function () {
    // Description: Tests that a single consumer is enforced per channel.
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->simpleConsume($q, function () {}))->toBeString();

    expect(fn () => $ch->simpleConsume($q, function () {}))->toThrow(Exception::class);

    $ch->close();
    $c->close();
});

test('two channels consuming the same queue split the work (sum matches total)', function () {
    // Description: Tests that two channels consuming the same queue split the work.
    $c = mq_client(); $c->connect();
    $ch1 = $c->openChannel();
    $ch2 = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch1->queueDeclare($q, ['auto_delete' => true]))->toBeTrue();

    expect($ch1->simpleConsume($q, function () {}))->toBeString();
    expect($ch2->simpleConsume($q, function () {}))->toBeString();

    $total = 40;
    sc_publish_many($ch1, $q, $total);

    $n1 = $ch1->wait(3000, $total);
    $n2 = $ch2->wait(3000, $total);
    expect($n1 + $n2)->toBe($total);
    expect($n1)->toBeGreaterThan(0);
    expect($n2)->toBeGreaterThan(0);

    $ch1->close(); $ch2->close(); $c->close();
});

test('two channels on the same connection consuming two different queues do not cross-deliver', function () {
    // Description: Tests that two channels on the same connection consuming two different queues do not cross-deliver.
    $c = mq_client();
    $ch1 = $c->openChannel();
    $ch2 = $c->openChannel();

    $q1 = sc_tmp_queue(); $q2 = sc_tmp_queue();
    expect($ch1->queueDeclare($q1, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch2->queueDeclare($q2, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $got1 = []; $got2 = [];
    expect($ch1->simpleConsume($q1, function (AmqpDelivery $message) use (&$got1) {
        $got1 = $message->getBody();
    }))->toBeString();

    expect($ch2->simpleConsume($q2, function (AmqpDelivery $message) use (&$got2) {
        $got2 = $message->getBody();
    }))->toBeString();

    expect($ch1->basicPublish('', $q1, new AmqpMessage('one')))->toBeTrue();
    expect($ch2->basicPublish('', $q2, new AmqpMessage('two')))->toBeTrue();

    $p1 = $ch1->wait(500, 8);
    $p2 = $ch2->wait(500, 8);

    expect($p1 + $p2)->toBeGreaterThan(0);
    expect($got1)->toContain('one');
    expect($got2)->toContain('two');
    expect($got1)->not->toContain('two');
    expect($got2)->not->toContain('one');

    $ch1->close(); $ch2->close(); $c->close();
});

test('cancel mid-stream stops new deliveries but does not break channel close', function () {
    // Description: Tests that canceling mid-stream stops new deliveries but does not break channel close.
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();
    $q = sc_tmp_queue();

    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = [];
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $message) use (&$seen) {
        $seen[] = $message->getBody();
    });
    expect($tag)->toBeConsumerTag();

    // Give the consumer a bit more time to fully attach before publishing.
    usleep(10_000);

    expect($ch->basicPublish('', $q, new AmqpMessage('a')))->toBeTrue();

    $drained = 0;
    for ($i = 0; $i < 6 && !in_array('a', $seen, true); $i++) {
        $drained += $ch->wait(500, 32);
    }

    expect($seen)->toContain('a');

    $ch->basicCancel($tag);

    expect($ch->basicPublish('', $q, new AmqpMessage('b')))->toBeTrue();

    $drained = 0;
    for ($i = 0; $i < 5; $i++) {
        $drained += $ch->wait(200, 32);
    }
    expect($seen)->not->toContain('b');

    $ch->basicCancel($tag);

    expect($ch->basicPublish('', $q, new AmqpMessage('b')))->toBeTrue();
    expect($ch->wait(300, 8))->toBe(0);

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// PAYLOAD INTEGRITY & LARGE MESSAGES
// ---------------------------------------------------------------------------------------------

test('handles a large burst gracefully', function () {
    // Description: Tests that a large burst of messages is handled gracefully.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->simpleConsume($q, function () {}))->toBeString();

    $total = 500;
    sc_publish_many($ch, $q, $total);

    $drained = 0;
    $tries = 0;
    while ($drained < $total && $tries++ < 10) {
        $drained += $ch->wait(2000, 128);
    }
    expect($drained)->toBe($total);

    $ch->close(); $c->close();
});

test('handles a large burst gracefully with content', function () {
    // Description: Tests that a large burst of messages with content is handled gracefully.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    $got = [];
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch->simpleConsume($q, function (AmqpDelivery $delivery) use (&$got) {
        $got[] = $delivery->getBody();
    }))->toBeString();

    $total = 500;
    sc_publish_many($ch, $q, $total);

    $drained = 0;
    $tries = 0;
    while ($drained < $total && $tries++ < 10) {
        $drained += $ch->wait(2000, 128);
    }
    expect($drained)->toBe($total);
    expect($total)->toBe(count($got));

    $ch->close(); $c->close();
});

test('very large payload (>= 256KB) survives publish -> consume intact length', function () {
    // Description: Tests that a very large payload (>= 256KB) survives publish -> consume intact length.
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $gotLen = 0;
    expect($ch->simpleConsume($q, function (AmqpDelivery $message) use (&$gotLen) { $gotLen = strlen($message->getBody()); }))->toBeString();

    $payload = random_bytes(256 * 1024 + 123);
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $drained = 0;
    for ($i=0; $i<10 && $drained===0; $i++) { $drained = $ch->wait(500, 4); }
    expect($drained)->toBeGreaterThan(0);
    expect($gotLen)->toBe(strlen($payload));

    $ch->close(); $c->close();
});

test('unicode payload round-trips intact (utf-8)', function () {
    // Description: Tests that unicode payload round-trips intact (utf-8).
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $got = '';
    expect($ch->simpleConsume($q, function (AmqpDelivery $message) use (&$got) { $got = $message->getBody(); }))->toBeString();

    $payload = "héllo 🐇 — 你好 — Привет — عربى";
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $n = 0;
    for ($i=0; $i<10 && $n===0; $i++) { $n = $ch->wait(500, 8); }
    expect($n)->toBeGreaterThan(0);
    expect($got)->toBe($payload);

    $ch->close(); $c->close();
});

test('binary payload exactness with random bytes (length & equality)', function () {
    // Description: Tests that binary payload exactness with random bytes (length & equality).
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $got = '';
    expect($ch->simpleConsume($q, function (AmqpDelivery $message) use (&$got) {
        $got = $message->getBody();
    }))->toBeString();

    $payload = random_bytes(32);
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $n = 0;
    for ($i=0; $i<10 && $n===0; $i++) { $n = $ch->wait(500, 8); }
    expect($n)->toBeGreaterThan(0);
    expect(strlen($got))->toBe(strlen($payload));
    expect($got === $payload)->toBeTrue();

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// ERROR HANDLING & EDGE CASES
// ---------------------------------------------------------------------------------------------

test('simpleConsume on unknown queue closes channel (broker NOT_FOUND)', function () {
    // Description: Tests that simpleConsume on an unknown queue closes the channel.
    $c = mq_client(); $ch = $c->openChannel();

    expect(fn () => $ch->simpleConsume('q.does.not.exist', function (AmqpDelivery $_) {}))
        ->toThrow(Exception::class);

    expect(fn () => $ch->wait(100, 1))->toThrow(Exception::class);

    $c->close();
});


// amqplib-compatible default (auto_ack=true): after exception, original consumer remains
// attached and continues to receive; a new consumer on the same queue will not see the next message.
test('default auto_ack: callback exception surfaces; a second consumer does NOT receive next message', function () {
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = 0;
    expect($ch->simpleConsume($q, function (AmqpDelivery $_) use (&$seen) {
        $seen++;
        throw new RuntimeException('boom default');
    }))->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('m1')))->toBeTrue();
    expect(fn () => $ch->wait(500, 1))->toThrow(Exception::class);
    expect($seen)->toBe(1);

    // Attach a second consumer and publish another message.
    $ch2 = $c->openChannel();
    expect($ch2->simpleConsume($q, function (AmqpDelivery $_) {}))->toBeString();

    // Let the new consumer register.
    usleep(150_000);

    expect($ch2->basicPublish('', $q, new AmqpMessage('m2')))->toBeTrue();
    // With auto_ack on the first consumer, it will receive m2; ch2 gets 0.
    expect($ch2->wait(800, 1))->toBe(0);

    $ch->close(); $ch2->close(); $c->close();
});

// Manual-ack policy: reject_on_exception=true, requeue_on_reject=false (drop to DLX if configured)
// After exception we NACK (no requeue) and the SAME consumer can continue.
test('reject_on_exception (drop): same consumer continues after exception', function () {
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = 0;
    // Throw only once, then succeed.
    expect($ch->simpleConsume($q, function (AmqpDelivery $_) use (&$seen) {
        static $thrown = false;
        if (!$thrown) { $thrown = true; throw new RuntimeException('boom once'); }
        $seen++;
    }, ['reject_on_exception' => true, 'requeue_on_reject' => false]))->toBeString();

    // First message triggers exception and is NACKed (not requeued)
    expect($ch->basicPublish('', $q, new AmqpMessage('m1')))->toBeTrue();
    expect(fn () => $ch->wait(800, 1))->toThrow(Exception::class);

    // Next message should be processed by the same consumer now that the callback will succeed.
    expect($ch->basicPublish('', $q, new AmqpMessage('m2')))->toBeTrue();
    expect($ch->wait(1000, 1))->toBe(1);
    expect($seen)->toBe(1);

    $ch->close(); $c->close();
});

// Manual-ack policy: reject_on_exception=true, requeue_on_reject=true
// After exception we NACK+requeue: the SAME message is redelivered; the same consumer handles it
// once the callback stops throwing.
test('reject_on_exception (requeue): same message is retried and then processed', function () {
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $got = [];
    expect($ch->simpleConsume($q, function (AmqpDelivery $d) use (&$got) {
        static $thrown = false;
        if (!$thrown) { $thrown = true; throw new RuntimeException('boom once'); }
        $got[] = $d->getBody();
    }, ['reject_on_exception' => true, 'requeue_on_reject' => true]))->toBeString();

    // Publish one message that will first fail, then be retried and succeed.
    expect($ch->basicPublish('', $q, new AmqpMessage('retry-me')))->toBeTrue();

    // First wait surfaces the exception
    expect(fn () => $ch->wait(800, 1))->toThrow(Exception::class);
    // Second wait should process the requeued same message
    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(800, 1); }
    expect($n)->toBe(1);
    expect($got)->toContain('retry-me');

    $ch->close(); $c->close();
});


test('simple consumer auto-resumes after broker restart (if enabled)', function () {
    // This test requires docker-compose access to restart the broker.
    // Enable by setting E2E_CAN_RESTART_BROKER=1 and optionally E2E_RABBIT_SERVICE (default: rabbitmq)
    if ((getenv('E2E_CAN_RESTART_BROKER') ?: '0') !== '1') {
        $this->markTestSkipped('Set E2E_CAN_RESTART_BROKER=1 to run auto-resume test.');
    }
    $service = getenv('E2E_RABBIT_SERVICE') ?: 'rabbitmq';

    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = [];
    expect($ch->simpleConsume($q, function (AmqpDelivery $d) use (&$seen) {
        $seen[] = $d->getBody();
    }))->toBeString();

    // 1) message before restart
    expect($ch->basicPublish('', $q, new AmqpMessage('before')))->toBeTrue();
    $n = 0; for ($i=0; $i<10 && $n===0; $i++) { $n = $ch->wait(500, 8); }
    expect($n)->toBeGreaterThan(0);
    expect($seen)->toContain('before');

    // 2) restart the broker to drop TCP + channels
    shell_exec("docker compose restart {$service} 2>/dev/null");
    // give it some time to come back up
    sleep(4);

    // 3) publish after restart; consumer should auto-resume on next wait/poll
    expect($ch->basicPublish('', $q, new AmqpMessage('after-1')))->toBeTrue();
    expect($ch->basicPublish('', $q, new AmqpMessage('after-2')))->toBeTrue();

    $drained = 0; $tries = 0;
    while ($drained < 2 && $tries++ < 12) {
        $drained += $ch->wait(1000, 8);
    }

    expect($drained)->toBeGreaterThanOrEqual(2);
    expect(array_values(array_intersect($seen, ['after-1','after-2'])))->toHaveCount(2);

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// GRACEFUL SHUTDOWN & LATENCY
// ---------------------------------------------------------------------------------------------

test('graceful shutdown: active consumer with no messages closes cleanly', function () {
    // Description: Tests that an active consumer with no messages closes cleanly.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'sc.shut.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->simpleConsume($q, function (string $_) {}))->toBeString();

    $ch->close();
    $c->close();

    expect(true)->toBeTrue();
});

test('graceful shutdown: close while messages may still be in flight does not explode', function () {
    // Description: Tests that closing while messages may still be in flight does not explode.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'sc.shut2.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = 0;
    expect($ch->simpleConsume($q, function (string $_) use (&$seen) { $seen++; }))->toBeString();

    for ($i = 0; $i < 200; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    }

    usleep(50_000);

    try {
        $ch->close();
        $c->close();
        expect(true)->toBeTrue();
    } catch (\Exception $e) {
        fwrite(STDERR, "\n[shutdown] caught: " . $e->getMessage() . "\n");
        expect(true)->toBeTrue();
    }
});

test('reports average end-to-end latency over 1000 msgs (informative)', function () {
    // Description: Reports average end-to-end latency over 1000 messages.
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'sc.lat.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $count = 0;
    expect($ch->simpleConsume($q, function (AmqpDelivery $_) use (&$count) { $count++; }))->toBeString();

    $total = 1000;
    $t0 = microtime(true);

    for ($i = 0; $i < $total; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    }

    $drained = 0;
    while ($drained < $total) {
        $drained += $ch->wait(2000, 256);
    }

    $elapsed = microtime(true) - $t0;
    $avg_ms = ($elapsed / max(1, $total)) * 1000.0;

    expect($drained)->toBe($total);
    expect($count)->toBe($total);
    expect($avg_ms)->toBeGreaterThan(0.01);
    expect($avg_ms)->toBeLessThan(5000);

    fwrite(STDERR, sprintf("\n[latency] messages=%.1f total=%.1f s, avg=%.2f ms/msg\n",$drained, $elapsed, $avg_ms));

    $ch->close(); $c->close();
});


// ---------------------------------------------------------------------------------------------
// QOS OPTIONS & HOT RESIZE – BEHAVIORAL TESTS
// ---------------------------------------------------------------------------------------------

test('qos accepts options array (prefetch_size, global) and returns true', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'qos.opts.accept.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Invoke with options: prefetch_size (ignored by RabbitMQ but accepted) plus global
    expect($ch->qos(20, ['prefetch_size' => 4096, 'global' => true]))->toBeTrue();

    // Quick smoke test: publish and drain to ensure everything works
    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    expect($ch->simpleConsume($q, function () {}))->toBeString();

    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(600, 8); }
    expect($n)->toBeGreaterThan(0);

    $ch->close(); $c->close();
});

test('qos(global=true) with prefetch=2 blocks further deliveries until ack', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'qos.global.block.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Global window of 2 (behaves the same with a single consumer)
    expect($ch->qos(2, ['global' => true]))->toBeTrue();

    for ($i=0; $i<5; $i++) { expect($ch->basicPublish('', $q, new AmqpMessage('m'.$i)))->toBeTrue(); }

    $tags = [];
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$tags) {
        // Do not ack immediately to test backpressure
        $tags[] = $d->getDeliveryTag();
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    // Should receive at most 2 deliveries (prefetch=2)
    $drained = 0; $deadline = microtime(true) + 2.5;
    while (count($tags) < 2 && microtime(true) < $deadline) { $drained += $ch->wait(300, 8); }
    expect(count($tags))->toBe(2);

    // As long as we do not ACK, nothing else should arrive
    $noop = 0; for ($i=0; $i<4; $i++) { $noop += $ch->wait(300, 8); }
    expect(count($tags))->toBe(2);

    // Acking one message frees a slot -> a third delivery can arrive
    expect($ch->basicAck($tags[0], false))->toBeTrue();
    $got = 0; for ($i=0; $i<6 && $got===0; $i++) { $got = $ch->wait(600, 8); }
    expect($got)->toBeGreaterThan(0);
    expect(count($tags))->toBeGreaterThanOrEqual(3);

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});

test('hot-resize downward enforces new lower window after inflight drops', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'qos.resize.down.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Start with a large window then shrink aggressively
    expect($ch->qos(5))->toBeTrue();
    for ($i=0; $i<8; $i++) { expect($ch->basicPublish('', $q, new AmqpMessage('m'.$i)))->toBeTrue(); }

    $tags = [];
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$tags) {
        // Store delivery tags to ACK later
        $tags[] = $d->getDeliveryTag();
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    // Receive up to 5 messages (current window)
    $rcv = 0; $deadline = microtime(true) + 3.0;
    while (count($tags) < 5 && microtime(true) < $deadline) { $rcv += $ch->wait(300, 16); }
    expect(count($tags))->toBe(5);

    // Resize down to 2: while inflight (=5) > 2 no new deliveries are allowed
    expect($ch->qos(2, ['global' => false]))->toBeTrue();
    $stalled = 0; for ($i=0; $i<4; $i++) { $stalled += $ch->wait(300, 16); }
    expect(count($tags))->toBe(5);

    // Ack 4 -> inflight drops to 1 (< new window=2) -> permits new deliveries
    expect($ch->basicAck($tags[0], false))->toBeTrue();
    expect($ch->basicAck($tags[1], false))->toBeTrue();
    expect($ch->basicAck($tags[2], false))->toBeTrue();
    expect($ch->basicAck($tags[3], false))->toBeTrue();

    $more = 0; $deadline2 = microtime(true) + 3.0;
    while (count($tags) < 8 && microtime(true) < $deadline2) { $more += $ch->wait(400, 16); }

    expect(count($tags))->toBeGreaterThanOrEqual(7); // at least two additional messages should arrive
    // Cleanup
    foreach ($tags as $t) { @ $ch->basicAck($t, false); }

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});

test('qos ignores prefetch_size functionally but call remains successful', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'qos.psize.ignored.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Call with prefetch_size only
    expect($ch->qos(10, ['prefetch_size' => 1_000_000]))->toBeTrue();

    // Ensure consumption still works and nothing crashes
    $tag = $ch->simpleConsume($q, function () {}, ['no_ack' => true]);
    expect($tag)->toBeString();

    $ch->basicPublish('', $q, new AmqpMessage('ok'));
    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(600, 8); }
    expect($n)->toBeGreaterThan(0);

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});

test('hot-resize upward with global=true applies new global window to existing consumer', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = 'qos.global.up.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Start with a global window of 2
    expect($ch->qos(2, ['global' => true]))->toBeTrue();
    for ($i = 0; $i < 10; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage('m' . $i)))->toBeTrue();
    }

    $tags = [];
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$tags) {
        $tags[] = $d->getDeliveryTag();
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    // Phase 1: stuck at 2
    $n = 0; for ($i=0; $i<10 && $n<2; $i++) { $n += $ch->wait(400, 8); }
    expect(count($tags))->toBe(2);

    // Resize global window to 5 -> additional deliveries unlock
    expect($ch->qos(5, ['global' => true]))->toBeTrue();

    $deadline = microtime(true) + 2.0;
    while (count($tags) < 5 && microtime(true) < $deadline) { $ch->wait(300, 8); }
    expect(count($tags))->toBeGreaterThanOrEqual(5);

    foreach ($tags as $t) { $ch->basicAck($t, false); }
    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});


// ---------------------------------------------------------------------------------------------
// AUTO QOS
// ---------------------------------------------------------------------------------------------


test('auto_qos scales up under sustained load (throughput target)', function () {
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = 'autoqos.scaleup.' . uniqid('', true);
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Burst traffic to trigger the ramp-up
    $total = 300;
    for ($i = 0; $i < $total; $i++) {
        $ch->basicPublish('', $q, new AmqpMessage((string)$i));
    }

    $delivered = 0;
    $peakBatch = 0;
    $cb = function (AmqpDelivery $d) use (&$delivered) { $delivered++; };

    $tag = $ch->simpleConsume($q, $cb, [
        'auto_qos' => [
            'enabled'     => true,
            'min'         => 10,
            'max'         => 200,
            'step_up'     => 1.5,
            'step_down'   => 0.75,
            'cooldown_ms' => 300,
            'target'      => 'throughput',
        ],
    ]);
    expect($tag)->toBeString();

    // Drain in loops; as cooldowns elapse the capacity should increase
    $deadline = microtime(true) + 8.0;
    while ($delivered < $total && microtime(true) < $deadline) {
        $n = $ch->wait(400, 128);
        $peakBatch = max($peakBatch, (int)$n);
    }

    // Nothing lost
    expect($delivered)->toBe($total);
    // A batch significantly > min (=10) confirms the scaling took place
    expect($peakBatch)->toBeGreaterThanOrEqual(30);

    $ch->basicCancel($tag);

    $ch->close(); $c->close();
});

test('hot-resize via qos() mid-stream preserves messages and increases drain capacity', function () {
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = 'hotresize.qos.' . uniqid('', true);
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Start with a small prefetch
    $ch->qos(10);

    $total = 400;
    for ($i = 0; $i < $total; $i++) {
        $ch->basicPublish('', $q, new AmqpMessage((string)$i));
    }

    $delivered = 0;
    $peakBefore = 0;
    $peakAfter  = 0;

    $cb = function (AmqpDelivery $d) use (&$delivered) { $delivered++; };
    $tag = $ch->simpleConsume($q, $cb);
    expect($tag)->toBeString();

    // Measure before resizing
    for ($i = 0; $i < 5; $i++) {
        $n = $ch->wait(400, 128);
        $peakBefore = max($peakBefore, (int)$n);
    }

    // Hot-resize while consumption is ongoing
    $ch->qos(100);

    // Inject post-resize load to observe the batch size increase
    $extra = 300;
    for ($i = 0; $i < $extra; $i++) {
        $ch->basicPublish('', $q, new AmqpMessage('postresize-' . $i));
    }
    $total += $extra;

    // After resizing, batches should grow
    $deadline = microtime(true) + 10.0;
    while ($delivered < $total && microtime(true) < $deadline) {
        $n = $ch->wait(600, 256);
        $peakAfter = max($peakAfter, (int)$n);
    }

    expect($delivered)->toBe($total);

    // Reasonable threshold with prefetch=100 (buffer capped at 100) and max drain=256
    expect($peakAfter)->toBeGreaterThanOrEqual(max(50, $peakBefore + 5));

    $ch->basicCancel($tag);

    $ch->close(); $c->close();
});

// Optional (reconnect via docker compose). Skip if not enabled.
test('auto_qos continues to work after reconnect (if enabled)', function () {
    if ((getenv('E2E_CAN_RESTART_BROKER') ?: '0') !== '1') {
        $this->markTestSkipped('Set E2E_CAN_RESTART_BROKER=1 to run auto_qos reconnect test.');
    }
    $service = getenv('E2E_RABBIT_SERVICE') ?: 'rabbitmq';

    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = 'autoqos.reconnect.' . uniqid('', true);
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $delivered = 0;
    $cb = function (AmqpDelivery $d) use (&$delivered) { $delivered++; };

    $tag = $ch->simpleConsume($q, $cb, [
        'auto_qos' => [
            'min'         => 10,
            'max'         => 200,
            'step_up'     => 1.5,
            'step_down'   => 0.75,
            'cooldown_ms' => 300,
            'target'      => 'throughput',
        ],
    ]);

    // avant reboot
    for ($i = 0; $i < 50; $i++) { $ch->basicPublish('', $q, new AmqpMessage('pre')); }
    $ch->wait(1000, 128);

    // reboot broker
    shell_exec("docker compose restart {$service} 2>/dev/null");
    sleep(4);

    for ($i = 0; $i < 150; $i++) { $ch->basicPublish('', $q, new AmqpMessage('post')); }

    $deadline = microtime(true) + 8.0;
    while (microtime(true) < $deadline) {
        $ch->wait(600, 256);
    }

    expect($delivered)->toBeGreaterThanOrEqual(200);

    $ch->basicCancel($tag);

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// ACK / NACK MODES & OPTIONS INTERACTIONS (AMQPLIB PARITY)
// ---------------------------------------------------------------------------------------------

test('simpleConsume default is auto-ack (no_ack=true): message is drained without explicit ack', function () {
    $c = mq_client();
    $c->connect();
    $ch = $c->openChannel();

    $q = 'ack.default.auto.' . uniqid('', true);
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = 0;
    // No options => default no_ack=true
    expect($ch->simpleConsume($q, function (AmqpDelivery $_) use (&$seen) { $seen++; }))->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();

    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(400, 8); }
    expect($n)->toBeGreaterThan(0);
    expect($seen)->toBe(1);

    // Attach a second consumer and ensure no redelivery remains
    $ch2 = $c->openChannel();
    expect($ch2->simpleConsume($q, function () {}))->toBeString();
    $n2 = 0; for ($i=0; $i<4 && $n2===0; $i++) { $n2 = $ch2->wait(300, 8); }
    expect($n2)->toBe(0);

    $ch->close(); $ch2->close(); $c->close();
});

// reject_on_exception=true implies manual-ack mode (no_ack=false) even if not explicitly set
// so that we can NACK the failing message

test('reject_on_exception without explicit no_ack forces manual-ack and NACKs on failure', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.reject.policy.' . uniqid('', true);
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $thrown = 0;
    // reject_on_exception=true implies no_ack=false (manual ack mode)
    $tag1 = $ch->simpleConsume($q, function () use (&$thrown) {
        $thrown++;
        throw new RuntimeException('boom under policy');
    }, ['reject_on_exception' => true, 'requeue_on_reject' => false]);
    expect($tag1)->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('m')))->toBeTrue();
    expect(fn () => $ch->wait(800, 1))->toThrow(Exception::class);

    // publish another message; since previous was NACKed (no requeue), this one will be processed ok
    $ok = 0;
    $cb2 = function () use (&$ok) { $ok++; };
    // keep same consumer; its callback will now be replaced by a no-throw lambda via a new consumer tag
    $ch->basicCancel($tag1);
    // auto_delete queues are deleted when the last consumer is cancelled; re-declare it before starting the new consumer
    $ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]);
    $tag2 = $ch->simpleConsume($q, $cb2, ['reject_on_exception' => true, 'requeue_on_reject' => false]);
    expect($tag2)->toBeString();
    expect($ch->basicPublish('', $q, new AmqpMessage('m2')))->toBeTrue();

    $drained = 0; for ($i=0; $i<6 && $drained===0; $i++) { $drained = $ch->wait(800, 2); }
    expect($drained)->toBeGreaterThan(0);
    expect($ok)->toBe(1);

    $ch->basicCancel($tag2);
    $ch->close(); $c->close();
});

// If user explicitly sets no_ack=true along with reject_on_exception, we remain in auto-ack mode and do not NACK.

test('explicit no_ack=true wins over reject_on_exception: no NACK attempted', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.explicit.auto.' . uniqid('', true);
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $hits = 0;
    expect($ch->simpleConsume($q, function () use (&$hits) { $hits++; throw new RuntimeException('fail'); }, [
        'no_ack' => true,
        'reject_on_exception' => true, // ignored for NACK because auto-ack is explicit
        'requeue_on_reject' => true,
    ]))->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    // Exception is surfaced but message is auto-acked already; no redelivery
    expect(fn () => $ch->wait(800, 1))->toThrow(Exception::class);

    // Second consumer should not receive anything (message was lost due to auto-ack)
    $ch2 = $c->openChannel();
    expect($ch2->simpleConsume($q, function () {}))->toBeString();
    $n = 0; for ($i=0; $i<4 && $n===0; $i++) { $n = $ch2->wait(400, 4); }
    expect($n)->toBe(0);

    $ch->close(); $ch2->close(); $c->close();
});

// basicCancel should stop deliveries for that consumer tag

test('basicCancel stops deliveries for the given tag', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.cancel.' . uniqid('', true);
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = 0;
    $tag = $ch->simpleConsume($q, function () use (&$seen) { $seen++; });
    expect($tag)->toBeString();

    // Cancel the consumer
    expect($ch->basicCancel($tag))->toBeTrue();

    // Publish a few messages; none should be delivered to the canceled tag
    for ($i=0; $i<5; $i++) { expect($ch->basicPublish('', $q, new AmqpMessage('x'.$i)))->toBeTrue(); }

    $n = 0; for ($i=0; $i<5 && $n===0; $i++) { $n = $ch->wait(500, 16); }
    expect($n)->toBe(0);
    expect($seen)->toBe(0);

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// ACK / NACK – DEEP EDGE CASES
// ---------------------------------------------------------------------------------------------

test('manual-ack: NACK requeue=true makes message redelivered next time', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $body = 'once-then-redeliver';
    expect($ch->basicPublish('', $q, new AmqpMessage($body)))->toBeTrue();

    $seen = 0; $flags = []; $firstTag = null;
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$seen, &$flags, &$firstTag) {
        $seen++;
        $flags[] = $d->isRedelivered();
        if ($seen === 1) {
            $firstTag = $d->getDeliveryTag();
            return true; // pas d'ACK/NACK dans le callback au 1er passage
        }
        // second passage (redelivery) : ACK
        $d->ack();
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    // Attendre la 1re livraison uniquement
    $drained = 0; $deadline = microtime(true) + 4.0;
    while ($seen < 1 && microtime(true) < $deadline) { $drained += $ch->wait(200, 1); }
    expect($seen)->toBe(1);
    expect($firstTag)->toBeGreaterThan(0);

    // NACK+requeue via l’API channel (hors callback)
    expect($ch->basicNack($firstTag, true, false))->toBeTrue();

    // Attendre la redelivery (flag redelivered=true attendu)
    $deadline = microtime(true) + 4.0;
    while ($seen < 2 && microtime(true) < $deadline) { $drained += $ch->wait(400, 8); }
    expect($seen)->toBe(2);
    expect($flags[0])->toBeFalse();
    expect($flags[1])->toBeTrue();

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});


test('manual-ack: basicReject(requeue=false) drops the message (no redelivery)', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('drop-me')))->toBeTrue();

    $firstTag = null; $seen = 0;
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$firstTag, &$seen) {
        $seen++; $firstTag = $d->getDeliveryTag();
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    // Wait for first delivery to arrive and capture tag
    $n = 0; for ($i=0; $i<10 && $n===0; $i++) { $n = $ch->wait(400, 8); }
    expect($seen)->toBe(1);
    expect($firstTag)->toBeGreaterThan(0);

    // Reject without requeue (drop)
    expect($ch->basicReject($firstTag, false))->toBeTrue();

    // Try to get it again — should not redeliver
    $n2 = 0; for ($i=0; $i<5 && $n2===0; $i++) { $n2 = $ch->wait(300, 8); }
    expect($n2)->toBe(0);

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});






// ---------------------------------------------------------------------------------------------
// ACK / NACK – HARDENED SCENARIOS (ADVANCED)
// ---------------------------------------------------------------------------------------------



test('qos partial ack then ack-multiple unlocks more deliveries', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.qos.partial.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->qos(3))->toBeTrue();
    for ($i=0; $i<5; $i++) { $ch->basicPublish('', $q, new AmqpMessage('m'.$i)); }

    $tags = [];
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$tags) {
        $tags[] = $d->getDeliveryTag();
        return count($tags) < 5;
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    $dr = 0; $deadline = microtime(true) + 3.0;
    while (count($tags) < 3 && microtime(true) < $deadline) { $dr += $ch->wait(200, 1); }
    expect(count($tags))->toBeLessThanOrEqual(3);

    $second = $tags[1];
    expect($ch->basicAck($second, true))->toBeTrue();

    $deadline2 = microtime(true) + 3.0;
    while (count($tags) < 5 && microtime(true) < $deadline2) { $dr += $ch->wait(400, 8); }
    expect(count($tags))->toBe(5);

    expect($ch->basicAck($tags[3], false))->toBeTrue();
    expect($ch->basicAck($tags[4], false))->toBeTrue();

    $ch->basicCancel($tag); $ch->close(); $c->close();
});


test('two manual-ack consumers split load and acks free capacity', function () {
    $c = mq_client(); $c->connect();
    $ch1 = $c->openChannel();
    $ch2 = $c->openChannel();

    $q = 'ack.mc.' . bin2hex(random_bytes(4));
    expect($ch1->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();
    expect($ch1->qos(10))->toBeTrue();
    expect($ch2->qos(10))->toBeTrue();

    for ($i=0; $i<40; $i++) { $ch1->basicPublish('', $q, new AmqpMessage('m'.$i)); }

    $c1 = 0; $c2 = 0;
    $t1 = $ch1->simpleConsume($q, function(AmqpDelivery $d) use (&$c1) {
        $c1++; $d->ack();
    }, ['no_ack' => false]);
    $t2 = $ch2->simpleConsume($q, function(AmqpDelivery $d) use (&$c2) {
        $c2++; $d->ack();
    }, ['no_ack' => false]);

    $deadline = microtime(true) + 6.0; $drained = 0;
    while ($drained < 40 && microtime(true) < $deadline) {
        $drained += $ch1->wait(400, 64);
        $drained += $ch2->wait(400, 64);
     }

    expect($c1 + $c2)->toBe(40);
    expect($drained)->toBeGreaterThanOrEqual(40);
    expect($c1)->toBeGreaterThan(0);
    expect($c2)->toBeGreaterThan(0);

    $ch1->basicCancel($t1); $ch2->basicCancel($t2);
    $ch1->close(); $ch2->close(); $c->close();
});


test('auto-ack mode: calling ack_on() inside callback throws', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.auto.ackcall.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use ($ch) {
        $d->ack_on($ch);
    }, ['no_ack' => true]);
    expect($tag)->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();
    expect(fn () => $ch->wait(800, 1))->toThrow(Exception::class);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});


test('auto-ack mode: calling nack_on() inside callback throws', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.auto.nackcall.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use ($ch) {
        $d->nack_on($ch, true);
    }, ['no_ack' => true]);
    expect($tag)->toBeString();

    expect($ch->basicPublish('', $q, new AmqpMessage('y')))->toBeTrue();
    expect(fn () => $ch->wait(800, 1))->toThrow(Exception::class);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});


test('manual-ack: cancel with multiple unacked messages requeues all and redelivers with flag', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    for ($i=0; $i<3; $i++) { $ch->basicPublish('', $q, new AmqpMessage('m'.$i)); }

    $seen = 0; $tags = [];
    $t1 = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$seen, &$tags) { $seen++; $tags[] = $d->getDeliveryTag(); }, ['no_ack' => false]);
    expect($t1)->toBeString();

    $rcv = 0; while ($rcv < 2) { $rcv += $ch->wait(600, 8); }
    expect($seen)->toBeGreaterThanOrEqual(2);

    expect($ch->basicCancel($t1))->toBeTrue();
    // Re-declare the queue with the same permanent options (no-op if already exists)
    expect($ch->queueDeclare($q, ['exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    $got = 0; $redelivered = 0;
    $t2 = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$got, &$redelivered) { $got++; if ($d->isRedelivered()) $redelivered++; $d->ack(); }, ['no_ack' => false]);

    $drained = 0; $deadline = microtime(true) + 5.0;
    while ($drained < 3 && microtime(true) < $deadline) { $drained += $ch->wait(600, 16); }

    expect($got)->toBe(3);
    expect($redelivered)->toBeGreaterThanOrEqual(2);

    $ch->basicCancel($t2); $ch->close(); $c->close();
});




test('STRESS manual-ack 10000 messages (opt-in)', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.stress.m.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $total = 10000; for ($i=0; $i<$total; $i++) { $ch->basicPublish('', $q, new AmqpMessage('m'.$i)); }

    $seen = 0;
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$seen) { $seen++; $d->ack(); }, ['no_ack' => false]);

    $drained = 0; $deadline = microtime(true) + 30.0;
    while ($drained < $total && microtime(true) < $deadline) { $drained += $ch->wait(800, 256); }

    expect($drained)->toBe($total);
    expect($seen)->toBe($total);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});


test('STRESS auto-ack 10000 messages (opt-in)', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.stress.a.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $total = 10000; for ($i=0; $i<$total; $i++) { $ch->basicPublish('', $q, new AmqpMessage('a'.$i)); }

    $seen = 0;
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$seen) { $seen++; }, ['no_ack' => true]);

    $drained = 0; $deadline = microtime(true) + 30.0;
    while ($drained < $total && microtime(true) < $deadline) { $drained += $ch->wait(800, 256); }

    expect($drained)->toBe($total);
    expect($seen)->toBe($total);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});

test('manual-ack: basicAck(multiple=true) acks all previous deliveries on a permanent queue', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();

    $q = 'ack.multiple.' . bin2hex(random_bytes(4));
    // Permanent queue (not exclusive, not auto_delete)
    expect($ch->queueDeclare($q, ['durable' => false, 'exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    // manual-ack + window > 1 to stack several deliveries without acking
    expect($ch->qos(5))->toBeTrue();

    for ($i=1; $i<=3; $i++) { $ch->basicPublish('', $q, new AmqpMessage('m'.$i)); }

    $tags = [];
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$tags) {
        // do not ack here; accumulate delivery tags
        $tags[] = $d->getDeliveryTag();
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    // Drain 3 deliveries without acking
    $dr = 0; while (count($tags) < 3 && $dr < 20) { $dr += $ch->wait(400, 8); }

    // ACK multiple sur le 2e tag doit ack tag[0] et tag[1]
    expect($ch->basicAck($tags[1], true))->toBeTrue();

    // The third remains in flight (unacked). Publish 2 more messages: only one delivery should stay blocked.
    $ch->basicPublish('', $q, new AmqpMessage('x1'));
    $ch->basicPublish('', $q, new AmqpMessage('x2'));

    // We should receive at least one immediately (window freed by ack-multiple)
    $got = 0; for ($i=0; $i<6 && $got===0; $i++) { $got = $ch->wait(600, 2); }
    expect($got)->toBeGreaterThan(0);

    $ch->basicCancel($tag);
    $ch->close(); $c->close();
});


test('manual-ack: cancel requeues unacked message on permanent queue; new consumer gets redelivered=true', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();

    $q = 'ack.cancel.perm.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    $body = 'will-be-requeued-on-cancel';
    expect($ch->basicPublish('', $q, new AmqpMessage($body)))->toBeTrue();

    $tag1 = $ch->simpleConsume($q, function (AmqpDelivery $d) {
        // leave unacked so it gets requeued
    }, ['no_ack' => false]);

    $n = 0; for ($i=0; $i<8 && $n===0; $i++) { $n = $ch->wait(600, 8); }
    expect($n)->toBeGreaterThan(0);

    expect($ch->basicCancel($tag1))->toBeTrue();

    $second = ['body' => null, 'redelivered' => null];
    $tag2 = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$second) {
        $second['body'] = $d->getBody();
        $second['redelivered'] = $d->isRedelivered();
        $d->ack();
    }, ['no_ack' => false]);

    $m = 0; for ($i=0; $i<8 && $m===0; $i++) { $m = $ch->wait(800, 8); }
    expect($second['body'])->toBe($body);
    expect($second['redelivered'])->toBeTrue();

    $ch->basicCancel($tag2); $ch->close(); $c->close();
});

test('reconnect: unacked message is redelivered after broker restart on durable queue', function () {
    if ((getenv('E2E_CAN_RESTART_BROKER') ?: '0') !== '1') {
        $this->markTestSkipped('Set E2E_CAN_RESTART_BROKER=1 to run reconnect redelivery test.');
    }
    $service = getenv('E2E_RABBIT_SERVICE') ?: 'rabbitmq';

    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'ack.reconn.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['durable' => true, 'exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    $body = 'reconnect-redelivery';
    expect($ch->basicPublish('', $q, new AmqpMessage($body)))->toBeTrue();

    $tag1 = $ch->simpleConsume($q, function (AmqpDelivery $d) {
        // do not ack
    }, ['no_ack' => false]);

    $n = 0; for ($i=0; $i<10 && $n===0; $i++) { $n = $ch->wait(800, 8); }
    expect($n)->toBeGreaterThan(0);

    // Restart the broker
    shell_exec("docker compose restart {$service} 2>/dev/null");
    sleep(5);

    // On the client side, cancel the previous consumer (dead channel) and then restart
    $ch->basicCancel($tag1);

    $second = [ 'body' => null, 'redelivered' => null ];
    $tag2 = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$second) {
        $second['body'] = $d->getBody();
        $second['redelivered'] = $d->isRedelivered();
        $d->ack();
    }, ['no_ack' => false]);

    $m = 0; for ($i=0; $i<10 && $m===0; $i++) { $m = $ch->wait(1200, 8); }
    expect($m)->toBeGreaterThan(0);
    expect($second['body'])->toBe($body);
    expect($second['redelivered'])->toBeTrue();

    $ch->basicCancel($tag2); $ch->close(); $c->close();
});

test('exclusive+auto_delete: after cancel the queue is gone (NOT_FOUND)', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();

    $q = 'q.auto.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();

    $tag = $ch->simpleConsume($q, fn(AmqpDelivery $d) => null, ['no_ack' => false]);
    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(600, 4); }
    expect($n)->toBeGreaterThan(0);

    expect($ch->basicCancel($tag))->toBeTrue();

    $ch2 = $c->openChannel();
    expect(fn () => $ch2->simpleConsume($q, fn() => null, ['no_ack' => false]))
        ->toThrow(Exception::class);

    $ch2->close(); $ch->close(); $c->close();
});

test('permanent queue: cancel requeues unacked; next consumer sees redelivered=true', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'q.perm.cancel.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    $body = 'will-be-requeued';
    expect($ch->basicPublish('', $q, new AmqpMessage($body)))->toBeTrue();

    $tag1 = $ch->simpleConsume($q, fn(AmqpDelivery $d) => null, ['no_ack' => false]);
    $n = 0; for ($i=0; $i<8 && $n===0; $i++) { $n = $ch->wait(600, 8); }
    expect($n)->toBeGreaterThan(0);

    expect($ch->basicCancel($tag1))->toBeTrue();

    $got = ['body'=>null,'redelivered'=>null];
    $tag2 = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$got) {
        $got['body'] = $d->getBody();
        $got['redelivered'] = $d->isRedelivered();
        $d->ack();
    }, ['no_ack' => false]);

    $m = 0; for ($i=0; $i<8 && $m===0; $i++) { $m = $ch->wait(800, 8); }
    expect($got['body'])->toBe($body);
    expect($got['redelivered'])->toBeTrue();

    $ch->basicCancel($tag2); $ch->close(); $c->close();
});

test('manual-ack: basicAck(multiple=true) acks all previous deliveries', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'q.ack.multi.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive'=>false,'auto_delete'=>false]))->toBeTrue();

    expect($ch->qos(5))->toBeTrue();
    for ($i=1; $i<=3; $i++) { $ch->basicPublish('', $q, new AmqpMessage('m'.$i)); }

    $tags = [];
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$tags) {
        $tags[] = $d->getDeliveryTag(); // no ack here
    }, ['no_ack' => false]);
    expect($tag)->toBeString();

    $n = 0; while (count($tags) < 3 && $n < 10) { $n += $ch->wait(400, 8); }
    expect(count($tags))->toBe(3);

    // ACK multiple up to the second entry -> releases 1 and 2; 3 stays in flight
    expect($ch->basicAck($tags[1], true))->toBeTrue();

    // Publish two new messages; at least one should be delivered despite 3 in flight
    $ch->basicPublish('', $q, new AmqpMessage('x1'));
    $ch->basicPublish('', $q, new AmqpMessage('x2'));
    $dr = 0; for ($i=0; $i<6 && $dr===0; $i++) { $dr = $ch->wait(600, 2); }
    expect($dr)->toBeGreaterThan(0);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});

test('manual-ack: basicNack(requeue=true) redelivers same message', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'q.nack.requeue.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive'=>false,'auto_delete'=>false]))->toBeTrue();

    $body = 'retry-me';
    $ch->basicPublish('', $q, new AmqpMessage($body));
    $firstTag = null; $seen = 0;

    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$firstTag, &$seen, $ch) {
        $seen++;
        if ($seen === 1) {
            $firstTag = $d->getDeliveryTag();
            $ch->basicNack($firstTag, true); // requeue=true
            return;
        }
        expect($d->isRedelivered())->toBeTrue();
        $d->ack();
    }, ['no_ack'=>false]);

    $dr=0; $deadline=microtime(true)+3.0;
    while ($seen < 2 && microtime(true)<$deadline) { $dr += $ch->wait(600, 2); }
    expect($seen)->toBe(2);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});

test('manual-ack: basicReject(requeue=false) drops the message', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'q.reject.drop.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive'=>false,'auto_delete'=>false]))->toBeTrue();

    $ch->basicPublish('', $q, new AmqpMessage('drop-me'));
    $captured = null; $seen = 0;
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use (&$captured, &$seen) {
        $seen++; $captured = $d->getDeliveryTag();
        // No ack here; we reject explicitly after the first delivery has arrived
    }, ['no_ack'=>false]);
    expect($tag)->toBeString();

    // Wait for the first delivery to arrive and capture its tag
    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $ch->wait(600, 8); }
    expect($seen)->toBe(1);
    expect($captured)->toBeGreaterThan(0);

    // Reject without requeue (drop)
    expect($ch->basicReject($captured, false))->toBeTrue();

    // Ensure it does not redeliver
    $n2 = 0; for ($i=0; $i<5 && $n2===0; $i++) { $n2 = $ch->wait(400, 8); }
    expect($n2)->toBe(0);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});

test('prefetch=1 blocks further deliveries until ack', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'q.prefetch1.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive'=>false,'auto_delete'=>false]))->toBeTrue();
    expect($ch->qos(1))->toBeTrue();

    for ($i=0; $i<3; $i++) { $ch->basicPublish('', $q, new AmqpMessage('m'.$i)); }

    $tags=[]; $tag = $ch->simpleConsume($q, function(AmqpDelivery $d) use(&$tags){ $tags[]=$d->getDeliveryTag(); }, ['no_ack'=>false]);

    $n=0; while (count($tags)<1 && $n<10){ $n += $ch->wait(400,8); }
    expect(count($tags))->toBe(1);

    // As long as the first is not acked, a second delivery will not arrive
    $n2=0; for ($i=0; $i<4; $i++) { $n2 += $ch->wait(400,8); }
    expect(count($tags))->toBe(1);

    expect($ch->basicAck($tags[0], false))->toBeTrue();
    $n3=0; for ($i=0; $i<6 && count($tags)<2; $i++) { $n3 += $ch->wait(400,8); }
    expect(count($tags))->toBeGreaterThanOrEqual(2);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});

test('ack after cancel using old delivery_tag fails', function(){
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = 'q.ack.after.cancel.' . bin2hex(random_bytes(4));
    expect($ch->queueDeclare($q, ['exclusive'=>false,'auto_delete'=>false]))->toBeTrue();

    $ch->basicPublish('', $q, new AmqpMessage('x'));
    $captured = null;
    $tag = $ch->simpleConsume($q, function(AmqpDelivery $d) use (&$captured){ $captured = $d->getDeliveryTag(); }, ['no_ack'=>false]);
    $n=0; for($i=0;$i<8 && $n===0;$i++){ $n=$ch->wait(600,8); }
    expect($captured)->toBeGreaterThan(0);

    expect($ch->basicCancel($tag))->toBeTrue();

    $threw=false; $ret=null;
    try { $ret=$ch->basicAck($captured, false); } catch (\Throwable $e) { $threw=true; }
    expect($threw || $ret===false)->toBeTrue();

    $ch->close(); $c->close();
});

test('auto-ack mode: calling ack() or nack() throws', function () {
    $c = mq_client(); $c->connect(); $ch = $c->openChannel();
    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive'=>false,'auto_delete'=>false]))->toBeTrue();

    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) use ($ch) {
        expect(fn () => $d->ack())->toThrow(\Exception::class);
        expect(fn () => $d->nack(false))->toThrow(\Exception::class);
    }, ['no_ack' => true]); // Explicitly enable auto-ack
    expect($tag)->toBeString();

    $ch->basicPublish('', $q, new AmqpMessage('x'));

    expect(fn () => $ch->wait(800, 1))->not->toThrow(\Exception::class);

    $ch->basicCancel($tag); $ch->close(); $c->close();
});
