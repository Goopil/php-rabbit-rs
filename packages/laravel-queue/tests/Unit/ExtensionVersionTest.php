<?php

declare(strict_types=1);

describe('ExtensionVersion', function () {
    it('states the same extension version constraint everywhere', function () {
        $composer = json_decode(file_get_contents(__DIR__.'/../../composer.json'), true);
        $constraint = $composer['require']['ext-rabbit_rs'];

        expect($constraint)->toBe('^0.0'); // aligned to workspace 0.0.x until 1.0
    });
});
