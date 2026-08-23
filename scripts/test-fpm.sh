#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

ROOT_DIR="$(ext_project_root)"
FIXTURE_DIR="${ROOT_DIR}/crates/rabbit-rs-php/tests/fixtures/fpm"
PHP_BIN_PATH="$(command -v "${PHP_BIN:-php}")"
PHP_FPM_PATH="$(command -v "${PHP_FPM_BIN:-php-fpm}")"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/rabbit-rs-fpm.XXXXXX")"

ARTIFACT="$(ext_artifact_path)"

if [[ ! -f "${ARTIFACT}" ]]; then
    echo "extension artifact not found: ${ARTIFACT}" >&2
    echo "Build the extension first: cargo build --manifest-path ${ROOT_DIR}/crates/rabbit-rs-php/Cargo.toml --features extension-tests" >&2
    exit 1
fi

export RABBIT_RS_FPM_PID="${TEST_DIR}/php-fpm.pid"
export RABBIT_RS_FPM_LOG="${TEST_DIR}/php-fpm.log"
export RABBIT_RS_FPM_SOCKET="${TEST_DIR}/php-fpm.sock"
if [[ "${EUID}" -eq 0 ]]; then
    export RABBIT_RS_FPM_USER="nobody"
    export RABBIT_RS_FPM_GROUP="$(id -gn nobody)"
else
    export RABBIT_RS_FPM_USER="$(id -un)"
    export RABBIT_RS_FPM_GROUP="$(id -gn)"
fi

cleanup() {
    if [[ -n "${FPM_PID:-}" ]]; then
        kill -TERM "${FPM_PID}" 2>/dev/null || true
        wait "${FPM_PID}" 2>/dev/null || true
    fi
    rm -rf "${TEST_DIR}"
}
trap cleanup EXIT

# Use -n to ignore system ini files, preventing double-loading.
"${PHP_FPM_PATH}" -n -F -y "${FIXTURE_DIR}/php-fpm.conf" -d "extension=${ARTIFACT}" &
FPM_PID=$!

for _ in {1..100}; do
    if [[ -S "${RABBIT_RS_FPM_SOCKET}" ]]; then
        break
    fi
    if ! kill -0 "${FPM_PID}" 2>/dev/null; then
        echo "php-fpm stopped before creating its socket" >&2
        exit 1
    fi
    sleep 0.05
done

if [[ ! -S "${RABBIT_RS_FPM_SOCKET}" ]]; then
    echo "php-fpm socket was not created" >&2
    exit 1
fi

"${PHP_BIN_PATH}" -- "${RABBIT_RS_FPM_SOCKET}" "${FIXTURE_DIR}/index.php" <<'PHP'
<?php
declare(strict_types=1);

function record(int $type, string $content): string {
    $padding = (8 - strlen($content) % 8) % 8;
    return pack('CCnnCC', 1, $type, 1, strlen($content), $padding, 0)
        . $content
        . str_repeat("\0", $padding);
}

function encoded_length(int $length): string {
    return $length < 128 ? chr($length) : pack('N', $length | 0x80000000);
}

function parameters(array $values): string {
    $encoded = '';
    foreach ($values as $name => $value) {
        $encoded .= encoded_length(strlen($name))
            . encoded_length(strlen($value))
            . $name
            . $value;
    }
    return $encoded;
}

function read_exact($stream, int $length): string {
    $buffer = '';
    while (strlen($buffer) < $length && !feof($stream)) {
        $chunk = fread($stream, $length - strlen($buffer));
        if ($chunk === false) {
            throw new RuntimeException('failed to read FastCGI response');
        }
        $buffer .= $chunk;
    }
    if (strlen($buffer) !== $length) {
        throw new RuntimeException('truncated FastCGI response');
    }
    return $buffer;
}

function begin_request(string $socket, string $script) {
    $stream = stream_socket_client("unix://{$socket}", $errorCode, $errorMessage, 5);
    if ($stream === false) {
        throw new RuntimeException("FastCGI connection failed: {$errorCode} {$errorMessage}");
    }
    $params = parameters([
        'SCRIPT_FILENAME' => $script,
        'SCRIPT_NAME' => '/index.php',
        'REQUEST_METHOD' => 'GET',
        'REQUEST_URI' => '/',
        'SERVER_PROTOCOL' => 'HTTP/1.1',
        'GATEWAY_INTERFACE' => 'CGI/1.1',
        'SERVER_NAME' => 'localhost',
        'SERVER_PORT' => '80',
    ]);
    fwrite($stream, record(1, pack('nC6', 1, 0, 0, 0, 0, 0, 0)));
    fwrite($stream, record(4, $params));
    fwrite($stream, record(4, ''));
    fwrite($stream, record(5, ''));
    return $stream;
}

function finish_request($stream): array {
    $stdout = '';
    $stderr = '';
    while (!feof($stream)) {
        $header = read_exact($stream, 8);
        $record = unpack('Cversion/Ctype/nrequest/nlength/Cpadding/Creserved', $header);
        $content = read_exact($stream, $record['length']);
        if ($record['padding'] > 0) {
            read_exact($stream, $record['padding']);
        }
        if ($record['type'] === 6) {
            $stdout .= $content;
        } elseif ($record['type'] === 7) {
            $stderr .= $content;
        } elseif ($record['type'] === 3) {
            break;
        }
    }
    fclose($stream);
    if ($stderr !== '') {
        throw new RuntimeException("FastCGI stderr: {$stderr}");
    }
    $parts = preg_split("/\r?\n\r?\n/", $stdout, 2);
    if (!isset($parts[1])) {
        throw new RuntimeException("invalid FastCGI response: {$stdout}");
    }
    return json_decode($parts[1], true, flags: JSON_THROW_ON_ERROR);
}

$streams = [];
for ($index = 0; $index < 16; $index++) {
    $streams[] = begin_request($argv[1], $argv[2]);
}

$workers = [];
foreach ($streams as $stream) {
    $response = finish_request($stream);
    if ($response['first_handle'] !== $response['second_handle']) {
        throw new RuntimeException('equivalent pools did not share a handle within one request');
    }
    $pid = (string) $response['pid'];
    if (isset($workers[$pid]) && $workers[$pid]['handle'] !== $response['first_handle']) {
        throw new RuntimeException('worker did not reuse its handle between requests');
    }
    $workers[$pid] = [
        'handle' => $response['first_handle'],
        'count' => ($workers[$pid]['count'] ?? 0) + 1,
    ];
}

if (count($workers) !== 2) {
    throw new RuntimeException('expected responses from two FPM workers');
}
if (count(array_unique(array_column($workers, 'handle'))) !== 2) {
    throw new RuntimeException('FPM workers announced the same handle');
}
foreach ($workers as $worker) {
    if ($worker['count'] < 2) {
        throw new RuntimeException('each FPM worker must serve multiple requests');
    }
}

echo "OK\n";
PHP
