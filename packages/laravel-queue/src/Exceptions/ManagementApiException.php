<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Exceptions;

/**
 * Raised when the RabbitMQ management API is unreachable or answers with a
 * non-successful status during topology verification; callers turn it into an
 * advisory warning.
 */
final class ManagementApiException extends \RuntimeException
{
}
