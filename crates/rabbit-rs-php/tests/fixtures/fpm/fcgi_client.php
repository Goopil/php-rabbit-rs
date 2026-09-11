<?php
declare(strict_types=1);

// Shared FastCGI client helpers for the FPM fixtures. The pool-isolation
// driver (inline in scripts/test-fpm.sh) and the single-request CLI driver
// (fpm_request.php) both speak raw FastCGI over the php-fpm unix socket.

function fcgi_record(int $type, string $content): string
{
    $padding = (8 - strlen($content) % 8) % 8;
    return pack('CCnnCC', 1, $type, 1, strlen($content), $padding, 0)
        . $content
        . str_repeat("\0", $padding);
}

function fcgi_encoded_length(int $length): string
{
    return $length < 128 ? chr($length) : pack('N', $length | 0x80000000);
}

function fcgi_parameters(array $values): string
{
    $encoded = '';
    foreach ($values as $name => $value) {
        $encoded .= fcgi_encoded_length(strlen($name))
            . fcgi_encoded_length(strlen($value))
            . $name
            . $value;
    }
    return $encoded;
}

function fcgi_read_exact($stream, int $length): string
{
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

function fcgi_begin_request(string $socket, string $script, array $extraParams = [])
{
    $stream = stream_socket_client("unix://{$socket}", $errorCode, $errorMessage, 5);
    if ($stream === false) {
        throw new RuntimeException("FastCGI connection failed: {$errorCode} {$errorMessage}");
    }
    $params = array_merge([
        'SCRIPT_FILENAME' => $script,
        'SCRIPT_NAME' => '/' . basename($script),
        'REQUEST_METHOD' => 'GET',
        'REQUEST_URI' => '/',
        'SERVER_PROTOCOL' => 'HTTP/1.1',
        'GATEWAY_INTERFACE' => 'CGI/1.1',
        'SERVER_NAME' => 'localhost',
        'SERVER_PORT' => '80',
    ], $extraParams);
    fwrite($stream, fcgi_record(1, pack('nC6', 1, 0, 0, 0, 0, 0, 0)));
    fwrite($stream, fcgi_record(4, fcgi_parameters($params)));
    fwrite($stream, fcgi_record(4, ''));
    fwrite($stream, fcgi_record(5, ''));
    return $stream;
}

function fcgi_finish_request($stream): array
{
    $stdout = '';
    $stderr = '';
    while (!feof($stream)) {
        $header = fcgi_read_exact($stream, 8);
        $record = unpack('Cversion/Ctype/nrequest/nlength/Cpadding/Creserved', $header);
        $content = fcgi_read_exact($stream, $record['length']);
        if ($record['padding'] > 0) {
            fcgi_read_exact($stream, $record['padding']);
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

function fcgi_request(string $socket, string $script, array $extraParams = []): array
{
    return fcgi_finish_request(fcgi_begin_request($socket, $script, $extraParams));
}
