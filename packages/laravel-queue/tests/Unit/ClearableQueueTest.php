<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Illuminate\Contracts\Queue\ClearableQueue;

describe('ClearableQueue contract', function () {
    beforeEach(function (): void {
        $this->app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
        ]);
        (new class($this->app) extends RabbitMqServiceProvider {
            protected function nativeExtensionLoaded(): bool
            {
                return true;
            }
        })->boot();
    });

    it('implements ClearableQueue so queue:clear works', function (): void {
        expect($this->app->make('queue')->connection('rabbit-rs'))
            ->toBeInstanceOf(Illuminate\Contracts\Queue\Queue::class)
            ->toBeInstanceOf(ClearableQueue::class);
    });
});
