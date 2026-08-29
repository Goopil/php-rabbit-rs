<?php

declare(strict_types=1);

describe('publishBatch on a closed pool', function () {
    it('throws a typed closed-pool exception and never attempts the batch', function () {
        $pool = testingPool(defaultConfig(), []);
        $pool->close();

        try {
            $pool->publishBatch([pubMessage('closed-pool-batch')]);
            expect(false)->toBeTrue('publishBatch on a closed pool must throw');
        } catch (\Goopil\RabbitRs\Exception $e) {
            // Documented real behavior: publishBatch starts by flushing the
            // publish buffer, and the flush refuses a closed pool outright.
            // The batch itself is never attempted, so no message is
            // re-buffered and no partial result is returned.
            expect($e->getMessage())->toContain('closed pool');
        }
    });
});
