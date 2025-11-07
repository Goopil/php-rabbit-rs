<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;

class RabbitRsAutoAckBenchmark extends RabbitRsFireAndForgetBenchmark
{
    public function getName(): string
    {
        return 'RabbitRs (Auto Ack)';
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;
        $callback = function ($delivery) use (&$consumed, $count) {
            $consumed++;
            if ($consumed >= $count) {
                $this->channel->basicCancel();
            }
        };

        $this->channel->qos(Config::PREFETCH_COUNT, ['global' => false]);

        $this->channel->simpleConsume(Config::QUEUE_NAME, $callback, [
            'no_ack' => true,
        ]);

        while ($consumed < $count) {
            $this->channel->wait(100, min($count - $consumed, 100));
        }
    }
}
