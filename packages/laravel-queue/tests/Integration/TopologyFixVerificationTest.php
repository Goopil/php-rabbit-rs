<?php

declare(strict_types=1);

/**
 * Issue #273: `rabbit-rs:topology --fix` must verify the declared objects
 * before reporting success — a false "topology declared" turns the repair
 * path into a silent outage. Run with the lab stack (CI only: the host has
 * no ext-rabbit_rs).
 */
describe('topology --fix post-condition verification', function () {
    beforeEach(function () {
        if (! extension_loaded('rabbit_rs')) {
            skip('ext-rabbit_rs is required for integration tests');
        }
        // The lab's stored configure permission only allows amq.* and
        // rabbit-rs-it-* names, so the unique objects live inside that regex.
        grantRabbitRsConfigure();
    });

    afterEach(function () {
        if (! isset($this->queueName)) {
            return;
        }
        deleteQueue($this->queueName);
        if (isset($this->exchangeName)) {
            managementRequest('DELETE', 'http://localhost:15672/api/exchanges/'.rawurlencode(ORDERS_VHOST).'/'.urlencode($this->exchangeName));
        }
    });

    it('confirms every declared object on the broker and exits 0 on a fresh topology', function () {
        // Nothing is pre-declared: the command itself must provision the
        // queue, the route exchange, and the route binding, then confirm
        // each of them before reporting success.
        $this->queueName = uniqueQueue();
        $this->exchangeName = uniqueQueue();

        config()->set('queue.connections.'.INTEGRATION_CONNECTION, array_merge(liveConfig($this->queueName), [
            'exchange' => $this->exchangeName,
            'management_url' => 'http://localhost:15672',
        ]));

        $vhost = rawurlencode(ORDERS_VHOST);

        $this->artisan('rabbit-rs:topology', ['--fix' => true, '--connection' => [INTEGRATION_CONNECTION]])
            ->expectsOutputToContain("queue '{$this->queueName}' exists")
            ->expectsOutputToContain("exchange '{$this->exchangeName}' declared")
            ->expectsOutputToContain("route binding '{$this->exchangeName}' -> '{$this->queueName}' declared")
            ->expectsOutputToContain('topology declared')
            ->assertExitCode(0);

        // The post-condition the success line claims, checked independently
        // through the management api (admin credentials).
        $queue = json_decode(managementRequest('GET', "http://localhost:15672/api/queues/{$vhost}/".urlencode($this->queueName)), true);
        expect($queue['name'] ?? null)->toBe($this->queueName);

        $exchange = json_decode(managementRequest('GET', "http://localhost:15672/api/exchanges/{$vhost}/".urlencode($this->exchangeName)), true);
        expect($exchange['name'] ?? null)->toBe($this->exchangeName);

        $bindings = json_decode(
            managementRequest('GET', "http://localhost:15672/api/exchanges/{$vhost}/{$this->exchangeName}/bindings/source"),
            true,
        ) ?? [];
        $routeBinding = array_values(array_filter($bindings, fn (array $binding): bool => ($binding['destination'] ?? null) === $this->queueName));
        expect($routeBinding)->not->toBeEmpty('route binding must exist on the broker after --fix');

        // A plain verify run confirms the fixed topology and exits 0.
        $this->artisan('rabbit-rs:topology', ['--connection' => [INTEGRATION_CONNECTION]])
            ->assertExitCode(0);
    });

    it('fails per object and never reports success for an un-declarable queue', function () {
        // The rabbit_rs user's configure permission only covers amq.*/rabbit-rs
        // names, so this queue is rejected broker-side at declare time
        // (ACCESS_REFUSED — the deterministic un-declarable-object pattern
        // from the lab's bug 13 write-up). The command must print the
        // per-object failure, never its success line, and exit non-zero.
        $this->queueName = 'blocked-'.uniqid('', true);

        config()->set('queue.connections.'.INTEGRATION_CONNECTION, array_merge(liveConfig($this->queueName), [
            'management_url' => 'http://localhost:15672',
        ]));

        Artisan::call('rabbit-rs:topology', ['--fix' => true, '--connection' => [INTEGRATION_CONNECTION]]);
        $output = Artisan::output();

        // The verify probe races the declare bring-up's doomed retries: when
        // the bring-up's FailedPermanent teardown closes the connection
        // first, the probe reports "probe failed" instead of a clean 404
        // "is missing" (issue #285). Either way the per-object failure is
        // reported and success is never claimed.
        $missingOrUnverifiable = str_contains($output, "queue '{$this->queueName}' is missing")
            || str_contains($output, "queue '{$this->queueName}' probe failed:");

        expect($missingOrUnverifiable)->toBeTrue('expected a per-object queue failure, got: '.$output)
            ->and($output)->toContain('declaration failed')
            ->and($output)->not->toContain('topology declared')
            ->and(Artisan::call('rabbit-rs:topology', ['--fix' => true, '--connection' => [INTEGRATION_CONNECTION]]))->toBe(1);
    });
});
