<?php
declare(strict_types=1);

// CLI driver: sends ONE FastCGI request to the php-fpm unix socket and prints
// the JSON response body. Usage:
//   php fpm_request.php <socket> <script> [NAME=VALUE ...]
// Extra NAME=VALUE pairs are forwarded as FastCGI params ($_SERVER).

require __DIR__ . '/fcgi_client.php';

if ($argc < 3) {
    fwrite(STDERR, "usage: php fpm_request.php <socket> <script> [NAME=VALUE ...]\n");
    exit(2);
}

$extra = [];
foreach (array_slice($argv, 3) as $pair) {
    $separator = strpos($pair, '=');
    if ($separator === false) {
        fwrite(STDERR, "invalid FastCGI param (expected NAME=VALUE): {$pair}\n");
        exit(2);
    }
    $extra[substr($pair, 0, $separator)] = substr($pair, $separator + 1);
}

echo json_encode(fcgi_request($argv[1], $argv[2], $extra), JSON_THROW_ON_ERROR), "\n";
