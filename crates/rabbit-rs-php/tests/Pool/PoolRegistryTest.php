<?php

declare(strict_types=1);

function poolConfig(string $vhost = '/'): array
{
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
            'vhost' => $vhost,
            'credentials' => ['username' => 'guest', 'password' => 'secret'],
            'tls' => ['enabled' => false, 'server_name' => null],
            'heartbeat' => 30,
        ]],
        'workers' => [],
        'topology_mode' => 'external',
    ];
}

describe('pool registry', function () {
    it('reports the current PID', function () {
        $pool = new \Goopil\RabbitRs\Pool(poolConfig());
        expect($pool->stats()['pid'])->toBe(getmypid());
        $pool->close();
    });

    it('reuses one handle for equivalent configs', function () {
        $first = new \Goopil\RabbitRs\Pool(poolConfig());
        $second = new \Goopil\RabbitRs\Pool(poolConfig());

        expect($first->stats()['handle'])->toBe($second->stats()['handle']);

        $first->close();
    });

    it('produces distinct handles for different vhosts', function () {
        $first = new \Goopil\RabbitRs\Pool(poolConfig());
        $different = new \Goopil\RabbitRs\Pool(poolConfig('/other'));

        expect($first->stats()['handle'])->not->toBe($different->stats()['handle']);

        $first->close();
        $different->close();
    });

    it('does not expose the internal configuration key', function () {
        $pool = new \Goopil\RabbitRs\Pool(poolConfig());
        expect($pool->stats())->not->toHaveKey('key');
        $pool->close();
    });

    it('invalidates aliases after closing a shared handle', function () {
        $first = new \Goopil\RabbitRs\Pool(poolConfig());
        $second = new \Goopil\RabbitRs\Pool(poolConfig());
        $first->close();

        try {
            $second->stats();
            expect(false)->toBeTrue('closing a shared handle must invalidate its aliases');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('closed');
        }
    });

    it('replaces a closed handle with a new one', function () {
        $first = new \Goopil\RabbitRs\Pool(poolConfig());
        $firstHandle = $first->stats()['handle'];
        $first->close();

        $replacement = new \Goopil\RabbitRs\Pool(poolConfig());
        expect($replacement->stats()['handle'])->not->toBe($firstHandle);
        $replacement->close();
    });
});
