<?php

declare(strict_types=1);

use Bench\ResultMeta;

describe('ResultMeta::config', function () {
    it('surfaces the broker and benchmark configuration', function () {
        $config = ResultMeta::config(payloadBytes: 1024);

        expect($config)->toHaveKeys([
            'rabbitmq',
            'message_count',
            'rounds',
            'warmup_rounds',
            'payload_bytes',
        ])
            ->and($config['rabbitmq']['host'])->toBe('127.0.0.1')
            ->and($config['rabbitmq']['port'])->toBe(5672)
            ->and($config['rabbitmq']['vhost'])->toBe('/')
            ->and($config['payload_bytes'])->toBe(1024);
    });

    it('never exposes credentials', function () {
        $config = ResultMeta::config();

        expect($config['rabbitmq'])->not->toHaveKey('password')
            ->and($config['rabbitmq']['user'])->toBe('***')
            ->and(json_encode($config))->not->toContain(\Bench\Config::RABBITMQ_PASSWORD);
    });
});

describe('ResultMeta::meta', function () {
    it('surfaces the runtime environment', function () {
        $meta = ResultMeta::meta();

        expect($meta)->toHaveKeys(['php', 'sapi', 'os', 'extensions'])
            ->and($meta['php'])->toBe(PHP_VERSION)
            ->and($meta['sapi'])->toBe(PHP_SAPI)
            ->and($meta['os'])->toBe(PHP_OS.' '.php_uname('r'))
            ->and($meta['extensions'])->toHaveKeys(['rabbit_rs', 'amqp']);
    });
});
