<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Http;
use RuntimeException;

/**
 * Environment probes for rabbit-rs:doctor. Resolved from the container so
 * test suites running without ext-rabbit_rs (and without a broker) can bind
 * a fake: the real broker probe performs an AMQP connection.
 */
class DoctorProbe
{
    /**
     * Readiness wait for the declare probe, in milliseconds. The compiled
     * config emits consumer.wait_timeout in milliseconds (see
     * ConnectionCompiler::consumer()), mirroring the core's ms deserialization.
     */
    public const DECLARE_READINESS_TIMEOUT_MS = 2_000;

    /** Upper bound on the canary-DLQ verification poll (attempts × interval). */
    private const CANARY_DLQ_ATTEMPTS = 30;

    private const CANARY_DLQ_POLL_MS = 300_000;

    /** Bulk DLQ scan window, in messages, per verification attempt. */
    private const CANARY_DLQ_SCAN_WINDOW = 100;

    private const CANARY_DLQ_PREFIX = 'rabbit-rs.canary.';

    /**
     * Doctor-owned DLQ for the canary: bound to the configured DLX with the
     * dead-letter routing key, so a copy of every dead-lettered message lands
     * here. The doctor declares it before the check and purges+deletes it
     * after, so canaries and foreign traffic can never wall off the verdict
     * (issue #288).
     */
    private function canaryDlqName(string $broker, string $exchange, string $routingKey): string
    {
        return self::CANARY_DLQ_PREFIX.substr(hash('sha256', $broker.'|'.$exchange.'|'.$routingKey), 0, 16);
    }

    public function extensionLoaded(): bool
    {
        return extension_loaded('rabbit_rs');
    }

    public function extensionVersion(): ?string
    {
        $version = phpversion('rabbit_rs');

        return $version === false ? null : $version;
    }

    /**
     * Connects to the broker described by the compiled native config and
     * returns the error message, or null when the connection succeeded
     * (reachability, auth, and vhost are validated by the native client).
     *
     * The pool construction is lazy, so size() is the call that actually
     * touches the broker; the probe pool is closed immediately afterwards.
     *
     * @param  array<string, mixed>  $nativeConfig
     */
    public function broker(array $nativeConfig): ?string
    {
        return $this->probePool($nativeConfig, function (Pool $pool) use ($nativeConfig): void {
            $pool->size(self::brokerName($nativeConfig), self::queueName($nativeConfig));
        });
    }

    /**
     * Checks queue existence with a passive probe (`Pool::size()`), returning
     * the native error message when the queue is missing (AMQP NOT-FOUND) or
     * the broker cannot be reached, and null when it exists.
     *
     * @param  array<string, mixed>  $nativeConfig
     */
    public function queueSize(array $nativeConfig, string $broker, string $queue): ?string
    {
        return $this->probePool($nativeConfig, function (Pool $pool) use ($broker, $queue): void {
            $pool->size($broker, $queue);
        });
    }

    /**
     * Declares the worker profile's topology by opening and closing a
     * transient consumer: the native connection brings up connection,
     * channels, exchanges, queues, bindings and consumers in recovery order,
     * which in declare mode creates anything missing. Returns the error
     * message, or null when the declaration succeeded.
     *
     * The probe runs against {@see declareConfig()}, which bounds the
     * consumer readiness wait to {@see DECLARE_READINESS_TIMEOUT_MS}: the
     * declaration lands before consumer readiness in the native bring-up
     * order, and the topology command's warn path covers the
     * readiness-unconfirmed case. Waiting for the full compiled timeout
     * (30 s by default) would stall `rabbit-rs:topology --fix` whenever no
     * worker consumes (lab report bug 11 remainder).
     *
     * @param  array<string, mixed>  $nativeConfig
     */
    public function declareTopology(array $nativeConfig, string $workerProfile): ?string
    {
        return $this->probePool(self::declareConfig($nativeConfig), function (Pool $pool) use ($workerProfile): void {
            $consumer = $pool->consumer($workerProfile);
            $consumer->close();
        });
    }

    /**
     * @internal exposed for tests; use declareTopology() instead.
     *
     * Returns a copy of the native config whose consumer readiness wait is
     * bounded to {@see DECLARE_READINESS_TIMEOUT_MS}, leaving every other
     * key untouched. Hand-written probe configs may omit the consumer
     * section (or carry a non-array value), in which case a minimal one is
     * created on the copy.
     *
     * @param  array<string, mixed>  $nativeConfig
     * @return array<string, mixed>
     */
    public static function declareConfig(array $nativeConfig): array
    {
        $probed = $nativeConfig;
        $consumer = $probed['consumer'] ?? null;
        if (! is_array($consumer)) {
            $consumer = [];
        }

        $consumer['wait_timeout'] = self::DECLARE_READINESS_TIMEOUT_MS;
        $probed['consumer'] = $consumer;

        return $probed;
    }

