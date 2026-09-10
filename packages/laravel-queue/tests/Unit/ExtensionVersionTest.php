<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;

describe('ExtensionVersion', function () {
    it('states the same extension version constraint everywhere', function () {
        $composer = json_decode(file_get_contents(__DIR__.'/../../composer.json'), true);

        // ext-rabbit_rs is a suggestion, not a requirement: composer install
        // must succeed without the extension (issue #58). The constraint
        // lives in the service provider constant; the suggest entry must
        // reference it so the text cannot drift.
        expect($composer['require'])->not->toHaveKey('ext-rabbit_rs')
            ->and($composer['suggest']['ext-rabbit_rs'] ?? null)
            ->toContain(RabbitMqServiceProvider::EXTENSION_CONSTRAINT);
    });

    it('covers the extension version the workspace actually builds', function () {
        // The rabbit-rs-php crate inherits its version from
        // [workspace.package] in the root Cargo.toml. Parsed as text so the
        // guard also runs in CI jobs without the Rust toolchain.
        $cargoToml = __DIR__.'/../../../../Cargo.toml';

        if (! is_file($cargoToml)) {
            $this->markTestSkipped('workspace Cargo.toml not available (standalone package checkout)');
        }

        $version = null;
        $inWorkspacePackage = false;
        foreach (file($cargoToml, FILE_IGNORE_NEW_LINES) as $line) {
            if (str_starts_with($line, '[')) {
                $inWorkspacePackage = $line === '[workspace.package]';

                continue;
            }
            if ($inWorkspacePackage && preg_match('/^version\s*=\s*"([^"]+)"/', $line, $m) === 1) {
                $version = $m[1];
                break;
            }
        }

        expect($version)->not->toBeNull('[workspace.package] must declare a version');

        // The caret constraint must equal the crate's current version, patch
        // included: the package and the extension are released in lockstep,
        // and the compiled native config schema evolves with every release
        // (deny_unknown_fields rejects unknown keys at pool creation), so an
        // older extension binary must never satisfy a newer package (the
        // 0.0 -> 0.1 drift that made the package uninstallable with ext
        // 0.1.0, and the 0.2.1 routes key that 0.2.0 binaries reject).
        $expectedConstraint = sprintf('^%s', $version);

        expect(RabbitMqServiceProvider::EXTENSION_CONSTRAINT)->toBe($expectedConstraint);
    });
});
