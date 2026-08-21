<?php

declare(strict_types=1);

namespace {
    require dirname(__DIR__).'/vendor/autoload.php';
}

namespace Laravel\Octane {
    if (! class_exists(Octane::class, false)) {
        final class Octane {}
    }
}

namespace Laravel\Octane\Events {
    if (! class_exists(WorkerReload::class, false)) {
        final class WorkerReload {}
    }

    if (! class_exists(WorkerStopping::class, false)) {
        final class WorkerStopping {}
    }
}

namespace Goopil\RabbitRs {
    if (! class_exists(Pool::class, false)) {
        class Exception extends \Exception {}

        final class BackpressureException extends Exception {}

        final class ConnectionException extends Exception {}

        final class Delivery
        {
            public int $ackCalls = 0;

            /** @var list<int> */
            public array $releaseDelays = [];

            /** @var list<bool> */
            public array $rejectRequeues = [];

            private bool $settled = false;

            private ?\Throwable $nextAckException = null;

            private ?\Closure $ackCallback = null;

            /**
             * @param array<string, mixed> $metadata
             */
            public function __construct(
                private readonly string $body,
                private array $metadata,
            ) {}

            public function payload(): string
            {
                return $this->body;
            }

            /**
             * @return array<string, mixed>
             */
            public function metadata(): array
            {
                return $this->metadata;
            }

            public function onAck(\Closure $callback): void
            {
                $this->ackCallback = $callback;
            }

            public function throwOnNextAck(\Throwable $exception): void
            {
                $this->nextAckException = $exception;
            }

            public function ack(): void
            {
                $this->assertPending();
                $this->ackCalls++;

                if ($this->ackCallback !== null) {
                    ($this->ackCallback)();
                }

                if ($this->nextAckException !== null) {
                    $exception = $this->nextAckException;
                    $this->nextAckException = null;

                    throw $exception;
                }

                $this->settled = true;
                $this->metadata['state'] = 'acked';
            }

            public function release(int $delayMs = 0): void
            {
                $this->assertPending();
                $this->releaseDelays[] = $delayMs;
                $this->settled = true;
                $this->metadata['state'] = $delayMs === 0 ? 'rejected' : 'acked';
            }

            public function reject(bool $requeue = false): void
            {
                $this->assertPending();
                $this->rejectRequeues[] = $requeue;
                $this->settled = true;
                $this->metadata['state'] = 'rejected';
            }

            private function assertPending(): void
            {
                if ($this->settled) {
                    throw new Exception('delivery is already settled');
                }
            }
        }

        final class Consumer
        {
            /** @var list<int> */
            public array $timeouts = [];

            public int $closeCalls = 0;

            /** @var list<Delivery> */
            private array $deliveries = [];

            private ?\Throwable $nextException = null;

            private bool $closed = false;

            public function push(Delivery $delivery): void
            {
                $this->deliveries[] = $delivery;
            }

            public function throwOnNext(\Throwable $exception): void
            {
                $this->nextException = $exception;
            }

            public function next(int $timeoutMs): ?Delivery
            {
                if ($this->closed) {
                    throw new Exception('consumer is closed');
                }
                if ($timeoutMs < 0) {
                    throw new Exception('timeoutMs must be a non-negative integer');
                }

                $this->timeouts[] = $timeoutMs;
                if ($this->nextException !== null) {
                    $exception = $this->nextException;
                    $this->nextException = null;

                    throw $exception;
                }

                return array_shift($this->deliveries);
            }

            public function close(): void
            {
                $this->closeCalls++;
                $this->closed = true;
            }
        }

        final class Pool
        {
            /** @var list<array<string, mixed>> */
            public array $published = [];

            /** @var list<list<array<string, mixed>>> */
            public array $publishedBatches = [];

            /** @var list<string> */
            public array $consumerProfiles = [];

            /** @var list<array{broker: string, queue: string}> */
            public array $sizeCalls = [];

            /** @var list<array{broker: string, queue: string}> */
            public array $clearCalls = [];

            /** @var array<string, int> */
            public array $sizeResults = [];

            /** @var array<string, mixed>|null */
            public ?array $statsResult = null;

            private ?\Throwable $nextPublishException = null;

            private ?\Throwable $nextSizeException = null;

            private ?\Throwable $nextClearException = null;

            /** @var array<string, Consumer> */
            private array $consumers = [];

            /** @var ?\Closure(string, string, int): void */
            private ?\Closure $connectionStateCallback = null;

            /** @var ?\Closure(string, int, int): void */
            private ?\Closure $backpressureCallback = null;

            /**
             * @param array<string, mixed> $config
             */
            public function __construct(public readonly array $config = []) {}

            public function throwOnNextPublish(\Throwable $exception): void
            {
                $this->nextPublishException = $exception;
            }

            public function throwOnNextSize(\Throwable $exception): void
            {
                $this->nextSizeException = $exception;
            }

            public function throwOnNextClear(\Throwable $exception): void
            {
                $this->nextClearException = $exception;
            }

            /**
             * @param array<string, mixed> $message
             */
            public function publish(array $message): string
            {
                $this->published[] = $message;
                $this->throwPendingException();

                return $message['message_id'];
            }

            /**
             * @param list<array<string, mixed>> $messages
             * @return list<string>
             */
            public function publishBatch(array $messages): array
            {
                $this->publishedBatches[] = $messages;
                $this->throwPendingException();

                return array_column($messages, 'message_id');
            }

            public function pushDelivery(string $profile, Delivery $delivery): void
            {
                $this->configuredConsumer($profile)->push($delivery);
            }

            public function consumer(string $profile): Consumer
            {
                $this->consumerProfiles[] = $profile;

                return $this->configuredConsumer($profile);
            }

            public function consumerFor(string $profile): Consumer
            {
                return $this->configuredConsumer($profile);
            }

            public function size(string $broker, string $queue): int
            {
                $this->sizeCalls[] = ['broker' => $broker, 'queue' => $queue];

                if ($this->nextSizeException !== null) {
                    $exception = $this->nextSizeException;
                    $this->nextSizeException = null;

                    throw $exception;
                }

                $key = "{$broker}:{$queue}";

                return $this->sizeResults[$key] ?? 0;
            }

            public function clear(string $broker, string $queue): void
            {
                $this->clearCalls[] = ['broker' => $broker, 'queue' => $queue];

                if ($this->nextClearException !== null) {
                    $exception = $this->nextClearException;
                    $this->nextClearException = null;

                    throw $exception;
                }
            }

            /**
             * Registers a PHP callback invoked when the connection state changes.
             *
             * @param \Closure(string, string, int): void $callback
             */
            public function onConnectionState(\Closure $callback): void
            {
                $this->connectionStateCallback = $callback;
            }

            /**
             * Registers a PHP callback invoked when backpressure is detected.
             *
             * @param \Closure(string, int, int): void $callback
             */
            public function onBackpressure(\Closure $callback): void
            {
                $this->backpressureCallback = $callback;
            }

            /**
             * Simulates a connection state change and invokes the registered callback.
             */
            public function simulateConnectionState(string $broker, string $state, int $generation): void
            {
                if ($this->connectionStateCallback !== null) {
                    ($this->connectionStateCallback)($broker, $state, $generation);
                }
            }

            /**
             * Simulates a backpressure event and invokes the registered callback.
             */
            public function simulateBackpressure(string $broker, int $inFlight, int $capacity): void
            {
                if ($this->backpressureCallback !== null) {
                    ($this->backpressureCallback)($broker, $inFlight, $capacity);
                }
            }

            /**
             * @return array<string, mixed>
             */
            public function stats(): array
            {
                return $this->statsResult ?? [
                    'closed' => false,
                    'pid' => 12345,
                    'handle' => 'conn:019f8f1a',
                    'publishes_total' => 100,
                    'confirmations_total' => 98,
                    'returns_total' => 2,
                    'backpressure_total' => 0,
                    'reconnects_total' => 1,
                    'deliveries_total' => 50,
                    'acks_total' => 48,
                    'rejects_total' => 2,
                    'confirmation_latency_p50' => 12,
                    'confirmation_latency_p95' => 45,
                    'confirmation_latency_p99' => 120,
                    'settlement_latency_p50' => 8,
                    'settlement_latency_p95' => 30,
                    'settlement_latency_p99' => 85,
                ];
            }

            private function throwPendingException(): void
            {
                if ($this->nextPublishException === null) {
                    return;
                }

                $exception = $this->nextPublishException;
                $this->nextPublishException = null;

                throw $exception;
            }

            private function configuredConsumer(string $profile): Consumer
            {
                foreach ($this->config['workers'] ?? [] as $worker) {
                    if (($worker['name'] ?? null) === $profile) {
                        if (! isset($this->consumers[$profile]) || $this->consumers[$profile]->closeCalls > 0) {
                            $this->consumers[$profile] = new Consumer();
                        }

                        return $this->consumers[$profile];
                    }
                }

                throw new Exception("workers.{$profile}: unknown worker profile");
            }
        }
    }
}