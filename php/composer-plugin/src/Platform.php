<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Installer;

final class Platform
{
    private function __construct(
        private readonly string $identifier,
    ) {
    }

    public static function detect(): self
    {
        $arch = strtolower(php_uname('m'));
        $arch = $arch !== '' ? $arch : 'unknown';

        return match (PHP_OS_FAMILY) {
            'Linux' => new self(sprintf('linux-%s-%s', self::isMusl() ? 'musl' : 'gnu', $arch)),
            'Darwin' => new self(sprintf('darwin-%s', $arch)),
            'Windows' => new self(sprintf('windows-%s', $arch)),
            default => throw new \RuntimeException(sprintf('Unsupported platform: %s (%s)', PHP_OS_FAMILY, php_uname())),
        };
    }

    public function id(): string
    {
        return $this->identifier;
    }

    private static function isMusl(): bool
    {
        if (PHP_OS_FAMILY !== 'Linux') {
            return false;
        }

        if (defined('PHP_MUSL_VERSION')) {
            return true;
        }

        $uname = strtolower(php_uname('v'));
        if (str_contains($uname, 'musl')) {
            return true;
        }

        if (is_readable('/etc/os-release')) {
            $data = @file_get_contents('/etc/os-release');
            if ($data !== false && str_contains(strtolower($data), 'alpine')) {
                return true;
            }
        }

        foreach (['/lib/ld-musl-x86_64.so.1', '/lib/ld-musl-aarch64.so.1', '/lib/x86_64-linux-musl/libc.so.1'] as $candidate) {
            if (file_exists($candidate)) {
                return true;
            }
        }

        return false;
    }
}

