<?php

use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\AmqpMessage;

// ---------------------------------------------------------------------------------------------
// TRANSACTION TESTS
// ---------------------------------------------------------------------------------------------

test('PhpChannel transaction methods work correctly', function () {
    // Description: Tests that PhpChannel transaction methods work correctly.
    $c = mq_client();
    $ch = $c->openChannel();

    // Test tx_select
    expect($ch->txSelect())->toBeTrue();

    // Test tx_commit
    expect($ch->txCommit())->toBeTrue();

    // Test tx_rollback
    expect($ch->txSelect())->toBeTrue();
    expect($ch->txRollback())->toBeTrue();

    $ch->close();
    $c->close();
});

test('PhpChannel transaction with publish and rollback', function () {
    // Description: Tests that messages published in a transaction can be rolled back.
    $c = mq_client();
    $ch = $c->openChannel();
    
    // Declare a temporary queue for testing
    $queue = 'test_transaction_queue_' . uniqid();
    $ch->queueDeclare($queue, ['exclusive' => true]);

    // First, publish a message without transaction to establish baseline
    $message1 = new AmqpMessage('normal_message');
    expect($ch->basicPublish('', $queue, $message1))->toBeTrue();
    
    // Give some time for the message to be processed
    usleep(100000); // 100ms
    
    // Verify the normal message is delivered
    $delivery1 = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery1)->not->toBeNull();
    expect($delivery1->getBody())->toBe('normal_message');

    // Start transaction
    expect($ch->txSelect())->toBeTrue();
    
    // Publish a message within the transaction
    $message2 = new AmqpMessage('transaction_test_message');
    expect($ch->basicPublish('', $queue, $message2))->toBeTrue();
    
    // Rollback the transaction
    expect($ch->txRollback())->toBeTrue();
    
    // Verify that no additional message was delivered (queue should be empty)
    $delivery2 = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery2)->toBeNull();

    $ch->close();
    $c->close();
});

test('PhpChannel transaction with publish and commit', function () {
    // Description: Tests that messages published in a transaction are delivered after commit.
    $c = mq_client();
    $ch = $c->openChannel();
    
    // Declare a temporary queue for testing
    $queue = 'test_transaction_queue_' . uniqid();
    $ch->queueDeclare($queue, ['exclusive' => true]);

    // First, verify queue is empty
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();

    // Start transaction
    expect($ch->txSelect())->toBeTrue();
    
    // Publish a message within the transaction
    $message = new AmqpMessage('transaction_test_message');
    expect($ch->basicPublish('', $queue, $message))->toBeTrue();
    
    // Verify message is not yet available (transaction not committed)
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();
    
    // Commit the transaction
    expect($ch->txCommit())->toBeTrue();
    
    // Give some time for the message to be processed
    usleep(100000); // 100ms
    
    // Verify that the message was delivered (queue should contain the message)
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->not->toBeNull();
    expect($delivery->getBody())->toBe('transaction_test_message');

    $ch->close();
    $c->close();
});

test('PhpChannel transaction with multiple messages and rollback', function () {
    // Description: Tests that multiple messages published in a transaction can be rolled back.
    $c = mq_client();
    $ch = $c->openChannel();
    
    // Declare a temporary queue for testing
    $queue = 'test_transaction_queue_' . uniqid();
    $ch->queueDeclare($queue, ['exclusive' => true]);

    // First, verify queue is empty
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();

    // Start transaction
    expect($ch->txSelect())->toBeTrue();
    
    // Publish multiple messages within the transaction
    for ($i = 0; $i < 5; $i++) {
        $message = new AmqpMessage("transaction_message_$i");
        expect($ch->basicPublish('', $queue, $message))->toBeTrue();
    }
    
    // Verify messages are not yet available (transaction not committed)
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();
    
    // Rollback the transaction
    expect($ch->txRollback())->toBeTrue();
    
    // Verify that no messages were delivered (queue should still be empty)
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();

    $ch->close();
    $c->close();
});

test('PhpChannel transaction with multiple messages and commit', function () {
    // Description: Tests that multiple messages published in a transaction are delivered after commit.
    $c = mq_client();
    $ch = $c->openChannel();
    
    // Declare a temporary queue for testing
    $queue = 'test_transaction_queue_' . uniqid();
    $ch->queueDeclare($queue, ['exclusive' => true]);

    // First, verify queue is empty
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();

    // Start transaction
    expect($ch->txSelect())->toBeTrue();
    
    // Publish multiple messages within the transaction
    $messages = [];
    for ($i = 0; $i < 3; $i++) {
        $message = new AmqpMessage("transaction_message_$i");
        $messages[] = "transaction_message_$i";
        expect($ch->basicPublish('', $queue, $message))->toBeTrue();
    }
    
    // Verify messages are not yet available (transaction not committed)
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();
    
    // Commit the transaction
    expect($ch->txCommit())->toBeTrue();
    
    // Give some time for the messages to be processed
    usleep(200000); // 200ms
    
    // Verify that all messages were delivered
    $receivedMessages = [];
    for ($i = 0; $i < 3; $i++) {
        $delivery = $ch->basicGet($queue, ['no_ack' => true]);
        expect($delivery)->not->toBeNull();
        $receivedMessages[] = $delivery->getBody();
    }
    
    // Verify we received all messages
    expect(count($receivedMessages))->toBe(3);
    
    // Verify queue is now empty
    $delivery = $ch->basicGet($queue, ['no_ack' => true]);
    expect($delivery)->toBeNull();

    $ch->close();
    $c->close();
});