    /**
     * Dead-letter canary: publishes a uniquely marked probe into the
     * connection's main queue, consumes it, rejects it terminally, and
     * asserts the broker dead-letters it. This is a behavioral check with
     * real broker traffic — it exercises queue args → DLX → binding → DLQ,
     * which static topology checks cannot prove.
     *
     * Verification is tiered and doctor-owned (#288): before the check the
     * doctor declares a dedicated canary DLQ ({@see canaryDlqName()}) bound
     * to the configured DLX with the dead-letter routing key, and purges and
     * deletes it in a finally block afterwards. The canary therefore always
     * lands in a queue no backlog can wall off, and nothing accumulates:
     * found on the configured DLQ → ok; found only in the canary DLQ (the
     * configured DLQ's backlog outgrew the 100-message scan window) →
     * inconclusive with the foreign count; never reaching the canary DLQ →
     * hard failure.
     *
     * Foreign messages encountered in the main queue are released untouched
     * (never acked, never dropped); on a configured-DLQ backlog the
     * verification scans a bounded window in bulk and stays passive towards
     * foreign traffic. Most meaningful in quiet windows: a running worker on
     * the same queue can pick the probe up first — that case is reported as
     * inconclusive, not as a dead-letter failure.
     *
     * Returns null when the canary was delivered, or the thrown exception so
     * the caller can distinguish a genuine dead-letter failure from an
     * inconclusive run (CanaryInconclusiveException).
     *
     * @param  array<string, mixed>  $nativeConfig
     * @param  array<string, mixed>  $config  connection config (management_url, credentials)
     */
    public function deadLetterCanary(
        array $nativeConfig,
        string $broker,
        string $exchange,
        string $routingKey,
        string $dlq,
        string $workerProfile,
        array $config,
        bool $competingConsumersExpected = false,
    ): ?RuntimeException {
        $url = $config['management_url'] ?? null;
        if (! is_string($url) || trim($url) === '') {
            return new RuntimeException('no management_url configured — the canary cannot verify DLQ delivery');
        }

        $username = is_string($config['username'] ?? null) ? $config['username'] : 'guest';
        $password = is_string($config['password'] ?? null) ? $config['password'] : 'guest';
        $base = rtrim(trim($url), '/');
        $vhost = is_string($nativeConfig['brokers'][0]['vhost'] ?? null) ? $nativeConfig['brokers'][0]['vhost'] : '/';

        // The canary DLQ must be bound to the configured DLX with the
        // effective dead-letter routing key — the same key the main queue's
        // x-dead-letter-routing-key carries (configured routing_key, or the
        // subscription queue name when unset, mirroring the core's topology
        // plan) — so the broker's dead-letter republish reaches both queues.
        $deadLetter = is_array($nativeConfig['dead_letter'] ?? null) ? $nativeConfig['dead_letter'] : [];
        $dlx = is_string($deadLetter['exchange'] ?? null) ? $deadLetter['exchange'] : '';
        $deadLetterRoutingKey = is_string($deadLetter['routing_key'] ?? null)
            ? $deadLetter['routing_key']
            : self::queueName($nativeConfig);
        $canaryDlq = $this->canaryDlqName($broker, $dlx, $deadLetterRoutingKey);
        $canaryDlqUrl = "{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($canaryDlq).'/get';

        $pool = new Pool(self::declareConfig($nativeConfig));
        $messageId = '';

        try {
            $mainQueue = self::queueName($nativeConfig);
            $messageId = 'doctor-dlx-canary-'.bin2hex(random_bytes(8));

            try {
                $this->declareCanaryDlq($base, $vhost, $canaryDlq, $dlx, $deadLetterRoutingKey, $username, $password);
            } catch (\Throwable $e) {
                // A canary DLQ that cannot be declared or bound can never
                // receive the canary, so the wiring verdict is fail with the
                // decisive cause — not the raw transport error's shape.
                throw new RuntimeException('dead-lettered canary never reached the DLX — the dead-letter wiring is broken (the canary DLQ could not be declared or bound: '.$e->getMessage().'), so dead-lettered messages would vanish', 0, $e);
            }

            $pool->publish([
                'broker' => $broker,
                'exchange' => $exchange,
                'routing_key' => $routingKey,
                'payload' => 'rabbit-rs doctor dead-letter canary',
                'message_id' => $messageId,
                'headers' => ['x-canary' => 'rabbit-rs-doctor'],
                'timeout_ms' => 5000,
            ]);
            // size() is a synchronous flush barrier: the buffered canary is
            // confirmed (or its failure raised) before the consume loop starts.
            $pool->size($broker, $mainQueue);

            $rejected = false;
            $foreign = 0;
            $consumer = $pool->consumer($workerProfile);
            try {
                $deadline = microtime(true) + 10.0;
                while (microtime(true) < $deadline) {
                    $delivery = $consumer->next(2000);
                    if ($delivery === null) {
                        continue;
                    }
                    if (($delivery->metadata()['message_id'] ?? '') === $messageId) {
                        $delivery->reject(false); // terminal reject → dead-lettered to the DLQ
                        $rejected = true;

                        break;
                    }
                    $delivery->release(); // foreign traffic: never acked, never dropped
                    $foreign++;
                }

                // Verify while the consumer is still open: the reject is a
                // fire-and-forget settlement, and closing the consumer before
                // it reaches the wire would requeue the delivery instead of
                // dead-lettering it.
                if ($rejected) {
                    $this->assertCanaryOnDlq($base, $vhost, $username, $password, $messageId, $dlq, $canaryDlqUrl);
                }
            } finally {
                $consumer->close();
            }

            if (! $rejected) {
                if ($foreign > 0) {
                    // Real consumers beat the probe to the queue: the wiring
                    // was not exercised, and that is an environment property,
                    // not a dead-letter failure.
                    throw new CanaryInconclusiveException(
                        "canary never reached this consumer — competing consumers processed {$foreign} message(s) first; re-run in a quiet window",
                    );
                }

                if ($competingConsumersExpected) {
                    // Running workers (e.g. Horizon) can consume the probe
                    // outright — the doctor's consumer sees no foreign
                    // traffic at all. Still an environment property.
                    throw new CanaryInconclusiveException(
                        'canary never reached this consumer — running consumers on this connection likely claimed the probe; pause the workers and re-run',
                    );
                }

                throw new RuntimeException('canary message was not consumed from the main queue within 10s');
            }
        } catch (RuntimeException $e) {
            return $e;
        } catch (\Throwable $e) {
            return new RuntimeException($e->getMessage(), 0, $e);
        } finally {
            $pool->close();
            $this->teardownCanaryDlq($base, $vhost, $canaryDlq, $username, $password);
        }

        return null;
    }

