<?php

namespace Goopil\RabbitRs\Benchmarks;

class PhpAmqpExtBatchConfirmBenchmark extends PhpAmqpExtFireAndForgetBenchmark
{
    public function getName(): string
    {
        return 'php-amqp (Batch Confirm)';
    }

    public function setUp()
    {
        parent::setUp();
        if (method_exists($this->channel, 'confirmSelect')) {
            $this->channel->confirmSelect();
            $this->registerConfirmHandlers();
        }
    }

    public function publishMessages(int $count)
    {
        parent::publishMessages($count);

        $this->waitForAllConfirms();
    }
}
