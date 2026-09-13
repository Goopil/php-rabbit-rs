<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Console\DoctorProbe;
use Illuminate\Support\Facades\Artisan;

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    // Fresh, uniquely named topology so the canary consumes exactly its own
    // probe and the DLQ holds nothing but it during the verification window.
    $this->mainQueue = uniqueQueue('rabbit-rs-it-canary');
    $this->dlq = uniqueQueue('rabbit-rs-it-canary-dead');
    $this->dlx = uniqueQueue('rabbit-rs-it-canary-dlx');
    // No dots in the connection name: config()->set treats them as path
    // separators and would nest the entry under the wrong key.
    $this->connectionName = 'rabbit-rs-canary-'.str_replace('.', '', (string) uniqid('', true));

    // admin/admin_lab: full permissions on AMQP and the management API, so
    // the pool can declare the topology and the canary can verify the DLQ.
    $this->config = array_merge(liveConfig($this->mainQueue), [
        'username' => 'admin',
        'password' => 'admin_lab',
        'management_url' => 'http://localhost:15672',
        'dead_letter' => ['exchange' => $this->dlx, 'queue' => $this->dlq],
    ]);
    config()->set('queue.connections.'.$this->connectionName, $this->config);
});

afterEach(function () {
    // The declare-mode pool brings the queue, DLX, DLQ and bindings up; tear
    // the uniquely named pieces down so the lab stays clean.
    deleteQueue($this->mainQueue);
    deleteQueue($this->dlq);
    managementRequest('DELETE', 'http://localhost:15672/api/exchanges/'.rawurlencode(ORDERS_VHOST).'/'.urlencode($this->dlx));
});

/**
 * Declares the connection's topology the way `rabbit-rs:topology --fix`
 * does (transient consumer bring-up). Without it the doctor's broker probe
 * fails NOT-FOUND on the not-yet-declared queue and the canary is skipped —
 * declare mode only creates topology when a pool opens.
 */
function declareCanaryTopology(string $connectionName, array $config): void
{
    grantRabbitRsConfigure();
    $compiled = ConnectionCompiler::compile($connectionName, $config);
    $probe = new DoctorProbe;
    $profile = (string) $compiled['native']['workers'][0]['name'];
    $error = $probe->declareTopology($compiled['native'], $profile);
    expect($error)->toBeNull("pre-declaration failed: {$error}");
}

it('proves the configured dead-letter wiring end-to-end through the doctor canary', function () {
    declareCanaryTopology($this->connectionName, $this->config);

    Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]);
    $output = Artisan::output();

    expect($output)->toContain('dead-letter canary: delivered, rejected, and received on the configured DLQ')
        ->and($output)->not->toContain('dead-letter canary failed')
        ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]))->toBe(0);
});

it('finds the canary behind a foreign DLQ backlog', function () {
    declareCanaryTopology($this->connectionName, $this->config);
    seedForeignDeadLetters($this->dlq, 40);

    Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]);
    $output = Artisan::output();

    // The bulk scan sees the whole (sub-window) DLQ: the canary dead-letters
    // behind 40 foreign messages and is still found — this was a hard fail
    // when the verification pulled one message at a time (#275).
    expect($output)->toContain('dead-letter canary: delivered')
        ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]))->toBe(0);
});

it('verifies the wiring through the canary DLQ when the configured DLQ backlog outgrows the scan window', function () {
    declareCanaryTopology($this->connectionName, $this->config);
    seedForeignDeadLetters($this->dlq, 110);

    Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]);
    $output = Artisan::output();

    // 110 foreign messages wall the canary off beyond the configured DLQ's
    // 100-message scan window, but the doctor-owned canary DLQ — purged and
    // deleted on every run, so a backlog can never wall it off (#288) —
    // proves the DLX wiring is alive: the verdict is now a precise warn
    // (wiring verified, foreign count reported) instead of the old opaque
    // "the backlog outgrew the scan; dead-letter delivery unverified".
    expect($output)->toContain('dead-letter canary inconclusive')
        ->and($output)->toContain('wiring verified through the canary DLQ')
        ->and($output)->not->toContain('dead-letter canary failed')
        ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]))->toBe(0);
});

it('purges and deletes its canary DLQ after the check so nothing accumulates', function () {
    declareCanaryTopology($this->connectionName, $this->config);

    Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]);

    // The canary DLQ is doctor-owned hygiene: once the check is over, no
    // rabbit-rs.canary.* queue may remain on the broker (#288). The test's
    // own topology (main queue, DLQ, DLX) is torn down by afterEach; the
    // canary DLQ is deliberately not, so this assertion is the proof.
    $queues = json_decode((string) managementRequest('GET', 'http://localhost:15672/api/queues'), true) ?: [];
    $canaryQueues = array_values(array_filter(
        is_array($queues) ? $queues : [],
        static fn (array $queue): bool => str_starts_with((string) ($queue['name'] ?? ''), 'rabbit-rs.canary.'),
    ));
    expect($canaryQueues)->toBe([]);
});

/**
 * Seeds foreign dead-lettered messages directly into a queue through the
 * default exchange (routing key = queue name), so the DLQ holds a backlog
 * the canary verification has to see past. `properties` must be a JSON
 * object — an empty PHP array encodes as [] and the API answers 500.
 */
function seedForeignDeadLetters(string $queue, int $count): void
{
    for ($i = 0; $i < $count; $i++) {
        $response = managementRequest('POST', 'http://localhost:15672/api/exchanges/'.rawurlencode(ORDERS_VHOST).'/amq.default/publish', json_encode([
            'properties' => new stdClass,
            'routing_key' => $queue,
            'payload' => 'foreign dead-letter backlog filler '.$i,
            'payload_encoding' => 'string',
        ]));
        expect($response)->toContain('"routed":true');
    }
}

it('fails loud when the dead-letter wiring is broken', function () {
    // External mode + a manually declared main queue: the wiring is never
    // declared, the DLX does not exist, so the terminal reject cannot
    // dead-letter anything and the canary must fail loud.
    $this->config['topology_mode'] = 'external';
    config()->set('queue.connections.'.$this->connectionName, $this->config);
    grantRabbitRsConfigure();
    declareQueue($this->mainQueue);

    Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]);
    $output = Artisan::output();

    expect($output)->toContain('dead-letter canary failed')
        ->and($output)->toContain('never reached the DLX')
        ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]))->toBe(1);
});
