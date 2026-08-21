<?php

declare(strict_types=1);

function outcomeMessage(string $id, int $timeoutMs = 1000): array
{
    return [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => 'payload',
        'message_id' => $id,
        'timeout_ms' => $timeoutMs,
    ];
}

dataset('outcomes', [
    'ack'            => ['ack', null, 'confirmed', null],
    'returned'       => ['returned', \Goopil\RabbitRs\Exception::class, 'returned', '312'],
    'pending'        => ['pending', \Goopil\RabbitRs\Exception::class, 'timed out', null],
    'transport_error' => ['transport_error', \Goopil\RabbitRs\ConnectionException::class, 'transport failed', null],
]);

it('maps publisher confirmations and transport failures to the PHP contract', function (string $outcome, ?string $exceptionClass, string $messageFragment, ?string $codeFragment) {
    $pool = testingPool(defaultConfig(), [
        'publication_outcomes' => [$outcome],
    ]);

    if ($outcome === 'ack') {
        expect($pool->publish(outcomeMessage('confirmed')))->toBe('confirmed');
    } else {
        $messageId = match ($outcome) {
            'returned' => 'returned',
            'pending' => 'timeout',
            'transport_error' => 'transport',
        };
        $timeoutMs = $outcome === 'pending' ? 1 : 1000;

        try {
            $pool->publish(outcomeMessage($messageId, $timeoutMs));
            expect(false)->toBeTrue("{$outcome} must fail");
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e)->toBeInstanceOf($exceptionClass);
            expect($e->getMessage())->toContain($messageFragment);
            if ($codeFragment !== null) {
                expect($e->getMessage())->toContain($codeFragment);
            }
        }
    }

    $pool->close();
})->with('outcomes');
