<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;

class AmqplibAutoAckBenchmark extends AmqplibFireAndForgetBenchmark
{
    public function getName(): string
    {
        return 'AMQPLib (Auto Ack)';
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;
        $callback = function ($msg) use (&$consumed, $count) {
            $consumed++;
            if ($consumed >= $count) {
                $this->channel->basic_cancel($msg->getConsumerTag());
            }
        };

        $this->channel->basic_consume(Config::QUEUE_NAME, '', false, true, false, false, $callback);

        while ($consumed < $count) {
            $this->channel->wait(null, false, Config::CONSUMER_WAIT_TIMEOUT);
        }
    }
}
