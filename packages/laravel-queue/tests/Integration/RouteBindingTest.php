<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;

describe('route binding declared by the pool', function () {
    beforeEach(function () {
        if (! extension_loaded('rabbit_rs')) {
            skip('ext-rabbit_rs is required for integration tests');
        }

        // Issue #205: nothing is pre-declared here. The declare-mode pool must
        // provision the queue itself, the publish route exchange, and the route
        // binding — otherwise the push below is unroutable and the pop stays
        // empty. The lab's stored configure permission only allows amq.* and
        // rabbit-rs-it-* names, so the unique exchange lives inside that regex.
        grantRabbitRsConfigure();

        $this->queueName = uniqueQueue();
        $this->exchangeName = uniqueQueue();

        [$this->pool, $this->queue] = integrationPoolAndQueue(
            $this->app,
            $this->queueName,
            configOverrides: [
                'exchange' => $this->exchangeName,
                'routing_key' => '{queue}',
            ],
            connectOverrides: ['block_for' => 3],
        );
    });

    afterEach(function () {
        if (isset($this->pool) && ! $this->pool->stats()['closed']) {
            $this->pool->close();
        }
        deleteQueue($this->queueName);
        // PoisonDeliveryTest declares a local deleteExchange(); keep the helper
        // local here too so the two never collide in one Pest run.
        managementRequest('DELETE', 'http://localhost:15672/api/exchanges/'.rawurlencode(ORDERS_VHOST).'/'.urlencode($this->exchangeName));
    });

    it('declares the publish route so a freshly declared queue is reachable', function () {
        $this->queue->push('stdClass', ['message' => 'route-binding']);

        $job = $this->queue->pop();
        expect($job)->not->toBeNull()
            ->toBeInstanceOf(RabbitMqJob::class);

        $body = json_decode($job->getRawBody(), true);
        expect($body['data'])->toBe(['message' => 'route-binding']);

        $job->delete();
        expect($this->queue->pop())->toBeNull();
    });
});

describe('topology command on the publish route', function () {
    beforeEach(function () {
        if (! extension_loaded('rabbit_rs')) {
            skip('ext-rabbit_rs is required for integration tests');
        }
        grantRabbitRsConfigure();

        // Pool-free on purpose: the topology command's probes close their
        // transient pools, and a probe pool sharing the live pool's config
        // fingerprint tears its shared connection down with it (the ext
        // closes the shared ConnectionHandle without a use count).
        $this->queueName = uniqueQueue();
        $this->exchangeName = uniqueQueue();

        config()->set('queue.connections.'.INTEGRATION_CONNECTION, array_merge(liveConfig($this->queueName), [
            'exchange' => $this->exchangeName,
            'routing_key' => '{queue}',
            // liveConfig carries no management_url; verify's route checks are
            // advisory without one. The lab's rabbit_rs user holds the
            // management tag, so its credentials suffice for the API.
            'management_url' => 'http://localhost:15672',
        ]));
    });

    afterEach(function () {
        deleteQueue($this->queueName);
        managementRequest('DELETE', 'http://localhost:15672/api/exchanges/'.rawurlencode(ORDERS_VHOST).'/'.urlencode($this->exchangeName));
    });

    it('flags a deleted route binding in verify and re-declares it with --fix', function () {
        $vhostUrl = rawurlencode(ORDERS_VHOST);
        $bindingsUrl = "http://localhost:15672/api/exchanges/{$vhostUrl}/{$this->exchangeName}/bindings/source";
        $routeBinding = static function () use ($bindingsUrl): array {
            return json_decode(managementRequest('GET', $bindingsUrl), true) ?? [];
        };
        $topology = fn (array $options = []) => $this->artisan(
            'rabbit-rs:topology',
            array_merge(['--connection' => [INTEGRATION_CONNECTION]], $options),
        );

        // Declare the full topology through the command itself, then confirm
        // the route binding landed on the broker.
        $topology(['--fix' => true])->assertExitCode(0);
        expect($routeBinding())->not->toBeEmpty();

        // Verify is green while the binding exists.
        $topology()->assertExitCode(0);

        // Delete the binding behind the config's back: publishes through the
        // exchange become unroutable while verify used to stay green.
        managementRequest(
            'DELETE',
            "http://localhost:15672/api/bindings/{$vhostUrl}/e/{$this->exchangeName}/q/{$this->queueName}/{$routeBinding()[0]['properties_key']}",
        );

        $topology()
            ->expectsOutputToContain("binding '{$this->exchangeName}' -> '{$this->queueName}'")
            ->assertExitCode(1);

        // Declare-mode --fix re-declares the route binding (the lab's
        // deleted-binding scenario, bug #14 follow-up).
        $topology(['--fix' => true])->assertExitCode(0);
        expect($routeBinding())->not->toBeEmpty('route binding must be re-declared by --fix');
    });
});
