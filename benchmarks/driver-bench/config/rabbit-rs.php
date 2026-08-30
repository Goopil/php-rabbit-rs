<?php

/*
|--------------------------------------------------------------------------
| rabbit-rs config override (driver-bench only)
|--------------------------------------------------------------------------
|
| The goopil package merges its full default config at boot
| (RabbitMqServiceProvider::mergeConfigFrom) and reads the same env
| variables as .env.example, so this file only carries the bench
| deviation: fairness requires classic durable queues for all three
| drivers while the package default is quorum. Top-level keys defined
| here REPLACE the package defaults, so the whole 'topology' block is
| restated.
|
*/

return [
    'topology' => [
        'queue' => [
            'type' => 'classic',
            'durable' => true,
            'delivery_limit' => null,
        ],
        'dead_letter' => null,
    ],
];
