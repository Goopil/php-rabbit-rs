<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use RuntimeException;

/**
 * Composer caret check against the ext-rabbit_rs constraint, shared by
 * connection resolution (RabbitMqServiceProvider) and rabbit-rs:doctor:
 * a loaded binary outside the constraint must fail with the install path
 * instead of a confusing native pool-creation error (deny_unknown_fields
 * on the newer compiled config).
 */
final class ExtensionConstraint
{
    /**
     * Composer caret constraint check, limited to the ^major.minor[.patch]
     * shape the package pins (ext-rabbit_rs ^0.3.10): on 0.x the caret admits
     * only the declared minor. Unknown shapes pass — callers report the
     * version instead of guessing.
     */
    public static function satisfiesCaret(string $version, string $constraint): bool
    {
        if (preg_match('/^\^(\d+)\.(\d+)(?:\.(\d+))?$/', $constraint, $matches) !== 1) {
            return true;
        }

        $major = (int) $matches[1];
        $minor = (int) $matches[2];
        $floor = sprintf('%d.%d.%d', $major, $minor, (int) ($matches[3] ?? 0));
        $ceiling = $major > 0
            ? sprintf('%d.0.0', $major + 1)
            : sprintf('0.%d.0', $minor + 1);

        return version_compare($version, $floor, '>=')
            && version_compare($version, $ceiling, '<');
    }

    /**
     * Asserts the loaded extension version satisfies the constraint. Only
     * reached when the extension is loaded — the missing-extension error
     * names the constraint and stays authoritative when it is absent. An
     * undeterminable version passes (the doctor reports it instead of
     * guessing).
     *
     * @throws RuntimeException when the loaded version is outside the constraint
     */
    public static function assertSatisfied(?string $version, string $constraint): void
    {
        $loaded = $version ?? 'unknown';
        if (self::satisfiesCaret($loaded, $constraint)) {
            return;
        }

        throw new RuntimeException(sprintf(
            'ext-rabbit_rs %s is loaded, but this driver requires ext-rabbit_rs %s. Upgrade the native extension with `pie install goopil/rabbit-rs-native` (macOS: `brew install goopil/rabbit-rs/rabbit-rs`), then retry.',
            $loaded,
            $constraint,
        ));
    }
}
