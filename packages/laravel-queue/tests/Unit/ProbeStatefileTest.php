<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\ProbeStatefile;

describe('ProbeStatefile writer', function () {
    it('writes a booting statefile with the documented JSON shape', function () {
        $dir = probeTempDir();
        $writer = new ProbeStatefile($dir, 123);

        $writer->heartbeat(42, 40, 2);

        $data = json_decode((string) file_get_contents($dir.'/123.json'), true);
        expect($data)->toMatchArray([
            'pid' => 123,
            'state' => 'booting',
            'connected' => true,
            'consumed' => 42,
            'acked' => 40,
            'nacked' => 2,
        ]);
    });

    it('flips to running after the first completed loop turn', function () {
        $dir = probeTempDir();
        $writer = new ProbeStatefile($dir, 123, heartbeatSeconds: 0);

        $writer->heartbeat(0, 0, 0);
        $writer->markRunning();
        $writer->heartbeat(5, 5, 0);

        $data = json_decode((string) file_get_contents($dir.'/123.json'), true);
        expect($data['state'])->toBe('running')
            ->and($data['consumed'])->toBe(5)
            ->and($data['acked'])->toBe(5)
            ->and($data['nacked'])->toBe(0);
    });

    it('throttles heartbeats to one write per window', function () {
        $dir = probeTempDir();
        $writer = new ProbeStatefile($dir, 123, heartbeatSeconds: 3600);

        $writer->heartbeat(1, 1, 0);
        $writer->heartbeat(2, 2, 0);

        $data = json_decode((string) file_get_contents($dir.'/123.json'), true);
        expect($data['consumed'])->toBe(1)
            ->and($data['acked'])->toBe(1);
    });

    it('reports due again once the heartbeat window elapsed', function () {
        $writer = new ProbeStatefile(probeTempDir(), 123, heartbeatSeconds: 3600);

        expect($writer->due())->toBeTrue();

        $writer->heartbeat(0, 0, 0);

        expect($writer->due())->toBeFalse();
    });

    it('writes immediately on connection state transitions', function () {
        $dir = probeTempDir();
        $writer = new ProbeStatefile($dir, 123, heartbeatSeconds: 3600);

        $writer->heartbeat(1, 1, 0);
        $writer->recordConnectionState('broker', 'recovering');

        $data = json_decode((string) file_get_contents($dir.'/123.json'), true);
        expect($data['connected'])->toBeFalse();
    });

    it('reports connected only while every observed broker is ready', function () {
        $dir = probeTempDir();
        $writer = new ProbeStatefile($dir, 123, heartbeatSeconds: 0);

        $writer->recordConnectionState('a', 'ready');
        $writer->recordConnectionState('b', 'ready');
        $writer->heartbeat(0, 0, 0);

        expect(json_decode((string) file_get_contents($dir.'/123.json'), true)['connected'])->toBeTrue();

        $writer->recordConnectionState('b', 'connecting');
        $writer->heartbeat(0, 0, 0);

        expect(json_decode((string) file_get_contents($dir.'/123.json'), true)['connected'])->toBeFalse();
    });

    it('writes draining on shutdown even inside the heartbeat window', function () {
        $dir = probeTempDir();
        $writer = new ProbeStatefile($dir, 123, heartbeatSeconds: 3600);

        $writer->heartbeat(0, 0, 0);
        $writer->draining();

        expect(json_decode((string) file_get_contents($dir.'/123.json'), true)['state'])->toBe('draining');
    });

    it('never lets write failures escape', function () {
        $file = probeTempDir().'/not-a-directory';
        file_put_contents($file, 'x');
        $writer = new ProbeStatefile($file, 123);

        expect(fn () => $writer->heartbeat(0, 0, 0))->not->toThrow(Throwable::class);
    });
});

describe('ProbeStatefile reader', function () {
    it('returns fresh statefiles with normalized fields', function () {
        $dir = probeTempDir();
        (new ProbeStatefile($dir, 123, heartbeatSeconds: 0))->heartbeat(42, 40, 2);

        $files = ProbeStatefile::fresh($dir, maxAgeSeconds: 5);

        expect($files)->toHaveCount(1)
            ->and($files[0]['pid'])->toBe(123)
            ->and($files[0]['state'])->toBe('booting')
            ->and($files[0]['connected'])->toBeTrue()
            ->and($files[0]['consumed'])->toBe(42)
            ->and($files[0]['acked'])->toBe(40)
            ->and($files[0]['nacked'])->toBe(2);
    });

    it('ignores statefiles older than the freshness window', function () {
        $dir = probeTempDir();
        (new ProbeStatefile($dir, 123, heartbeatSeconds: 0))->heartbeat(0, 0, 0);
        touch($dir.'/123.json', time() - 10);

        expect(ProbeStatefile::fresh($dir, 5))->toBe([]);
    });

    it('skips malformed statefiles', function () {
        $dir = probeTempDir();
        file_put_contents($dir.'/garbage.json', 'not json');

        expect(ProbeStatefile::fresh($dir, 5))->toBe([]);
    });

    it('fails closed when the connected flag is missing', function () {
        $dir = probeTempDir();
        file_put_contents($dir.'/9.json', json_encode(['pid' => 9, 'state' => 'running']));

        $files = ProbeStatefile::fresh($dir, 5);

        expect($files)->toHaveCount(1)
            ->and($files[0]['connected'])->toBeFalse();
    });

    it('returns an empty list when the directory does not exist', function () {
        expect(ProbeStatefile::fresh(probeTempDir().'/missing', 5))->toBe([]);
    });
});
