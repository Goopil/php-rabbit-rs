<?php

return [
    'driver' => env('SESSION_DRIVER', 'array'),
    'lifetime' => (int) env('SESSION_LIFETIME', 120),
    'expire_on_close' => false,
    'encrypt' => false,
    'files' => storage_path('framework/sessions'),
    'connection' => null,
    'table' => 'sessions',
    'store' => null,
    'lottery' => [2, 100],
    'cookie' => env('SESSION_COOKIE', 'rabbit_rs_runtime_session'),
    'path' => '/',
    'domain' => env('SESSION_DOMAIN'),
    'secure' => null,
    'http_only' => true,
    'same_site' => 'lax',
    'partitioned' => false,
];
