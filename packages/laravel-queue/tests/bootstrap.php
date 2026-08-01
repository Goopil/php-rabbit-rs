<?php

declare(strict_types=1);

namespace {
    require dirname(__DIR__).'/vendor/autoload.php';
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

        final class Pool
        {
            /** @var list<array<string, mixed>> */
            public array $published = [];

            /** @var list<list<array<string, mixed>>> */
            public array $publishedBatches = [];

            private ?\Throwable $nextPublishException = null;

            /**
             * @param array<string, mixed> $config
             */
            public function __construct(public readonly array $config = []) {}

            public function throwOnNextPublish(\Throwable $exception): void
            {
                $this->nextPublishException = $exception;
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

            private function throwPendingException(): void
            {
                if ($this->nextPublishException === null) {
                    return;
                }

                $exception = $this->nextPublishException;
                $this->nextPublishException = null;

                throw $exception;
            }
        }
    }
}