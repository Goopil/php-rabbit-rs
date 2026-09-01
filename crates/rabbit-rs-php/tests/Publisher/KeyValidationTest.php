<?php

declare(strict_types=1);

function publishMessageWithKey(string $messageId, array $extra): array
{
    return array_merge(pubMessage($messageId), $extra);
}

describe('publish key validation', function () {
    beforeEach(function () {
        $this->pool = testingPool(defaultConfig(), ['confirmed_publications' => 5]);
    });

    afterEach(function () {
        $this->pool->close();
    });

    it('rejects an unknown publish key', function () {
        $message = publishMessageWithKey('typo', ['delai_ms' => 5000]);

        expect(fn () => $this->pool->publish($message))
            ->toThrow(\Goopil\RabbitRs\Exception::class, 'message.delai_ms: unknown field');
    });

    it('rejects an unknown key inside a batch message', function () {
        $message = publishMessageWithKey('batch-typo', ['delay__ms' => 5]);

        expect(fn () => $this->pool->publishBatch([pubMessage('batch-ok'), $message]))
            ->toThrow(\Goopil\RabbitRs\Exception::class, 'messages[1].delay__ms: unknown field');
    });
});
