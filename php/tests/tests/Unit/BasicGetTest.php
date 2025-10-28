<?php

use PHPUnit\Framework\ExpectationFailedException;
use Goopil\RabbitRs\AmqpMessage;
use Goopil\RabbitRs\AmqpDelivery;
use Goopil\RabbitRs\PhpClient;

test('basicGet on empty queue returns null', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $got = $ch->basicGet($q); // default no_ack=true
    expect($got)->toBeNull();

    $ch->close(); $c->close();
});

test('basicGet default no_ack=true drains one message and next call is null', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('m1')))->toBeTrue();

    $d1 = $ch->basicGet($q); // auto-ack by default (as per amqplib)
    expect($d1)->toBeInstanceOf(AmqpDelivery::class);
    expect($d1->getBody())->toBe('m1');

    // When no_ack=true (auto-ack), calling ack() or nack() raises an exception
    expect(fn () => $d1->ack())->toThrow(Exception::class);
    expect(fn () => $d1->nack(true))->toThrow(Exception::class);

    // Because the message was auto-acked, nothing remains
    $d2 = $ch->basicGet($q);
    expect($d2)->toBeNull();

    $ch->close(); $c->close();
});

test('basicGet manual-ack: ack() removes the message', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('ack-me')))->toBeTrue();

    $d = $ch->basicGet($q, ['no_ack' => false]);
    expect($d)->toBeInstanceOf(AmqpDelivery::class);
    expect($d->getBody())->toBe('ack-me');

    $d->ack();

    // Nothing else remains afterward
    expect($ch->basicGet($q))->toBeNull();

    $ch->close(); $c->close();
});

test('basicGet manual-ack: nack(requeue=true) re-delivers the same message with redelivered=true', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = mh_tmp_queue(); // non auto_delete queue so redelivery can be observed cleanly
    expect($ch->queueDeclare($q, ['exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('retry')))->toBeTrue();

    $d1 = $ch->basicGet($q, ['no_ack' => false]);
    expect($d1)->toBeInstanceOf(AmqpDelivery::class);
    expect($d1->getBody())->toBe('retry');

    $d1->nack(true); // requeue

    // Should come back immediately with the redelivered flag set
    $d2 = $ch->basicGet($q, ['no_ack' => false]);
    expect($d2)->toBeInstanceOf(AmqpDelivery::class);
    expect($d2->getBody())->toBe('retry');
    expect($d2->isRedelivered())->toBeTrue();

    $d2->ack();

    $ch->close(); $c->close();
});

test('basicGet manual-ack: reject(requeue=false) drops the message (no redelivery)', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = mh_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => false, 'auto_delete' => false]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('drop-me')))->toBeTrue();

    $d = $ch->basicGet($q, ['no_ack' => false]);
    expect($d)->toBeInstanceOf(AmqpDelivery::class);
    expect($d->getBody())->toBe('drop-me');

    $d->reject(false); // drop

    // Should not be delivered again
    expect($ch->basicGet($q, ['no_ack' => false]))->toBeNull();

    $ch->close(); $c->close();
});

test('basicGet multiple messages follow the publish order (FIFO by default)', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->basicPublish('', $q, new AmqpMessage('m1')))->toBeTrue();
    expect($ch->basicPublish('', $q, new AmqpMessage('m2')))->toBeTrue();
    expect($ch->basicPublish('', $q, new AmqpMessage('m3')))->toBeTrue();

    // no_ack=true (default) -> each get auto-acks and proceeds to the next
    $d1 = $ch->basicGet($q);
    $d2 = $ch->basicGet($q);
    $d3 = $ch->basicGet($q);

    expect($d1?->getBody())->toBe('m1');
    expect($d2?->getBody())->toBe('m2');
    expect($d3?->getBody())->toBe('m3');

    expect($ch->basicGet($q))->toBeNull();

    $ch->close(); $c->close();
});

test('basicGet against an unknown queue closes the channel (NOT_FOUND)', function () {
    $c = mq_client(); $ch = $c->openChannel();

    // The broker will respond NOT_FOUND -> surface an exception and close the channel
    expect(fn () => $ch->basicGet('q.does.not.exist'))->toThrow(Exception::class);
    expect(fn () => $ch->wait(100, 1))->toThrow(Exception::class);

    $c->close();
});


