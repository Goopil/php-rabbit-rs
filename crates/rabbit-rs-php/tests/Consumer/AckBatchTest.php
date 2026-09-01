<?php

declare(strict_types=1);

function fetchDeliveries(\Goopil\RabbitRs\Consumer $consumer, int $count): array
{
    $deliveries = $consumer->nextBatch(min($count, 256), 10);
    while (count($deliveries) < $count) {
        $deliveries[] = $consumer->next(10);
    }

    return array_slice($deliveries, 0, $count);
}

describe('ackBatch boundary', function () {
    it('acknowledges exactly 256 deliveries', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => array_map(
                fn (int $i) => ['message_id' => "ack-{$i}", 'payload' => 'p'],
                range(0, 255),
            ),
        ]);
        $consumer = $pool->consumer('main');
        $deliveries = fetchDeliveries($consumer, 256);

        $consumer->ackBatch($deliveries);

        expect($deliveries[0]->metadata()['state'])->toBe('acked');
        expect($deliveries[255]->metadata()['state'])->toBe('acked');

        $pool->close();
    });

    it('rejects 257 deliveries before any settlement side effect', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => array_map(
                fn (int $i) => ['message_id' => "over-{$i}", 'payload' => 'p'],
                range(0, 256),
            ),
        ]);
        $consumer = $pool->consumer('main');
        $deliveries = fetchDeliveries($consumer, 257);

        expect(fn () => $consumer->ackBatch($deliveries))
            ->toThrow(\Goopil\RabbitRs\Exception::class, 'ackBatch: maximum 256 deliveries per call');

        expect($deliveries[0]->metadata()['state'])->toBe('pending');
        expect($deliveries[255]->metadata()['state'])->toBe('pending');
        expect($deliveries[256]->metadata()['state'])->toBe('pending');

        $pool->close();
    });
});
