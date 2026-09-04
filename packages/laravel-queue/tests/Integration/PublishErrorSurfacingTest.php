<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Exceptions\QueueException;

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    $this->queueName = uniqueQueue('rabbit-rs-it-surface');
    declareQueue($this->queueName);
    grantRabbitRsConfigure();

    // A routing key with no binding on the default exchange makes every
    // publish unroutable while pops keep consuming from the declared queue.
    // Safe mode (the compiled default) publishes with mandatory=true, so the
    // broker must return the publication instead of silently dropping it.
    [$this->pool, $this->queue] = integrationPoolAndQueue(
        $this->app,
        $this->queueName,
        configOverrides: ['routing_key' => 'rabbit-rs-it-no-binding-'.uniqid('', true)],
    );
});

afterEach(function () {
    // Best-effort teardown: pending publish errors would otherwise surface
    // from stats() and mask the test result.
    try {
        if (isset($this->pool)) {
            $this->pool->drainErrors();
            if (! $this->pool->stats()['closed']) {
                $this->pool->close();
            }
        }
    } catch (Throwable) {
        // terminal pool state is irrelevant for teardown
    }
    deleteQueue($this->queueName);
});

it('surfaces an unroutable mandatory publish at the next pop', function () {
    $this->queue->push('stdClass', ['unroutable' => true]);

    // The sync flush inside the consumer path raises the definitive return
    // at the pop that flushed it (full-deadline semantics); a pipelined
    // drain would record it for the next pop instead. Either way it must
    // surface, never vanish.
    $thrown = null;
    $deadline = microtime(true) + 5;
    while (microtime(true) < $deadline) {
        try {
            $this->queue->pop();
            usleep(100_000);
        } catch (QueueException $exception) {
            $thrown = $exception;
            break;
        }
    }

    expect($thrown)->not->toBeNull('an unroutable mandatory publication must surface at the next pop')
        ->and($thrown->getMessage())->toContain('unroutable');
});

it('surfaces a returned batch through drainSettlementErrors', function () {
    // 64 pushes reach the buffer threshold: the auto-flush spawns a
    // pipelined drain, so the returns are recorded for the next operation
    // instead of raised from a synchronous flush.
    for ($i = 0; $i < 64; $i++) {
        $this->queue->push('stdClass', ['unroutable' => $i]);
    }

    $thrown = null;
    $deadline = microtime(true) + 5;
    while (microtime(true) < $deadline) {
        try {
            $this->queue->drainSettlementErrors();
            usleep(100_000);
        } catch (QueueException $exception) {
            $thrown = $exception;
            break;
        }
    }

    expect($thrown)->not->toBeNull('drainSettlementErrors must raise the recorded returns')
        ->and($thrown->getMessage())->toContain('unroutable');

    // One failure is raised per call (sync-parity: the raise consumes the
    // records taken at that moment); the next call must find the queue
    // empty rather than re-raising stale records.
    expect(count($this->pool->drainErrors()))->toBeLessThanOrEqual(63);
});