test('basicGet returns routing metadata (exchange, routingKey, redelivered, tag)', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Default exchange -> routing_key must be the queue name
    expect($ch->basicPublish('', $q, new AmqpMessage('m')))->toBeTrue();

    $d = $ch->basicGet($q, ['no_ack' => false]);
    expect($d)->toBeInstanceOf(AmqpDelivery::class);
    expect($d->getExchange())->toBe('');
    expect($d->getRoutingKey())->toBe($q);
    expect($d->isRedelivered())->toBeFalse();
    expect($d->getDeliveryTag())->toBeGreaterThan(0);

    $d->nack(true); // requeue

    $d2 = $ch->basicGet($q, ['no_ack' => false]);
    expect($d2)->toBeInstanceOf(AmqpDelivery::class);
    expect($d2->isRedelivered())->toBeTrue();
    $d2->ack();

    $ch->close(); $c->close();
});

// Properties & headers should be preserved exactly as with consume
test('basicGet preserves properties and headers', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $bin = random_bytes(8);

    $msg = new AmqpMessage('hello', [
        'content_type' => 'application/json',
        'priority' => 7,
        'app_id' => 'test-app',
        'application_headers' => [
            'k1' => 'v',
            'k2' => 42,
            'k3' => 3.14,
            'k4' => null,
            'k5' => true,
            'tbl' => [ 'a' => 1, 'b' => [ 'x' => 'y' ] ],
            'list' => [1, 2, 3],
            'bin' => $bin,
        ],
    ]);

    expect($ch->basicPublish('', $q, $msg))->toBeTrue();

    $d = $ch->basicGet($q); // auto-ack
    expect($d)->toBeInstanceOf(AmqpDelivery::class);
    expect($d->getContentType())->toBe('application/json');
    expect($d->getPriority())->toBe(7);
    expect($d->getAppId())->toBe('test-app');
    $headers = $d->getHeaders();
    expect($headers['k1'])->toBe('v');
    expect($headers['k2'])->toBe(42);
    expect($headers['k5'])->toBe(true);
    expect($headers['tbl']['b']['x'])->toBe('y');
    expect(count($headers['list']))->toBe(3);
    expect(strlen($headers['bin']))->toBe(strlen($bin));
    expect($headers['bin'] === $bin)->toBeTrue();

    $ch->close(); $c->close();
});

// Large payloads should be retrievable via basic_get as well
test('basicGet handles very large payload intact', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $payload = random_bytes(512 * 1024 + 7);
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $d = $ch->basicGet($q);
    expect($d)->toBeInstanceOf(AmqpDelivery::class);
    expect(strlen($d->getBody()))->toBe(strlen($payload));
    expect($d->getBody() === $payload)->toBeTrue();

    $ch->close(); $c->close();
});

// Unicode payload through basic_get
test('basicGet preserves unicode payload', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $payload = "héllo 🐇 — 你好 — Привет — عربى";
    expect($ch->basicPublish('', $q, new AmqpMessage($payload)))->toBeTrue();

    $d = $ch->basicGet($q);
    expect($d)->toBeInstanceOf(AmqpDelivery::class);
    expect($d->getBody())->toBe($payload);

    $ch->close(); $c->close();
});

// Manual-ack: multiple unacked deliveries are allowed; ack later
test('basicGet manual-ack allows multiple outstanding deliveries until ack', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    for ($i=0; $i<3; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage('m'.$i)))->toBeTrue();
    }

    $d1 = $ch->basicGet($q, ['no_ack' => false]);
    $d2 = $ch->basicGet($q, ['no_ack' => false]);
    $d3 = $ch->basicGet($q, ['no_ack' => false]);

    expect($d1->getBody())->toBe('m0');
    expect($d2->getBody())->toBe('m1');
    expect($d3->getBody())->toBe('m2');

    // Ack them out of order, server should accept tags independently
    $d2->ack();
    $d1->ack();
    $d3->ack();

    expect($ch->basicGet($q))->toBeNull();

    $ch->close(); $c->close();
});

