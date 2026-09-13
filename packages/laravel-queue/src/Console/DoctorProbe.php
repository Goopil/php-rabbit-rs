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

    /** Upper bound on the canary's DLQ verification loop (attempts × interval). */
    private const CANARY_DLQ_ATTEMPTS = 30;

    private const CANARY_DLQ_POLL_MS = 300_000;

    /** Bulk DLQ scan window, in messages, per verification attempt. */
    private const CANARY_DLQ_SCAN_WINDOW = 100;

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
     * asserts the broker dead-letters it into the configured DLQ. This is a
     * behavioral check with real broker traffic — it exercises queue args →
     * DLX → binding → DLQ, which static topology checks cannot prove.
     *
     * Foreign messages encountered in the main queue are released untouched
     * (never acked, never dropped); on a DLQ backlog the verification scans
     * a bounded window in bulk and stays passive towards foreign traffic.
     * Most meaningful in quiet windows: a running worker on the same queue
     * can pick the probe up first — that case is reported as inconclusive,
     * not as a dead-letter failure.
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
    ): ?RuntimeException {
        $pool = new Pool(self::declareConfig($nativeConfig));
        $messageId = '';

        try {
            $mainQueue = self::queueName($nativeConfig);
            $messageId = 'doctor-dlx-canary-'.bin2hex(random_bytes(8));

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
                    $vhost = is_string($nativeConfig['brokers'][0]['vhost'] ?? null) ? $nativeConfig['brokers'][0]['vhost'] : '/';
                    $this->assertCanaryOnDlq($config, $vhost, $dlq, $messageId);
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

                throw new RuntimeException('canary message was not consumed from the main queue within 10s');
            }
        } finally {
            $pool->close();
        }

        return null;
    }

    /**
     * Asserts the canary reached the DLQ via the management API. Each
     * attempt scans a bounded window in bulk with `ack_requeue_true` (pure
     * inspection: every message is requeued untouched, foreign traffic
     * included) and passes when the canary appears anywhere in the batch —
     * position is irrelevant, so a DLQ backlog cannot hide it. The canary
     * itself is deliberately left in the DLQ: removing it would take an
     * `ack_requeue_false` pull, which drops every other message in the
     * window.
     *
     * When no attempt finds the canary, the verdict depends on coverage:
     * fewer messages returned than the window means the whole DLQ was seen
     * and the canary is genuinely gone (hard failure); a full window means
     * the backlog outgrew the scan and the run is inconclusive.
     *
     * @param  array<string, mixed>  $config
     */
    private function assertCanaryOnDlq(array $config, string $vhost, string $dlq, string $messageId): void
    {
        $url = $config['management_url'] ?? null;
        if (! is_string($url) || trim($url) === '') {
            throw new RuntimeException('no management_url configured — the canary cannot verify DLQ delivery');
        }

        $username = is_string($config['username'] ?? null) ? $config['username'] : 'guest';
        $password = is_string($config['password'] ?? null) ? $config['password'] : 'guest';
        $base = rtrim(trim($url), '/');
        $queueUrl = "{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($dlq).'/get';

        for ($attempt = 0; $attempt < self::CANARY_DLQ_ATTEMPTS; $attempt++) {
            $messages = $this->pullFromQueue($queueUrl, $username, $password, 'ack_requeue_true', self::CANARY_DLQ_SCAN_WINDOW);
            foreach ($messages as $message) {
                $candidate = is_array($message['properties'] ?? null) ? (string) ($message['properties']['message_id'] ?? '') : '';
                if ($candidate === $messageId) {
                    return;
                }
            }

            if (count($messages) >= self::CANARY_DLQ_SCAN_WINDOW) {
                // A full window is a wall of foreign backlog at the head;
                // re-reading it can never reveal the canary behind it, and
                // requeuing preserved its position. Inconclusive, not failed.
                throw new CanaryInconclusiveException('canary not located within the '.self::CANARY_DLQ_SCAN_WINDOW.'-message DLQ scan window — the backlog outgrew the scan; dead-letter delivery unverified');
            }

            // The whole DLQ was visible: keep polling for the canary's async
            // arrival, then report a genuine absence.
            usleep(self::CANARY_DLQ_POLL_MS);
        }

        throw new RuntimeException('canary message not found in DLQ — the whole queue was scanned and the dead-lettered canary is gone');
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
