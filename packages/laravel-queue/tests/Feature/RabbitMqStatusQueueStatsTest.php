<?php

declare(strict_types=1);

use Illuminate\Support\Facades\Http;

const STATUS_QUEUE_JSON_COMMAND = 'rabbit-rs:status --format=json';
const STATUS_MGMT_URL = 'http://mq.local:15672';

beforeEach(function () {
    config()->set('queue.connections.rabbit-rs', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'vhost' => '/',
        'username' => 'guest',
        'password' => 'guest',
        'management_url' => STATUS_MGMT_URL,
    ]);
});

describe('status command queue stats', function () {
    it('json output includes queue counters from the management api', function () {
        Http::fake([
            STATUS_MGMT_URL.'/api/queues/*' => Http::response([
                'messages_delivered' => 10,
                'messages_acked' => 8,
                'messages_redelivered' => 2,
            ]),
        ]);

        $this->artisan(STATUS_QUEUE_JSON_COMMAND)
            ->assertSuccessful()
            ->expectsOutputToContain('"management_url_configured": true')
            ->expectsOutputToContain('"messages_delivered": 10')
            ->expectsOutputToContain('"messages_acked": 8')
            ->expectsOutputToContain('"messages_redelivered": 2');

        Http::assertSent(
            fn ($request) => $request->method() === 'GET'
                && str_starts_with($request->url(), STATUS_MGMT_URL.'/api/queues/'),
        );
    });

    it('requests the vhost-encoded queue endpoint', function () {
        Http::fake(['*' => Http::response([])]);

        $this->artisan(STATUS_QUEUE_JSON_COMMAND)->assertSuccessful();

        Http::assertSent(
            fn ($request) => $request->url() === STATUS_MGMT_URL.'/api/queues/%2F/default',
        );
    });

    it('defaults missing management counters to zero', function () {
        Http::fake(['*' => Http::response([])]);

        $this->artisan(STATUS_QUEUE_JSON_COMMAND)
            ->assertSuccessful()
            ->expectsOutputToContain('"messages_delivered": 0')
            ->expectsOutputToContain('"messages_acked": 0')
            ->expectsOutputToContain('"messages_redelivered": 0');
    });

    it('degrades gracefully when the management api fails', function () {
        Http::fake(['*' => Http::response(['error' => 'boom'], 500)]);

        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('unavailable');
    });

    it('human output shows cross-process queue metrics', function () {
        Http::fake(['*' => Http::response([
            'messages_delivered' => 10,
            'messages_acked' => 8,
            'messages_redelivered' => 2,
        ])]);

        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('Queue Metrics')
            ->expectsOutputToContain('rabbit-rs/default')
            ->expectsOutputToContain('redelivered');
    });

    it('human output states that redeliveries also count crash requeues', function () {
        Http::fake(['*' => Http::response([])]);

        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('crash requeues');
    });

    it('human output reports when the management url is not configured', function () {
        config()->set('queue.connections.rabbit-rs.management_url', null);
        Http::fake(['*' => Http::response([])]);

        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('management url not configured');

        Http::assertNothingSent();
    });

    it('json output reports when the management url is not configured', function () {
        config()->set('queue.connections.rabbit-rs.management_url', null);
        Http::fake(['*' => Http::response([])]);

        $this->artisan(STATUS_QUEUE_JSON_COMMAND)
            ->assertSuccessful()
            ->expectsOutputToContain('"management_url_configured": false');

        Http::assertNothingSent();
    });

    it('labels native pool metrics as same-process only', function () {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('same-process only');
    });
});
