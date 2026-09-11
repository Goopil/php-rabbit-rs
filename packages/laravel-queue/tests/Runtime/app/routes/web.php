<?php

use App\Jobs\RuntimeJob;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;
use Illuminate\Support\Facades\Queue;

// Resolved at boot time: env() must not be called from request scope under Octane.
$connection = (string) (env('RABBIT_RS_CONNECTION') ?: 'rabbit-rs');

Route::post('/publish', function () use ($connection) {
    // The #218 certification pattern: a lone publish with NO follow-up
    // operation. The publication parks in the native publish buffer until a
    // flush path delivers it (age timer, reload, or graceful stop). The
    // harness pins that reload and graceful stop both flush it without loss.
    Queue::connection($connection)->push(new RuntimeJob);

    return response()->json(['published' => true]);
});

Route::get('/consume-one', function () use ($connection) {
    $job = Queue::connection($connection)->pop();
    if ($job === null) {
        return response()->json(['acked' => false]);
    }

    $job->delete();

    return response()->json(['acked' => true, 'job_id' => $job->getJobId()]);
});

Route::get('/stats', function () use ($connection) {
    // Recompile with the same inputs the connector uses so the factory
    // returns the SAME pool the publish route published through (identical
    // native fingerprint), exposing its live publish buffer occupancy.
    $config = (array) config("queue.connections.{$connection}");
    $compiled = ConnectionCompiler::compile($connection, $config, RabbitRsConnections::packageDefaults());
    $pool = app(NativePoolFactory::class)->make($compiled['native']);

    return response()->json($pool->stats());
});
