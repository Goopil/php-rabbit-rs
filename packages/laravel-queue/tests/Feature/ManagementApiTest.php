<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\ManagementApi;
use Illuminate\Support\Facades\Http;

const MGMT_API_URL = 'http://mq.local:15672';

beforeEach(function () {
    config()->set('queue.connections.rabbit-rs', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'vhost' => 'staging',
        'username' => 'worker',
        'password' => 'secret',
        'management_url' => MGMT_API_URL,
    ]);
});

describe('ManagementApi queue depth', function () {
    it('reads messages_ready from the management api', function () {
        Http::fake([
            MGMT_API_URL.'/api/queues/*' => Http::response([
                'messages' => 7,
                'messages_ready' => 5,
                'messages_unacknowledged' => 2,
            ]),
        ]);

        expect(ManagementApi::queueDepth('rabbit-rs', 'default'))->toBe(5);
    });

    it('requests the vhost-encoded queue endpoint with the connection credentials', function () {
        Http::fake(['*' => Http::response(['messages_ready' => 1])]);

        ManagementApi::queueDepth('rabbit-rs', 'orders');

        Http::assertSent(function ($request) {
            return $request->method() === 'GET'
                && $request->url() === MGMT_API_URL.'/api/queues/staging/orders'
                && $request->header('Authorization')[0] !== null;
        });
    });

    it('encodes special characters in the vhost and queue name', function () {
        config()->set('queue.connections.rabbit-rs.vhost', 'my/vhost');
        Http::fake(['*' => Http::response(['messages_ready' => 1])]);

        ManagementApi::queueDepth('rabbit-rs', 'jobs/eu');

        Http::assertSent(
            fn ($request) => $request->url() === MGMT_API_URL.'/api/queues/my%2Fvhost/jobs%2Feu',
        );
    });

    it('returns null when management_url is not configured', function () {
        config()->set('queue.connections.rabbit-rs.management_url', null);
        Http::fake(['*' => Http::response(['messages_ready' => 1])]);

        expect(ManagementApi::queueDepth('rabbit-rs', 'default'))->toBeNull();

        Http::assertNothingSent();
    });

    it('returns null when the connection is unknown', function () {
        expect(ManagementApi::queueDepth('nope', 'default'))->toBeNull();
    });

    it('returns null when the management api fails', function () {
        Http::fake(['*' => Http::response('down', 500)]);

        expect(ManagementApi::queueDepth('rabbit-rs', 'default'))->toBeNull();
    });

    it('returns null when the response carries no depth gauge', function () {
        Http::fake(['*' => Http::response([])]);

        expect(ManagementApi::queueDepth('rabbit-rs', 'default'))->toBeNull();
    });
});
