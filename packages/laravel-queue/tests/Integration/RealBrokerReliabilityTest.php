<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use PHPUnit\Framework\Assert;

/** Owns a single per-test proxy and refuses to act on an unknown lab. */
final class ReliabilityProxy
{
    private const API = 'http://127.0.0.1:18474';

    private function __construct(public readonly string $name, public readonly int $port) {}

    /** @return array{int, string} */
    private static function request(string $method, string $path, ?array $payload = null): array
    {
        $curl = curl_init(self::API.$path);
        curl_setopt_array($curl, [
            CURLOPT_CUSTOMREQUEST => $method,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_TIMEOUT => 5,
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

    public static function create(): self
    {
        [$status, $body] = self::request('GET', '/proxies/rabbitmq-1');
        Assert::assertSame(200, $status, 'The lab fingerprint proxy must be reachable');
        $fingerprint = json_decode($body, true, flags: JSON_THROW_ON_ERROR);
        Assert::assertSame('rabbitmq-1:5672', $fingerprint['upstream'] ?? null, 'Refusing a foreign proxy');

        $name = 'reliability-'.bin2hex(random_bytes(8));
        foreach (range(24504, 24509) as $port) {
            [$status] = self::request('POST', '/proxies', [
                'name' => $name,
                'listen' => '0.0.0.0:'.$port,
                'upstream' => 'rabbitmq-1:5672',
                'enabled' => true,
            ]);
            if ($status === 200 || $status === 201) {
                return new self($name, $port);
            }
        }

        Assert::fail('No private Toxiproxy listen port was available');
    }

    public function addReset(): void
    {
        [$status] = self::request('POST', '/proxies/'.$this->name.'/toxics', [
            'name' => 'test-reset',
            'type' => 'reset_peer',
            'stream' => 'downstream',
            'toxicity' => 1.0,
            'attributes' => ['timeout' => 0],
        ]);
        Assert::assertSame(200, $status, 'Reset toxic must be applied');
        [$status, $body] = self::request('GET', '/proxies/'.$this->name.'/toxics/test-reset');
        Assert::assertSame(200, $status, 'Reset toxic must be observable');
        $toxic = json_decode($body, true, flags: JSON_THROW_ON_ERROR);
        Assert::assertSame('reset_peer', $toxic['type'] ?? null);
    }

    public function removeReset(): void
    {
        [$status] = self::request('DELETE', '/proxies/'.$this->name.'/toxics/test-reset');
        Assert::assertSame(204, $status, 'Reset toxic must be removed');
        [$status] = self::request('GET', '/proxies/'.$this->name.'/toxics/test-reset');
        Assert::assertSame(404, $status, 'Removed toxic must be absent');
    }

    public function addTimeout(string $name, string $stream): void
    {
        [$status] = self::request('POST', '/proxies/'.$this->name.'/toxics', [
            'name' => $name,
            'type' => 'timeout',
            'stream' => $stream,
            'toxicity' => 1.0,
            'attributes' => ['timeout' => 100],
        ]);
        Assert::assertSame(200, $status, 'Timeout toxic must be applied');
        [$status] = self::request('GET', '/proxies/'.$this->name.'/toxics/'.$name);
        Assert::assertSame(200, $status, 'Timeout toxic must be observable');
    }

    public function removeTimeout(string $name): void
    {
        [$status] = self::request('DELETE', '/proxies/'.$this->name.'/toxics/'.$name);
        Assert::assertSame(204, $status, 'Timeout toxic must be removed');
        [$status] = self::request('GET', '/proxies/'.$this->name.'/toxics/'.$name);
        Assert::assertSame(404, $status, 'Removed timeout must be absent');
    }

    public function close(): void
    {
        [$status] = self::request('DELETE', '/proxies/'.$this->name);
        Assert::assertSame(204, $status, 'Owned proxy must be deleted');
        [$status] = self::request('GET', '/proxies/'.$this->name);
        Assert::assertSame(404, $status, 'Deleted proxy must be absent');
    }
}

/** Creates and deletes only the uniquely named queue owned by this test. */
final class ReliabilityQueue
{
    /** @return array{int, string} */
    private static function request(string $method, string $queueName, ?array $payload = null): array
    {
        $url = 'http://127.0.0.1:15672/api/queues/'.rawurlencode(ORDERS_VHOST).'/'.rawurlencode($queueName);
        $curl = curl_init($url);
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

    public static function create(string $queueName, bool &$owned): void
    {
        [$status] = self::request('GET', $queueName);
        Assert::assertSame(404, $status, 'The test queue name must not already exist');
        [$status] = self::request('PUT', $queueName, [
            'durable' => true,
            'arguments' => ['x-queue-type' => 'quorum'],
        ]);
        $owned = $status === 201;
        Assert::assertSame(201, $status, 'The test-owned queue must be created');
    }

    public static function delete(string $queueName): void
    {
        [$status] = self::request('DELETE', $queueName);
        Assert::assertSame(204, $status, 'The test-owned queue must be deleted');
        [$status] = self::request('GET', $queueName);
        Assert::assertSame(404, $status, 'The test-owned queue must be absent');
    }
}

it('publishes a confirmed message through an isolated lab proxy', function () {
    if (! extension_loaded('rabbit_rs')) {
        $this->fail('ext-rabbit_rs is required for real-broker reliability tests');
    }

    $queueName = uniqueQueue('rabbit-rs-it-reliability');
    $proxy = ReliabilityProxy::create();
    $pool = null;
    $queueOwned = false;
    try {
        ReliabilityQueue::create($queueName, $queueOwned);
        [$pool, $queue] = integrationPoolAndQueue(
            $this->app,
            $queueName,
            connectOverrides: ['block_for' => 1],
            connectionName: 'rabbit-rs-reliability-'.$proxy->name,
            brokerHosts: ['127.0.0.1:'.$proxy->port],
        );

        $id = $queue->push('stdClass', ['run' => $proxy->name]);
        $pool->flush();
        $job = $queue->pop();
        expect($job)->toBeInstanceOf(RabbitMqJob::class)
            ->and($job->getJobId())->toBe($id)
            ->and(json_decode($job->getRawBody(), true, flags: JSON_THROW_ON_ERROR)['data']['run'])->toBe($proxy->name);
        $job->delete();
        $proxy->addReset();
        $proxy->removeReset();
    } finally {
        try {
            if ($pool !== null) {
                $pool->close();
            }
        } finally {
            try {
                if ($queueOwned) {
                    ReliabilityQueue::delete($queueName);
                }
            } finally {
                $proxy->close();
            }
        }
    }
});

it('cuts and restores a real AMQP connection using its own proxy', function () {
    $proxy = ReliabilityProxy::create();
    $socket = null;
    $open = static function () use ($proxy) {
        $socket = stream_socket_client('tcp://127.0.0.1:'.$proxy->port, timeout: 3);
        Assert::assertNotFalse($socket, 'Proxy must accept a TCP connection');
        stream_set_timeout($socket, 3);
        Assert::assertSame(8, fwrite($socket, "AMQP\x00\x00\x09\x01"));
        Assert::assertNotSame('', fread($socket, 7), 'Broker must reply to the AMQP handshake');

        return $socket;
    };
    try {
        $socket = $open();
        $proxy->addTimeout('cut-up', 'upstream');
        $proxy->addTimeout('cut-down', 'downstream');
        $deadline = microtime(true) + 10;
        do {
            fread($socket, 8192);
        } while (! feof($socket) && microtime(true) < $deadline);
        expect(feof($socket))->toBeTrue('The injected fault must close the established connection');
        fclose($socket);
        $socket = null;
        $proxy->removeTimeout('cut-up');
        $proxy->removeTimeout('cut-down');
        $socket = $open();
    } finally {
        if (is_resource($socket)) {
            fclose($socket);
        }
        $proxy->close();
    }
});

it('recovers a pool after a proxy outage and delivers an outage-buffered publication', function () {
    if (! extension_loaded('rabbit_rs')) {
        $this->fail('ext-rabbit_rs is required for real-broker reliability tests');
    }

    $queueName = uniqueQueue('rabbit-rs-it-recovery');
    $proxy = ReliabilityProxy::create();
    $pool = null;
    $upstreamCut = false;
    $downstreamCut = false;
    $queueOwned = false;
    try {
        ReliabilityQueue::create($queueName, $queueOwned);
        [$pool, $queue] = integrationPoolAndQueue(
            $this->app,
            $queueName,
            configOverrides: ['flush_interval' => 3_600_000],
            connectOverrides: ['block_for' => 1],
            connectionName: 'rabbit-rs-recovery-'.$proxy->name,
            brokerHosts: ['127.0.0.1:'.$proxy->port],
        );
        $states = [];
        $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$states): void {
            $states[] = [$broker, $state, $generation];
        });

        $beforeId = $queue->push('stdClass', ['phase' => 'before']);
        $pool->flush();
        $baseline = $pool->stats()['reconnects_total'];
        expect(array_column($states, 1))->toContain('ready');
        $faultStart = count($states);

        $proxy->addTimeout('recovery-up', 'upstream');
        $upstreamCut = true;
        $proxy->addTimeout('recovery-down', 'downstream');
        $downstreamCut = true;
        $deadline = microtime(true) + 15;
        do {
            $pool->stats();
            if (array_intersect(array_column(array_slice($states, $faultStart), 1), ['recovering', 'connecting', 'disconnected']) !== []) {
                break;
            }
            usleep(100_000);
        } while (microtime(true) < $deadline);
        expect(array_intersect(array_column(array_slice($states, $faultStart), 1), ['recovering', 'connecting', 'disconnected']))
            ->not->toBeEmpty('The pool must observe the injected connection failure');

        $duringId = $queue->push('stdClass', ['phase' => 'during']);
        expect($duringId)->not->toBe($beforeId, 'The two publications must have distinct IDs');
        $proxy->removeTimeout('recovery-up');
        $upstreamCut = false;
        $proxy->removeTimeout('recovery-down');
        $downstreamCut = false;
        $deadline = microtime(true) + 20;
        do {
            $reconnects = $pool->stats()['reconnects_total'];
            if ($reconnects > $baseline && in_array('ready', array_column(array_slice($states, $faultStart), 1), true)) {
                break;
            }
            usleep(100_000);
        } while (microtime(true) < $deadline);
        expect($reconnects)->toBeGreaterThan($baseline, 'The pool must reconnect after the proxy is restored');
        expect(array_column(array_slice($states, $faultStart), 1))->toContain('ready');

        $pool->flush();
        $deadline = microtime(true) + 15;
        $expected = [$beforeId => 'before', $duringId => 'during'];
        $seen = [];
        do {
            $job = $queue->pop();
            if ($job instanceof RabbitMqJob) {
                $id = $job->getJobId();
                expect($expected)->toHaveKey($id);
                expect(json_decode($job->getRawBody(), true, flags: JSON_THROW_ON_ERROR)['data']['phase'])->toBe($expected[$id]);
                $seen[$id] = true;
                $job->delete();
            }
        } while (count($seen) < count($expected) && microtime(true) < $deadline);
        expect(array_keys($seen))->toContain($beforeId, $duringId);
    } finally {
        try {
            if ($upstreamCut) {
                $proxy->removeTimeout('recovery-up');
            }
        } finally {
            try {
                if ($downstreamCut) {
                    $proxy->removeTimeout('recovery-down');
                }
            } finally {
                try {
                    if ($pool !== null) {
                        $pool->close();
                    }
                } finally {
                    try {
                        if ($queueOwned) {
                            ReliabilityQueue::delete($queueName);
                        }
                    } finally {
                        $proxy->close();
                    }
                }
            }
        }
    }
});

it('redelivers an unacknowledged message after an established consumer loses its connection', function () {
    if (! extension_loaded('rabbit_rs')) {
        $this->fail('ext-rabbit_rs is required for real-broker reliability tests');
    }

    $queueName = uniqueQueue('rabbit-rs-it-consumer-recovery');
    $proxy = ReliabilityProxy::create();
    $pool = null;
    $queueOwned = false;
    $upstreamCut = false;
    $downstreamCut = false;
    try {
        ReliabilityQueue::create($queueName, $queueOwned);
        [$pool, $queue] = integrationPoolAndQueue(
            $this->app,
            $queueName,
            connectOverrides: ['block_for' => 1],
            connectionName: 'rabbit-rs-consumer-recovery-'.$proxy->name,
            brokerHosts: ['127.0.0.1:'.$proxy->port],
        );
        $states = [];
        $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$states): void {
            $states[] = [$broker, $state, $generation];
        });
        $id = $queue->push('stdClass', ['phase' => 'unacknowledged']);
        $pool->flush();
        $held = $queue->pop();
        expect($held)->toBeInstanceOf(RabbitMqJob::class)
            ->and($held->getJobId())->toBe($id)
            ->and($held->attempts())->toBe(1);
        $baseline = $pool->stats()['reconnects_total'];
        $faultStart = count($states);

        $proxy->addTimeout('consumer-up', 'upstream');
        $upstreamCut = true;
        $proxy->addTimeout('consumer-down', 'downstream');
        $downstreamCut = true;
        $deadline = microtime(true) + 15;
        do {
            $pool->stats();
            if (array_intersect(array_column(array_slice($states, $faultStart), 1), ['recovering', 'connecting', 'disconnected']) !== []) {
                break;
            }
            usleep(100_000);
        } while (microtime(true) < $deadline);
        expect(array_intersect(array_column(array_slice($states, $faultStart), 1), ['recovering', 'connecting', 'disconnected']))
            ->not->toBeEmpty('The pool must observe the injected connection failure');
        $proxy->removeTimeout('consumer-up');
        $upstreamCut = false;
        $proxy->removeTimeout('consumer-down');
        $downstreamCut = false;

        $deadline = microtime(true) + 20;
        do {
            $reconnects = $pool->stats()['reconnects_total'];
            if ($reconnects > $baseline && in_array('ready', array_column(array_slice($states, $faultStart), 1), true)) {
                break;
            }
            usleep(100_000);
        } while (microtime(true) < $deadline);
        expect($reconnects)->toBeGreaterThan($baseline);
        expect(array_column(array_slice($states, $faultStart), 1))->toContain('ready');

        $deadline = microtime(true) + 15;
        $redelivered = null;
        $transientErrors = 0;
        do {
            try {
                $redelivered = $queue->pop();
            } catch (ConnectionException $exception) {
                $transientErrors++;
                if ($transientErrors > 5) {
                    throw $exception;
                }
            }
        } while ($redelivered === null && microtime(true) < $deadline);
        expect($redelivered)->toBeInstanceOf(RabbitMqJob::class)
            ->and($redelivered->getJobId())->toBe($id)
            ->and($redelivered->attempts())->toBeGreaterThan(1)
            ->and(json_decode($redelivered->getRawBody(), true, flags: JSON_THROW_ON_ERROR)['data']['phase'])->toBe('unacknowledged');
        $redelivered->delete();
    } finally {
        try {
            if ($upstreamCut) {
                $proxy->removeTimeout('consumer-up');
            }
        } finally {
            try {
                if ($downstreamCut) {
                    $proxy->removeTimeout('consumer-down');
                }
            } finally {
                try {
                    if ($pool !== null) {
                        $pool->close();
                    }
                } finally {
                    try {
                        if ($queueOwned) {
                            ReliabilityQueue::delete($queueName);
                        }
                    } finally {
                        $proxy->close();
                    }
                }
            }
        }
    }
});
