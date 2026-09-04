#!/usr/bin/env php
<?php

declare(strict_types=1);

/*
 * Round D profiling probe: publish N messages in a given safety mode through
 * the same code path as the driver-bench dispatch cell (Queue::push), then
 * dump Pool::stats() percentiles (confirmation latency = broker confirm RTT
 * as observed by the publisher actor).
 *
 * Usage: php probe-confirm-rtt.php <safety: safe|blind|unsafe> <count> [interval_flush_ms]
 */

require __DIR__.'/../../../../benchmarks-driver-bench-autoload.php';
