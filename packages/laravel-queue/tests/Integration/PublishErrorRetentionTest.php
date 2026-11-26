<?php

declare(strict_types=1);

use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;

it('surfaces every returned publication from a real broker across queue drains', function () {
    if (! extension_loaded('rabbit_rs')) {
        $this->fail('ext-rabbit_rs is required for this integration test');
    }

    $queueName = uniqueQueue('rabbit-rs-it-retained-errors');
    $pool = null;
    try {
        declareQueue($queueName);
        grantRabbitRsConfigure();
        [$pool, $queue] = integrationPoolAndQueue(
            $this->app,
            $queueName,
            configOverrides: [
                'routing_key' => uniqueQueue('rabbit-rs-it-unbound'),
                'flush_interval' => 3_600_000,
            ],
            connectionName: 'rabbit-rs-error-retention',
        );

        $ids = [];
        foreach (range(1, 3) as $number) {
            $ids[] = $queue->push('stdClass', ['sequence' => $number]);
        }

        try {
            $pool->flush();
            $this->fail('The first unroutable message must fail the flush');
        } catch (NativeException $exception) {
            expect($exception->getMessage())->toContain($ids[0]);
        }

        foreach (array_slice($ids, 1) as $id) {
            try {
                $queue->drainSettlementErrors();
                $this->fail('Every remaining returned message must surface');
            } catch (QueueException $exception) {
                expect($exception->getMessage())->toContain($id);
            }
        }
        $queue->drainSettlementErrors();
        expect($pool->drainErrors())->toBe([])
            ->and($pool->stats()['dropped_error_records_total'])->toBe(0);
    } finally {
        if ($pool !== null) {
            $pool->close();
        }
        deleteQueue($queueName);
    }
});
