<?php

declare(strict_types=1);

use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Pool;
use Illuminate\Queue\Events\JobQueueing;
use Illuminate\Support\Facades\Event;

/**
 * The native publishBatch bounds the chunking must respect. They mirror
 * conversion::MAX_BATCH_MESSAGES / MAX_BATCH_PAYLOAD_BYTES in the PHP
 * extension: a native call beyond either bound is rejected wholesale,
 * before anything is sent.
 */
const BULK_CHUNK_MAX_MESSAGES = 256;
const BULK_CHUNK_MAX_PAYLOAD_BYTES = 1024 * 1024;

/**
 * @return array{RabbitMqQueue, Pool}
 */
function bulkChunkQueue(): array
{
    $pool = new Pool;
    $queue = new RabbitMqQueue($pool, [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'bulk.jobs',
            'routing_key' => '{queue}',
        ],
    ], 'default');
    $queue->setContainer(app());

    return [$queue, $pool];
}

/**
 * Cumulative payload bytes a recorded batch carries, measured the way the
 * native conversion budget counts them: strlen of every message payload.
 *
 * @param  list<array<string, mixed>>  $batch
 */
function batchPayloadBytes(array $batch): int
{
    return array_sum(array_map(
        static fn (array $message): int => strlen((string) $message['payload']),
        $batch,
    ));
}

/**
 * @return list<string>
 */
function bulkChunkJobs(int $count): array
{
    return array_map(
        static fn (int $index): string => "App\\Jobs\\BulkChunked{$index}",
        range(0, $count - 1),
    );
}

it('chunks bulk publishes beyond the native batch message limit', function (): void {
    [$queue, $pool] = bulkChunkQueue();

    $messageIds = $queue->bulk(bulkChunkJobs(300));

    // 300 immediate jobs need two native calls: 256 + 44 (chunk 2 holds the
    // remainder) — a single call would be rejected wholesale by the native
    // message-count bound.
    expect($pool->publishedBatches)->toHaveCount(2)
        ->and(count($pool->publishedBatches[0]))->toBe(BULK_CHUNK_MAX_MESSAGES)
        ->and(count($pool->publishedBatches[1]))->toBe(44)
        ->and(count(array_merge(...$pool->publishedBatches)))->toBe(300) // nothing lost
        ->and($messageIds)->toBe(array_map( // ids stay in job order across chunks
            static fn (array $message): string => $message['message_id'],
            array_merge(...$pool->publishedBatches),
        ));
});

it('chunks bulk by cumulative payload below 1 MiB per chunk', function (): void {
    [$queue, $pool] = bulkChunkQueue();

    // Three ~400 KiB payloads: two stay under the 1 MiB cumulative bound,
    // the third would cross it, so the chunk splits 2 + 1 — far below the
    // 256-message bound, proving the payload bound alone drove the split.
    $queue->bulk(['App\\Jobs\\BulkA', 'App\\Jobs\\BulkB', 'App\\Jobs\\BulkC'], [
        'pad' => str_repeat('a', 400 * 1024),
    ]);

    expect($pool->publishedBatches)->toHaveCount(2)
        ->and(count($pool->publishedBatches[0]))->toBe(2)
        ->and(count($pool->publishedBatches[1]))->toBe(1)
        ->and(batchPayloadBytes($pool->publishedBatches[0]))->toBeLessThanOrEqual(BULK_CHUNK_MAX_PAYLOAD_BYTES)
        ->and(batchPayloadBytes($pool->publishedBatches[1]))->toBeLessThanOrEqual(BULK_CHUNK_MAX_PAYLOAD_BYTES);
});

it('surfaces the chunk failure after earlier chunks were published', function (): void {
    [$queue, $pool] = bulkChunkQueue();

    // Arm the native failure as the second chunk's first message is queued:
    // JobQueueing fires per message before the chunk's native call, so
    // chunk 1 (jobs 0..255) is already published at that point.
    Event::listen(JobQueueing::class, function (JobQueueing $event) use ($pool): void {
        if ($event->job === 'App\\Jobs\\BulkChunked256') {
            $pool->throwOnNextPublish(new NativeException(
                'publisher transport failed during the batch publication',
            ));
        }
    });

    try {
        $queue->bulk(bulkChunkJobs(300));
        self::fail('The second-chunk failure was not surfaced.');
    } catch (QueueException $exception) {
        expect($exception->getMessage())->toContain('transport failed');
    }

    // Chunk 1's publications remain — documented at-least-once partial
    // success: the caller retry re-publishes every job and the stable
    // message_id keeps the duplicates identifiable. The fake records a
    // batch before throwing (it does not model native atomicity), so the
    // failed chunk 2 also appears at index 1; index 0 is what the broker
    // accepted.
    $chunkOne = $pool->publishedBatches[0];
    $chunkOneIds = array_map(
        static fn (array $message): string => $message['message_id'],
        $chunkOne,
    );

    expect(count($chunkOne))->toBe(BULK_CHUNK_MAX_MESSAGES)
        ->and(count(array_unique($chunkOneIds)))->toBe(BULK_CHUNK_MAX_MESSAGES); // intact, no loss or duplication
    foreach ($chunkOne as $message) {
        $payload = json_decode((string) $message['payload'], true, flags: JSON_THROW_ON_ERROR);
        expect($payload['uuid'])->toBe($message['message_id'])
            ->and('bulk.jobs')->toBe($message['exchange']);
    }
});
