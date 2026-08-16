#!/usr/bin/env php
<?php

declare(strict_types=1);

if ($argc < 4) {
    fwrite(STDERR, "Usage: merge-result.php <publish-file> <consume-file> <driver> <payload> <batch> <count>\n");
    exit(1);
}

$publishFile = $argv[1];
$consumeFile = $argv[2];
$driver = $argv[3];
$payload = (int) $argv[4];
$batch = (int) $argv[5];
$count = (int) $argv[6];

$publish = file_exists($publishFile) ? file_get_contents($publishFile) : '{}';
$consume = file_exists($consumeFile) ? file_get_contents($consumeFile) : '{}';

$publishData = json_decode($publish, true);
$consumeData = json_decode($consume, true);

if (!is_array($publishData)) {
    $publishData = [];
}
if (!is_array($consumeData)) {
    $consumeData = [];
}

echo json_encode([
    'driver' => $driver,
    'payload_size' => $payload,
    'batch_size' => $batch,
    'message_count' => $count,
    'publish' => $publishData,
    'consume' => $consumeData,
], JSON_PRETTY_PRINT);
