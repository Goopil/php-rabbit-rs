<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Exceptions;

/**
 * Thrown when the RabbitMQ management API responds with a non-successful
 * HTTP status; console commands treat it as advisory and degrade to a
 * warning instead of failing.
 */
final class ManagementApiException extends \RuntimeException {}
