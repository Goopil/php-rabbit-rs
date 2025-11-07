<?php

namespace Goopil\RabbitRs\Benchmarks;

class PhpAmqpExtAutoAckBenchmark extends PhpAmqpExtFireAndForgetBenchmark
{
    public function getName(): string
    {
        return 'php-amqp (Auto Ack)';
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;

        $this->queue->consume(function (\AMQPEnvelope $envelope, \AMQPQueue $queue) use (&$consumed, $count) {
            $consumed++;
            if ($consumed >= $count) {
                return false;
            }
            return true;
        }, $this->autoAckFlag());
    }
}
