<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use RuntimeException;

/**
 * Thrown by the doctor's dead-letter canary when the environment prevents a
 * verdict — a DLQ backlog larger than the scan window, or competing
 * consumers that starved the probe — as opposed to a verified dead-letter
 * failure. The doctor renders it as a warning, not a failure.
 */
final class CanaryInconclusiveException extends RuntimeException {}
