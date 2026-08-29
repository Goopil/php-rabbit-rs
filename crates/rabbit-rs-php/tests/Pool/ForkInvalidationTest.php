<?php

declare(strict_types=1);

uses()->group('isolation');

function forkConfig(): array
{
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
            'vhost' => '/',
            'credentials' => ['username' => 'guest', 'password' => 'secret'],
            'tls' => ['enabled' => false, 'server_name' => null],
            'heartbeat' => 30,
        ]],
        'workers' => [],
        'topology_mode' => 'external',
    ];
}

it('invalidates inherited pools after fork and creates a child-local registry', function () {
    if (!extension_loaded('pcntl')) {
        $this->markTestSkipped('pcntl is required');
    }

    $parentPool = new \Goopil\RabbitRs\Pool(forkConfig());
    $parentStats = $parentPool->stats();

    $childPid = pcntl_fork();
    if ($childPid === -1) {
        throw new \Goopil\RabbitRs\Exception('fork failed');
    }

    if ($childPid === 0) {
        try {
            $parentPool->stats();
            exit(10);
        } catch (\Goopil\RabbitRs\Exception $e) {
            if (!str_contains($e->getMessage(), 'fork')) {
                exit(11);
            }
        }

        $childPool = new \Goopil\RabbitRs\Pool(forkConfig());
        $childStats = $childPool->stats();
        if ($childStats['pid'] !== getmypid()) {
            exit(12);
        }
        if ($childStats['handle'] === $parentStats['handle']) {
            exit(13);
        }
        $childPool->close();
        exit(0);
    }

    pcntl_waitpid($childPid, $status);
    expect(pcntl_wifexited($status))->toBeTrue('child must exit normally');
    expect(pcntl_wexitstatus($status))->toBe(0, 'child lifecycle assertions');
    expect($parentPool->stats()['handle'])->toBe($parentStats['handle'], 'parent handle remains valid');
    $parentPool->close();
});
