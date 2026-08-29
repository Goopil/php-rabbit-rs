<?php

declare(strict_types=1);

describe('secret redaction', function () {
    it('does not expose the password in validation errors', function () {
        $password = 'native-password-must-stay-secret';

        $config = defaultConfig();
        $config['brokers'][0]['hosts'][0]['port'] = 'not-a-port';
        $config['brokers'][0]['credentials']['password'] = $password;

        expect(fn () => new \Goopil\RabbitRs\Pool($config))->toThrow(
            fn (\Goopil\RabbitRs\Exception $e) => expect($e->getMessage())->not->toContain($password),
        );
    });

    it('does not expose private key material in validation errors', function () {
        $privateKey = 'PRIVATE-KEY-MATERIAL';

        $config = defaultConfig();
        $config['brokers'][0]['hosts'][0]['port'] = 'not-a-port';
        $config['brokers'][0]['tls'] = [
            'enabled' => true,
            'server_name' => 'rabbit.internal',
            'private_key' => $privateKey,
        ];

        expect(fn () => new \Goopil\RabbitRs\Pool($config))->toThrow(
            fn (\Goopil\RabbitRs\Exception $e) => expect($e->getMessage())->not->toContain($privateKey),
        );
    });
});