    /**
     * Tiered DLQ verification for the rejected canary (#288).
     *
     * Tier 1 polls the doctor-owned canary DLQ — declared fresh before the
     * check and purged+deleted afterwards, so no backlog can wall its window
     * off. The 30-attempt × 300 ms poll reuses the bulk pull; on exhaustion
     * the verdict is a hard failure: a DLX that routes nothing is exactly
     * the dead-letter loss this canary exists to catch, and the old
     * inconclusive-on-backlog could mask it behind a deep configured-DLQ
     * backlog.
     *
     * Tier 2 adds the configured-DLQ evidence with a single 100-message
     * `ack_requeue_true` window (pure inspection: every message is requeued
     * untouched, foreign traffic included). Finding the canary there proves
     * the full chain onto the *configured* DLQ; missing it there means the
     * backlog outgrew the scan — the wiring is verified, the configured-DLQ
     * delivery is not, and that state is inconclusive with the foreign count,
     * not a failure.
     */
    private function assertCanaryOnDlq(
        string $base,
        string $vhost,
        string $username,
        string $password,
        string $messageId,
        string $configuredDlq,
        string $canaryDlqUrl,
    ): void {
        $configuredDlqUrl = "{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($configuredDlq).'/get';

        // Tier 1: the canary must reach the doctor-owned canary DLQ — the DLX
        // is alive and routed. This queue is purged+deleted each run, so the
        // 100-message window can never be walled off by a backlog (#288).
        $foundInCanaryDlq = false;
        for ($attempt = 0; $attempt < self::CANARY_DLQ_ATTEMPTS && ! $foundInCanaryDlq; $attempt++) {
            $messages = $this->pullFromQueue($canaryDlqUrl, $username, $password, 'ack_requeue_true', self::CANARY_DLQ_SCAN_WINDOW);
            foreach ($messages as $message) {
                $candidate = is_array($message['properties'] ?? null) ? (string) ($message['properties']['message_id'] ?? '') : '';
                if ($candidate === $messageId) {
                    $foundInCanaryDlq = true;

                    break;
                }
            }

            if (! $foundInCanaryDlq) {
                usleep(self::CANARY_DLQ_POLL_MS);
            }
        }

        if (! $foundInCanaryDlq) {
            throw new RuntimeException('dead-lettered canary never reached the DLX — the dead-letter wiring is broken (the canary DLQ was empty), so dead-lettered messages would vanish');
        }

        // Tier 2: configured-DLQ evidence (single 100-message window, requeued).
        $foreign = 0;
        $messages = $this->pullFromQueue($configuredDlqUrl, $username, $password, 'ack_requeue_true', self::CANARY_DLQ_SCAN_WINDOW);
        foreach ($messages as $message) {
            $candidate = is_array($message['properties'] ?? null) ? (string) ($message['properties']['message_id'] ?? '') : '';
            if ($candidate === $messageId) {
                return; // [ok] full chain proven on the configured DLQ
            }
            $foreign++;
        }

        throw new CanaryInconclusiveException(
            'wiring verified through the canary DLQ, but the canary sits deeper than the '.self::CANARY_DLQ_SCAN_WINDOW
            .'-message scan window of the configured DLQ ('.$foreign.' foreign/stale messages at its head) — dead-letter delivery to the configured DLQ unverified'
        );
    }

