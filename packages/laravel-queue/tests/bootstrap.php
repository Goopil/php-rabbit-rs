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