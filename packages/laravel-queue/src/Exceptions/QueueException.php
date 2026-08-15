<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Exceptions;

use Goopil\RabbitRs\Exception as NativeException;
use RuntimeException;

final class QueueException extends RuntimeException
{
    public static function fromNative(NativeException $exception): self
    {
        return new self($exception->getMessage(), (int) $exception->getCode(), $exception);
    }
}