<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Config;

use InvalidArgumentException;

/**
 * Scalar coercion primitives shared by the connection config compiler:
 * every value is validated against its expected shape (accepting the
 * env-style string forms Laravel env() produces) and every failure throws an
 * InvalidArgumentException identifying the exact config path.
 */
final class Values
{
    public static function string(mixed $value, string $path, bool $allowEmpty = false): string
    {
        if (! is_string($value) || (! $allowEmpty && $value === '')) {
            self::invalid($path, $allowEmpty ? 'must be a string' : 'must be a non-empty string');
        }

        return $value;
    }

    public static function boolean(mixed $value, string $path): bool
    {
        if (is_bool($value)) {
            return $value;
        }

        // Laravel env() returns strings for .env flags (e.g. '1', 'true'),
        // so accept those forms and reject anything else strictly.
        if (is_string($value)) {
            $normalized = filter_var($value, FILTER_VALIDATE_BOOLEAN, FILTER_NULL_ON_FAILURE);
            if ($normalized !== null) {
                return $normalized;
            }
        }

        self::invalid($path, 'must be a boolean or an env-style boolean string (e.g. "1", "true")');
    }

    public static function integer(mixed $value, string $path): int
    {
        if (is_int($value)) {
            return $value;
        }

        // Laravel env() returns strings for .env numbers (e.g. '64'), so
        // accept signed integer strings and let the caller range-check.
        if (is_string($value) && preg_match('/^-?\d+$/', $value) === 1) {
            return (int) $value;
        }

        self::invalid($path, 'must be an integer or an env-style integer string (e.g. "64")');
    }

    public static function positiveInt(mixed $value, string $path, ?int $max = null): int
    {
        $value = self::integer($value, $path);
        if ($value < 1) {
            self::invalid($path, 'must be a positive integer');
        }
        if ($max !== null && $value > $max) {
            self::invalid($path, 'must be at most '.$max);
        }

        return $value;
    }

    public static function boundedI16(mixed $value, string $path): int
    {
        $value = self::integer($value, $path);
        if ($value < -32768 || $value > 32767) {
            self::invalid($path, 'must be an integer between -32768 and 32767');
        }

        return $value;
    }

    /**
     * @param array<mixed> $section
     * @param list<string> $known
     */
    public static function rejectUnknownKeys(array $section, array $known, string $path): void
    {
        foreach (array_keys($section) as $key) {
            if (! in_array($key, $known, true)) {
                self::invalid($path.'.'.$key, 'unknown key');
            }
        }
    }

    public static function invalid(string $path, string $message): never
    {
        throw new InvalidArgumentException($path.': '.$message);
    }
}
