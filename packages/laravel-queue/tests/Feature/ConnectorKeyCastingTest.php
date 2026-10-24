<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;

/**
 * Reads a connector-cast framework key back off a resolved queue. The
 * properties are private/protected by design; reflection is the
 * established test seam for compiled connector state.
 */
function castedFrameworkKey(object $queue, string $property): mixed
{
    // @phpstan-ignore-next-line — intentionally accessing a private property for test verification.
    return (new ReflectionProperty($queue, $property))->getValue($queue);
}

beforeEach(function (): void {
    bootFakeNativeExtension($this->app);
});

describe('connector framework key casting', function () {
    it('casts block_for and after_commit from env-style strings', function () {
        config()->set('queue.connections.envy', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'block_for' => '3',
            'after_commit' => '1',
        ]);

        $queue = $this->app->make('queue')->connection('envy');

        expect($queue)->toBeInstanceOf(RabbitMqQueue::class)
            ->and(castedFrameworkKey($queue, 'blockForMilliseconds'))->toBe(3000)
            ->and(castedFrameworkKey($queue, 'dispatchAfterCommit'))->toBeTrue();
    });

    it('names the config path when block_for is not an integer', function () {
        config()->set('queue.connections.envy', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'block_for' => 'soon',
        ]);

        expect(fn () => $this->app->make('queue')->connection('envy'))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.envy.block_for');
    });

    it('names the config path when after_commit is not a boolean', function () {
        config()->set('queue.connections.envy', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'after_commit' => 'maybe',
        ]);

        expect(fn () => $this->app->make('queue')->connection('envy'))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.envy.after_commit');
    });
});