    /**
     * Declares the canary DLQ and binds it to the configured DLX with the
     * dead-letter routing key, through the management API. A direct exchange
     * routes a copy of every matching message to every bound queue, so the
     * canary reaches this queue alongside the configured DLQ — and because
     * the doctor purges+deletes it after every run, its window can never be
     * walled off by accumulated traffic (#288).
     */
    private function declareCanaryDlq(
        string $base,
        string $vhost,
        string $canaryDlq,
        string $dlx,
        string $routingKey,
        string $username,
        string $password,
    ): void {
        Http::withBasicAuth($username, $password)
            ->put("{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($canaryDlq), [
                'durable' => true,
                'auto_delete' => false,
                'arguments' => ['x-queue-type' => 'quorum'],
            ])->throw();
        Http::withBasicAuth($username, $password)
            ->post("{$base}/api/bindings/".rawurlencode($vhost).'/e/'.rawurlencode($dlx).'/q/'.rawurlencode($canaryDlq), [
                'routing_key' => $routingKey,
            ])->throw();
    }

    /**
     * Purges and deletes the canary DLQ. Best-effort hygiene: every failure
     * is swallowed so the teardown can never mask the check's verdict — a
     * surviving queue is cleaned up by the next run's declare+teardown.
     */
    private function teardownCanaryDlq(string $base, string $vhost, string $canaryDlq, string $username, string $password): void
    {
        try {
            Http::withBasicAuth($username, $password)
                ->delete("{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($canaryDlq).'/contents')->throw();
        } catch (\Throwable) {
            // best-effort hygiene; never mask the check's verdict
        }
        try {
            Http::withBasicAuth($username, $password)
                ->delete("{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($canaryDlq))->throw();
        } catch (\Throwable) {
            // same
        }
    }

    /**
     * Pulls up to `$count` messages from a queue via the management API with
     * the given ack mode (`ack_requeue_true` inspects without consuming,
     * `ack_requeue_false` removes everything pulled).
     *
     * @return list<array<string, mixed>>
     */
    private function pullFromQueue(string $url, string $username, string $password, string $ackMode, int $count): array
    {
        $response = Http::withBasicAuth($username, $password)
            ->timeout(5)
            ->acceptJson()
            ->post($url, ['count' => $count, 'ackmode' => $ackMode, 'encoding' => 'auto', 'truncate' => 50_000]);

        if (! $response->successful()) {
            throw new RuntimeException('management api returned HTTP '.$response->status()." reading the DLQ ({$ackMode})");
        }

        $messages = $response->json();

        return is_array($messages) ? $messages : [];
    }

    /**
     * Runs one probe against a transient pool and reports the native error
     * message, or null when the probe succeeded. The pool is always closed.
     *
     * @param  array<string, mixed>  $nativeConfig
     * @param  callable(Pool): void  $probe
     */
    private function probePool(array $nativeConfig, callable $probe): ?string
    {
        try {
            $pool = new Pool($nativeConfig);
            try {
                $probe($pool);
            } finally {
                $pool->close();
            }
        } catch (\Throwable $e) {
            return $e->getMessage();
        }

        return null;
    }

    /**
     * @param  array<string, mixed>  $nativeConfig
     */
    private static function brokerName(array $nativeConfig): string
    {
        return $nativeConfig['brokers'][0]['name'] ?? 'default';
    }

    /**
     * @param  array<string, mixed>  $nativeConfig
     */
    private static function queueName(array $nativeConfig): string
    {
        return $nativeConfig['workers'][0]['subscriptions'][0]['queue'] ?? 'default';
    }
}
