<?php

declare(strict_types=1);

// PHPStan bootstrap: make the classes that exist only outside the package
// visible to analysis without installing them.
//
// 1. ext-rabbit_rs: the native extension is not loaded in CI/analysis
//    processes; the generated stubs (crates/rabbit-rs-php/stubs/) declare
//    the real API. Guarded so a standalone split-package checkout without
//    the workspace still analyses.
// 2. Laravel\Octane and Laravel\Horizon: suggested, not required. The test
//    bootstrap declares conditional fakes that PHPStan picks up the same
//    way the test process does.

$stubs = dirname(__DIR__, 2).'/crates/rabbit-rs-php/stubs/rabbit_rs.stub.php';

if (is_file($stubs)) {
    require_once $stubs;
}

require_once __DIR__.'/tests/bootstrap.php';
