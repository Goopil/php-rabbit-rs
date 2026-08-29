<?php

declare(strict_types=1);

describe('blind flush barrier', function () {
    it('delivers buffered publications to the broker before flush returns', function () {
        $pool = testingPool(defaultConfig(), ['publisher_safety' => 'blind']);

        try {
            $pool->publish(pubMessage('blind-1'));
            $pool->publish(pubMessage('blind-2'));
            $pool->flush();

            expect($pool->stats()['publishes_total'])->toBe(2);

            // A second flush is a no-op barrier: no duplicate deliveries.
            $pool->flush();
            expect($pool->stats()['publishes_total'])->toBe(2);
        } finally {
            $pool->close();
        }
    });
});
