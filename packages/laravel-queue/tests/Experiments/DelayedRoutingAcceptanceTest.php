<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use PHPUnit\Framework\Assert;

final class DelayExperimentManagement
{
    /** @return array{int, string} */
    public static function request(string $method, string $path, ?array $payload = null): array
    {
        $curl = curl_init('http://127.0.0.1:15672'.$path);
        curl_setopt_array($curl, [
            CURLOPT_CUSTOMREQUEST => $method,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_TIMEOUT => 5,
            CURLOPT_USERPWD => 'admin:admin_lab',
        ]);
        if ($payload !== null) {
            curl_setopt($curl, CURLOPT_POSTFIELDS, json_encode($payload, JSON_THROW_ON_ERROR));
            curl_setopt($curl, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
        }
        $body = curl_exec($curl);
        $status = curl_getinfo($curl, CURLINFO_RESPONSE_CODE);
        curl_close($curl);

        return [$status, $body === false ? '' : $body];
    }

    /** @return array<string, mixed> */
    public static function binding(string $exchange, string $queue): array
    {
        $vhost = rawurlencode(ORDERS_VHOST);
        [$status, $body] = self::request('GET', '/api/exchanges/'.$vhost.'/'.rawurlencode($exchange).'/bindings/source');
        Assert::assertSame(200, $status, 'The delayed exchange must exist');
        $bindings = json_decode($body, true, flags: JSON_THROW_ON_ERROR);
        $matching = array_values(array_filter($bindings, static fn (array $binding): bool => ($binding['destination'] ?? null) === $queue && ($binding['routing_key'] ?? null) === $queue));
        Assert::assertCount(1, $matching, 'The exact test-owned delayed binding must exist');

        return $matching[0];
    }

    /** @return list<array<string, mixed>> */
    public static function buckets(string $exchange): array
    {
        [$status, $body] = self::request('GET', '/api/queues/'.rawurlencode(ORDERS_VHOST));
        Assert::assertSame(200, $status, 'The management queue listing must be available');
        $queues = json_decode($body, true, flags: JSON_THROW_ON_ERROR);

        return array_values(array_filter($queues, static fn (array $queue): bool => str_starts_with($queue['name'] ?? '', 'rabbit-rs.delay.')
            && ($queue['arguments']['x-dead-letter-exchange'] ?? null) === $exchange));
    }
}

it('retains a confirmed delayed publication while its binding is temporarily absent', function (string $mode) {
    if (! extension_loaded('rabbit_rs')) {
        $this->fail('ext-rabbit_rs is required for this experiment');
    }
    $queueName = uniqueQueue('rabbit-rs-it-delay-accept');
    $exchange = uniqueQueue('rabbit-rs-it-delay-exchange');
    $delayedExchange = $exchange.'.delayed';
    $usesTtl = str_starts_with($mode, 'ttl');
    $policyName = $queueName.'-policy';
    $pool = null;
    try {
        grantRabbitRsConfigure();
        [$pool, $queue] = integrationPoolAndQueue(
            $this->app,
            $queueName,
            configOverrides: [
                'exchange' => $exchange,
                'routing_key' => '{queue}',
                'delay' => ['mode' => $usesTtl ? 'ttl' : 'plugin'],
            ],
            connectOverrides: ['block_for' => 1],
            connectionName: 'rabbit-rs-delay-acceptance',
        );
        $controlId = $queue->later(4, 'stdClass', ['control' => true]);
        $pool->flush();
        $vhost = rawurlencode(ORDERS_VHOST);
        $targetExchange = $usesTtl ? $exchange : $delayedExchange;
        if ($mode === 'plugin') {
            [$status] = DelayExperimentManagement::request('POST', '/api/bindings/'.$vhost.'/e/'
                .rawurlencode($targetExchange).'/q/'.rawurlencode($queueName), [
                    'routing_key' => $queueName,
                    'arguments' => new stdClass,
                ]);
            expect($status)->toBe(201, 'The explicit control binding must be created');
        }
        DelayExperimentManagement::binding($targetExchange, $queueName);
        $deadline = microtime(true) + 12;
        $control = null;
        do {
            $control = $queue->pop();
        } while ($control === null && microtime(true) < $deadline);
        expect($control)->toBeInstanceOf(RabbitMqJob::class)
            ->and($control->getJobId())->toBe($controlId);
        $control->delete();

        if ($mode === 'ttl_at_least_once') {
            $buckets = DelayExperimentManagement::buckets($exchange);
            expect($buckets)->toHaveCount(1);
            [$status] = DelayExperimentManagement::request('PUT', '/api/policies/'.$vhost.'/'.rawurlencode($policyName), [
                'pattern' => '^'.preg_quote($buckets[0]['name'], '/').'$',
                'definition' => ['dead-letter-strategy' => 'at-least-once', 'overflow' => 'reject-publish'],
                'priority' => 100,
                'apply-to' => 'quorum_queues',
            ]);
            expect($status)->toBeIn([201, 204], 'The test-scoped at-least-once policy must be accepted');
            $deadline = microtime(true) + 10;
            do {
                $buckets = DelayExperimentManagement::buckets($exchange);
                if (($buckets[0]['effective_policy_definition']['dead-letter-strategy'] ?? null) === 'at-least-once'
                    && ($buckets[0]['effective_policy_definition']['overflow'] ?? null) === 'reject-publish') {
                    break;
                }
                usleep(100_000);
            } while (microtime(true) < $deadline);
            expect($buckets[0]['effective_policy_definition']['dead-letter-strategy'] ?? null)->toBe('at-least-once')
                ->and($buckets[0]['effective_policy_definition']['overflow'] ?? null)->toBe('reject-publish');
        }

        $binding = DelayExperimentManagement::binding($targetExchange, $queueName);
        $id = $queue->later(4, 'stdClass', ['experiment' => $queueName]);
        $pool->flush();
        fwrite(STDERR, 'Confirmed experiment publication: '
            .json_encode(['mode' => $mode, 'message_id' => $id, 'queue' => $queueName], JSON_THROW_ON_ERROR).PHP_EOL);
        if ($usesTtl) {
            $buckets = DelayExperimentManagement::buckets($exchange);
            expect($buckets)->toHaveCount(1)
                ->and($buckets[0]['arguments']['x-message-ttl'] ?? null)->toBe(5000);
        }
        $path = '/api/bindings/'.$vhost.'/e/'.rawurlencode($targetExchange).'/q/'
            .rawurlencode($queueName).'/'.rawurlencode($binding['properties_key']);
        [$status] = DelayExperimentManagement::request('DELETE', $path);
        expect($status)->toBe(204, 'The delayed binding must be removed');
        [$status, $body] = DelayExperimentManagement::request(
            'GET', '/api/exchanges/'.$vhost.'/'.rawurlencode($targetExchange).'/bindings/source',
        );
        expect($status)->toBe(200)
            ->and(array_column(json_decode($body, true, flags: JSON_THROW_ON_ERROR), 'destination'))
            ->not->toContain($queueName);

        // Let the confirmed publish reach its due time while the binding is
        // absent, then restore only this test-owned binding through topology.
        usleep(7_000_000);
        if ($usesTtl) {
            $observed = DelayExperimentManagement::buckets($exchange);
            fwrite(STDERR, 'TTL bucket state before binding restoration: '
                .json_encode($observed, JSON_THROW_ON_ERROR).PHP_EOL);
        }
        [$status] = DelayExperimentManagement::request('POST', '/api/bindings/'.$vhost.'/e/'
            .rawurlencode($targetExchange).'/q/'.rawurlencode($queueName), [
                'routing_key' => $queueName,
                'arguments' => new stdClass,
            ]);
        expect($status)->toBe(201);

        $observationStarted = microtime(true);
        $deadline = $observationStarted + ($mode === 'ttl_at_least_once' ? 90 : 10);
        $received = null;
        $bucketDisappearanceReported = false;
        do {
            $received = $queue->pop();
            if ($mode === 'ttl_at_least_once' && ! $bucketDisappearanceReported
                && DelayExperimentManagement::buckets($exchange) === []) {
                $bucketDisappearanceReported = true;
                fwrite(STDERR, 'TTL bucket disappeared '
                    .round(microtime(true) - $observationStarted, 3)
                    .' seconds after binding restoration'.PHP_EOL);
            }
        } while ($received === null && microtime(true) < $deadline);
        expect($received)->toBeInstanceOf(RabbitMqJob::class)
            ->and($received->getJobId())->toBe($id);
        $received->delete();
    } finally {
        if ($pool !== null) {
            $pool->close();
        }
        $vhost = rawurlencode(ORDERS_VHOST);
        if ($usesTtl) {
            // RabbitMQ 4.2 rejects conditional deletion of quorum queues.
            // Retain the owned topology so pending work cannot be deleted.
            fwrite(STDERR, 'Retained TTL experiment resources: '
                .json_encode(['queue' => $queueName, 'exchange' => $exchange, 'policy' => $mode === 'ttl_at_least_once' ? $policyName : null], JSON_THROW_ON_ERROR).PHP_EOL);
        } else {
            deleteQueue($queueName);
            DelayExperimentManagement::request('DELETE', '/api/exchanges/'.$vhost.'/'.rawurlencode($exchange));
            DelayExperimentManagement::request('DELETE', '/api/exchanges/'.$vhost.'/'.rawurlencode($delayedExchange));
        }
    }
})->with(['plugin', 'ttl', 'ttl_at_least_once']);

it('observes custom delayed routing in declare mode before and after external binding', function () {
    if (! extension_loaded('rabbit_rs')) {
        $this->fail('ext-rabbit_rs is required for this experiment');
    }
    $queueName = uniqueQueue('rabbit-rs-it-custom-delay');
    $exchange = uniqueQueue('rabbit-rs-it-custom-exchange');
    $delayedExchange = $exchange.'.delayed';
    $vhost = rawurlencode(ORDERS_VHOST);
    $pool = null;
    try {
        grantRabbitRsConfigure();
        [$pool, $queue] = integrationPoolAndQueue(
            $this->app,
            $queueName,
            configOverrides: [
                'exchange' => $exchange,
                'routing_key' => '{queue}',
                'delay' => ['mode' => 'plugin'],
            ],
            connectOverrides: ['block_for' => 1],
            connectionName: 'rabbit-rs-custom-delay-evidence',
        );
        $id = $queue->later(5, 'stdClass', ['control' => true]);
        $pool->flush();
        [$status, $body] = DelayExperimentManagement::request('GET', '/api/exchanges/'.$vhost.'/'.rawurlencode($delayedExchange).'/bindings/source');
        expect($status)->toBe(200);
        $bindings = json_decode($body, true, flags: JSON_THROW_ON_ERROR);
        $matching = array_values(array_filter($bindings, static fn (array $binding): bool => ($binding['destination'] ?? null) === $queueName));
        expect($matching)->toBe([], 'Declare mode did not provision a binding for this custom delayed exchange');

        [$status] = DelayExperimentManagement::request('POST', '/api/bindings/'.$vhost.'/e/'.rawurlencode($delayedExchange).'/q/'.rawurlencode($queueName), [
            'routing_key' => $queueName,
            'arguments' => new stdClass,
        ]);
        expect($status)->toBe(201);
        DelayExperimentManagement::binding($delayedExchange, $queueName);
        $deadline = microtime(true) + 12;
        $job = null;
        do {
            $job = $queue->pop();
        } while ($job === null && microtime(true) < $deadline);
        expect($job)->toBeInstanceOf(RabbitMqJob::class)
            ->and($job->getJobId())->toBe($id);
        $job->delete();
    } finally {
        if ($pool !== null) {
            $pool->close();
        }
        deleteQueue($queueName);
        DelayExperimentManagement::request('DELETE', '/api/exchanges/'.$vhost.'/'.rawurlencode($exchange));
        DelayExperimentManagement::request('DELETE', '/api/exchanges/'.$vhost.'/'.rawurlencode($delayedExchange));
    }
});
