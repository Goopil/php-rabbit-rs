<?php

declare(strict_types=1);

/**
 * Native config for the pool claim tests. Publishing stays buffer-only so
 * the assertions target pool handle state, never a broker, and the
 * close-time flush wait stays bounded.
 */
function claimProbeConfig(): array
{
    $config = defaultConfig();
    $config['publisher'] = [
        'flush_interval' => 3_600_000,
        'confirm_timeout' => 1000,
    ];

    return $config;
}

describe('pool handle claims', function () {
    it('keeps a live pool working when a transient probe pool closes', function () {
        $live = new \Goopil\RabbitRs\Pool(claimProbeConfig());
        $liveHandle = $live->stats()['handle'];

        // The doctor pattern (issue #221): the probe pool is built from the
        // same native config as the live pool, so both resolve to one shared
        // connection handle.
        $probe = new \Goopil\RabbitRs\Pool(claimProbeConfig());
        expect($probe->stats()['handle'])->toBe($liveHandle);

        $probe->close();

        // The live pool must keep working after the probe closed.
        expect($live->stats()['handle'])->toBe($liveHandle);
        expect($live->publish(pubMessage('after-probe')))->toBe('after-probe');

        // Closing the last claim tears the shared connection down: the
        // registry replaces the retired handle on the next construction.
        $live->close();
        $fresh = new \Goopil\RabbitRs\Pool(claimProbeConfig());
        expect($fresh->stats()['handle'])->not->toBe($liveHandle);
        $fresh->close();
    });
});
