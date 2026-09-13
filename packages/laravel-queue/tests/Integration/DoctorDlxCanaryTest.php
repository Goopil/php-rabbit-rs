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

    expect($output)->toContain('dead-letter canary: delivered')
        ->and($output)->not->toContain('dead-letter canary failed')
        ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]))->toBe(0);
});

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
        ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]))->toBe(1);
});
