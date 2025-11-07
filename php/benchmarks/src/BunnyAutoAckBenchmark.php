<?php

namespace Goopil\RabbitRs\Benchmarks;

use Bunny\Channel;
use Bunny\Message;

class BunnyAutoAckBenchmark extends BunnyFireAndForgetBenchmark
{
    public function getName(): string
    {
        return 'Bunny (Auto Ack)';
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;

        $this->channel->consume(function (Message $msg, Channel $ch) use (&$consumed, $count) {
            $consumed++;
            if ($consumed >= $count) {
                $ch->cancel($msg->consumerTag);
            }
        }, Config::QUEUE_NAME, '', false, true);

        while ($consumed < $count) {
            $this->client->run(0.1);
        }
    }
}