// Closing the channel without ack should requeue unacked message
// and allow a fresh consumer to pick it with redelivered=true (amqplib parity)
test('basicGet manual-ack then close channel requeues message for next consumer', function () {
    $c = mq_client();

    $q = mh_tmp_queue(); // durable/permanent helper

    // Channel 1: GET without ack, then close
    $ch1 = $c->openChannel();
    expect($ch1->queueDeclare($q, ['exclusive' => false, 'auto_delete' => false]))->toBeTrue();
    expect($ch1->basicPublish('', $q, new AmqpMessage('stay')))->toBeTrue();

    $d = $ch1->basicGet($q, ['no_ack' => false]);
    expect($d)->toBeInstanceOf(AmqpDelivery::class);
    expect($d->getBody())->toBe('stay');

    // Close without ack -> broker requeues
    $ch1->close();

    // Channel 2: consume should see the message redelivered
    $ch2 = $c->openChannel();
    $got = null; $tag = $ch2->simpleConsume($q, function (AmqpDelivery $m) use (&$got) { $got = $m; });
    // give the consumer time to attach and drain
    $n = 0; for ($i=0; $i<10 && $n===0; $i++) { $n = $ch2->wait(600, 8); }

    expect($got)->toBeInstanceOf(AmqpDelivery::class);
    expect($got->getBody())->toBe('stay');
    expect($got->isRedelivered())->toBeTrue();
    $ch2->basicCancel($tag);

    $ch2->close(); $c->close();
});

// Interleaving with a live consumer: publish after consumer attached; GET should not steal already-delivered messages
// (Best-effort: we only assert that either GET gets it or consumer gets it, but not both.)
// Marked as weakly deterministic, but validates there is no duplication.
 test('basicGet does not duplicate messages alongside a live consumer', function () {
    $c = mq_client(); $c->connect();
    $chC = $c->openChannel();
    $q = sc_tmp_queue();
    expect($chC->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    $seen = 0;
    $tag = $chC->simpleConsume($q, function () use (&$seen) { $seen++; });

    // let consumer attach
    usleep(50_000);

    // Publish one; then try a GET immediately on a separate channel
    expect($chC->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();

    $chG = $c->openChannel();
    $got = $chG->basicGet($q);

    // Drain consumer side a bit
    $n = 0; for ($i=0; $i<6 && $n===0; $i++) { $n = $chC->wait(400, 4); }

    // Exactly one side must have seen it
    expect(($got instanceof AmqpDelivery) xor ($seen === 1))->toBeTrue();

    $chC->basicCancel($tag);
    $chC->close(); $chG->close(); $c->close();
});

// -----------------------------------------------------------------------------
// basic_get manual-ack: old delivery_tag cannot be acked after channel close; message requeued
// -----------------------------------------------------------------------------

test('basicGet manual-ack: old delivery_tag cannot be acked after channel close; message is requeued and redelivered', function () {
    $c = mq_client();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // publish one message
    expect($ch->basicPublish('', $q, new AmqpMessage('x')))->toBeTrue();

    // manual-ack get
    $d1 = $ch->basicGet($q, ['no_ack' => false]);
    expect($d1)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    $tag1 = $d1->getDeliveryTag();

    // Close the channel to force broker to requeue the unacked message
    expect($ch->close())->toBeTrue();

    // Ack with the old tag on the now-closed (or invalidated) channel should either throw or return false
    $threw = false; $ret = null;
    try { $ret = $ch->basicAck($tag1, false); } catch (\Throwable $e) { $threw = true; }
    expect($threw || $ret === false)->toBeTrue();

    // Re-open a fresh channel and verify the message is requeued and marked redelivered
    $ch2 = $c->openChannel();
    $d2 = $ch2->basicGet($q, ['no_ack' => false]);
    expect($d2)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect($d2->getBody())->toBe('x');
    expect($d2->isRedelivered())->toBeTrue();

    // cleanup
    expect($ch2->basicAck($d2->getDeliveryTag(), false))->toBeTrue();
    $ch2->close();
    $c->close();
});

test('default behavior is auto-ack (no_ack=true)', function () {
    $c = mq_client(); $c->connect();
    $ch = $c->openChannel();

    $q = sc_tmp_queue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Pas d'options fournies → no_ack=true implicitement
    $tag = $ch->simpleConsume($q, function (AmqpDelivery $d) {
        expect(fn () => $d->ack())->toThrow(Exception::class);
    });

    expect($ch->basicPublish('', $q, new AmqpMessage('msg')))->toBeTrue();

    expect($ch->wait(500, 1))->toBe(1); // message processed and auto-acked

    // No redelivery expected
    expect($ch->basicGet($q))->toBeNull();

    $ch->close(); $c->close();
});
