<?php

use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\AmqpMessage;

// ---------------------------------------------------------------------------------------------
// BASIC CHANNEL TESTS
// ---------------------------------------------------------------------------------------------

test('PhpClient can open a channel and get an id', function () {
    // Description: Tests that PhpClient can open a channel and get a valid channel ID.
    $c = mq_client();

    $ch = $c->openChannel();
    expect($ch->getId())->toBeGreaterThan(0);

    $ch->close();
    $c->close();
});

test('PhpChannel qos() accepts a sane prefetch and basicPublish succeeds (confirm ACK)', function () {
    // Description: Tests that PhpChannel qos() accepts a sane prefetch value and basicPublish succeeds.
    $c = mq_client();
    $ch = $c->openChannel();

    expect($ch->qos(10))->toBeTrue();
    expect($ch->basicPublish('amq.fanout', '', new AmqpMessage('hello-from-tests')))->toBeTrue();

    $ch->close();
    $c->close();
});
