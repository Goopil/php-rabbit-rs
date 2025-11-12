<?php

use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\AmqpMessage;

// ---------------------------------------------------------------------------------------------
// EXCHANGE UNBIND TESTS
// ---------------------------------------------------------------------------------------------

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

test('exchangeUnbind with arguments works correctly', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $src = 'ex.src.args.' . bin2hex(random_bytes(3));
    $dst = 'ex.dst.args.' . bin2hex(random_bytes(3));
    $q   = 'q.x2x.args.'  . bin2hex(random_bytes(3));

    expect($ch->exchangeDeclare($src, 'topic'))->toBeTrue();
    expect($ch->exchangeDeclare($dst, 'fanout'))->toBeTrue();
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    expect($ch->queueBind($q,  $dst, ''))->toBeTrue();
    
    // Bind with arguments
    expect($ch->exchangeBind($dst, $src, 'test.key', [
        'arguments' => ['test-arg' => 'value']
    ]))->toBeTrue();

    // Unbind with same arguments
    expect($ch->exchangeUnbind($dst, $src, 'test.key', [
        'arguments' => ['test-arg' => 'value']
    ]))->toBeTrue();

    $ch->close(); $c->close();
});

// ---------------------------------------------------------------------------------------------
// BASIC RECOVER TESTS
// ---------------------------------------------------------------------------------------------

test('basicRecover requeues unacked messages', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = 'q.recover.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Publish a few messages
    for ($i = 1; $i <= 3; $i++) {
        expect($ch->basicPublish('', $q, new AmqpMessage("message-$i")))->toBeTrue();
    }

    // Get messages without acking them
    $deliveries = [];
    for ($i = 1; $i <= 3; $i++) {
        $delivery = $ch->basicGet($q, ['no_ack' => false]);
        expect($delivery)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
        $deliveries[] = $delivery;
    }

    // Queue should be empty now
    $empty = $ch->basicGet($q, ['no_ack' => false]);
    expect($empty)->toBeNull();

    // Call basic.recover with requeue=true
    expect($ch->basicRecover(true))->toBeTrue();

    // Messages should be available again
    for ($i = 1; $i <= 3; $i++) {
        $delivery = $ch->basicGet($q, ['no_ack' => false]);
        expect($delivery)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
        expect($delivery->getBody())->toBe("message-$i");
    }

    $ch->close(); $c->close();
});

test('basicRecover with requeue=false throws NOT_IMPLEMENTED on RabbitMQ', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = 'q.recover.no.requeue.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Publish a message
    expect($ch->basicPublish('', $q, new AmqpMessage('test-message')))->toBeTrue();

    // Get message without acking it
    $delivery = $ch->basicGet($q, ['no_ack' => false]);
    expect($delivery)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);

    // Queue should be empty now
    $empty = $ch->basicGet($q, ['no_ack' => false]);
    expect($empty)->toBeNull();

    // Call basic.recover with requeue=false - should throw NOT_IMPLEMENTED on RabbitMQ
    expect(fn() => $ch->basicRecover(false))->toThrow(Exception::class);

    // Channel is closed by broker after NOT_IMPLEMENTED; tolerate close()
    try { $ch->close(); } catch (Exception $e) { /* already closed by broker */ }
    $c->close();
});

test('basicRecover works with default requeue parameter', function () {
    $c = mq_client(); $ch = $c->openChannel();

    $q = 'q.recover.default.' . bin2hex(random_bytes(3));
    expect($ch->queueDeclare($q, ['exclusive' => true, 'auto_delete' => true]))->toBeTrue();

    // Publish a message
    expect($ch->basicPublish('', $q, new AmqpMessage('test-message')))->toBeTrue();

    // Get message without acking it
    $delivery = $ch->basicGet($q, ['no_ack' => false]);
    expect($delivery)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);

    // Call basic.recover without parameters (should default to requeue=true)
    expect($ch->basicRecover())->toBeTrue();

    // Message should be available again
    $delivery = $ch->basicGet($q, ['no_ack' => false]);
    expect($delivery)->toBeInstanceOf(\Goopil\RabbitRs\AmqpDelivery::class);
    expect($delivery->getBody())->toBe('test-message');

    $ch->close(); $c->close();
});