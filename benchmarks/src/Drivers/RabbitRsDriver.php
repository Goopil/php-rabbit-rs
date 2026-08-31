<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\BenchmarkException;
use Bench\Config;
use Bench\ScenarioMode;
use Goopil\RabbitRs\Consumer;
use Goopil\RabbitRs\Pool;

class RabbitRsDriver extends AbstractBenchmark
{
    private const QUEUE = 'bench.rabbit-rs';

    private ?Pool $pool = null;
    private ?Consumer $consumer = null;

    public function getName(): string
    {
        return 'rabbit-rs';
    }

    public function setUp(): void
    {
        $config = [
            'brokers' => [[
                'name' => 'default',
                'hosts' => [['host' => Config::RABBITMQ_HOST, 'port' => Config::RABBITMQ_PORT]],
                'vhost' => Config::RABBITMQ_VHOST,
                'credentials' => ['username' => Config::RABBITMQ_USER, 'password' => Config::RABBITMQ_PASSWORD],
                'tls' => ['enabled' => false],
                'heartbeat' => 30,
            ]],
            'workers' => [[
                'name' => 'default',
                'subscriptions' => [[
                    'name' => 'default',
                    'broker' => 'default',
                    'queue' => self::QUEUE,
                    'weight' => 1,
                    'priority_class' => 0,
                    'prefetch' => match ($this->scenarioMode) {
                        ScenarioMode::LARAVEL_WORKER => Config::PREFETCH_LARAVEL,
                        default => Config::PREFETCH_COUNT,
                    },
                    'early_ack' => match ($this->scenarioMode) {
                        ScenarioMode::AUTO_ACK => true,
                        default => false,
                    },
                    'no_ack' => match ($this->scenarioMode) {
                        ScenarioMode::AUTO_ACK => true,
                        default => false,
                    },
                ]],
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                ],
            ]],
            'topology_mode' => 'declare',
            'publisher' => [
                'confirms' => match ($this->scenarioMode) {
                    ScenarioMode::FIRE_AND_FORGET, ScenarioMode::AUTO_ACK, ScenarioMode::LARAVEL_WORKER => false,
                    ScenarioMode::BATCH_CONFIRM, ScenarioMode::LARAVEL_DISPATCH => true,
                },
                'mandatory' => match ($this->scenarioMode) {
                    ScenarioMode::FIRE_AND_FORGET, ScenarioMode::AUTO_ACK, ScenarioMode::LARAVEL_WORKER => false,
                    ScenarioMode::BATCH_CONFIRM, ScenarioMode::LARAVEL_DISPATCH => true,
                },
                'safety' => match ($this->scenarioMode) {
                    ScenarioMode::FIRE_AND_FORGET, ScenarioMode::AUTO_ACK, ScenarioMode::LARAVEL_WORKER => 'blind',
                    ScenarioMode::BATCH_CONFIRM, ScenarioMode::LARAVEL_DISPATCH => 'safe',
                },
                'confirm_timeout' => 30000,
            ],
        ];

        $this->pool = new Pool($config);
    }

    public function purgeQueue(): void
    {
        if ($this->pool !== null) {
            try {
                $this->pool->clear('default', self::QUEUE);
            } catch (\Throwable) {
                // Queue may not exist yet; safe to ignore.
            }
        }
    }

    public function publishMessages(int $count): void
    {
        if ($this->pool === null) {
            throw new BenchmarkException('Driver not set up');
        }

        match ($this->scenarioMode) {
            ScenarioMode::LARAVEL_DISPATCH => $this->publishSingle($count),
            default => $this->publishBatched($count),
        };
    }

    private function publishBatched(int $count): void
    {
        $batchSize = 256;
        $timeoutMs = 5000;

        $batch = [];
        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $batch[] = [
                'broker' => 'default',
                'exchange' => '',
                'routing_key' => self::QUEUE,
                'payload' => pack('P', $ts) . $this->createMessage((string) $i),
                'message_id' => $this->uuid(),
                'timeout_ms' => $timeoutMs,
            ];

            if (count($batch) >= $batchSize) {
                $publishStart = hrtime(true);
                $this->pool->publishBatch($batch);
                $publishElapsed = (hrtime(true) - $publishStart) / 1_000_000;
                $this->recordPublishLatency($publishElapsed / count($batch));
                $batch = [];
            }
        }

        if ($batch !== []) {
            $publishStart = hrtime(true);
            $this->pool->publishBatch($batch);
            $publishElapsed = (hrtime(true) - $publishStart) / 1_000_000;
            $this->recordPublishLatency($publishElapsed / count($batch));
        }
    }

    private function publishSingle(int $count): void
    {
        $timeoutMs = 30000;

        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $publishStart = hrtime(true);
            $this->pool->publish([
                'broker' => 'default',
                'exchange' => '',
                'routing_key' => self::QUEUE,
                'payload' => pack('P', $ts) . $this->createMessage((string) $i),
                'message_id' => $this->uuid(),
                'timeout_ms' => $timeoutMs,
            ]);
            $publishElapsed = (hrtime(true) - $publishStart) / 1_000_000;
            $this->recordPublishLatency($publishElapsed);
        }

        $this->pool->flush();
    }

    public function consumeMessages(int $count): void
    {
        if ($this->pool === null) {
            throw new BenchmarkException('Driver not set up');
        }

        if ($this->consumer === null) {
            $this->consumer = $this->pool->consumer('default');
        }

        match ($this->scenarioMode) {
            ScenarioMode::BATCH_CONFIRM, ScenarioMode::LARAVEL_DISPATCH => $this->consumeBatchConfirm($count),
            default => $this->consumeSingle($count),
        };
    }

    private function consumeBatchConfirm(int $count): void
    {
        $consumed = 0;
        $consecutiveNulls = 0;
        while ($consumed < $count) {
            $batch = $this->consumer->nextBatch(256, 1000);
            if ($batch === []) {
                $consecutiveNulls++;
                if ($consecutiveNulls >= 3) {
                    break;
                }
                continue;
            }
            $consecutiveNulls = 0;

            $last = end($batch);
            foreach ($batch as $d) {
                $this->recordLatencyFromPayload($d->payload());
                $this->recordReceived($d->metadata()['message_id']);
            }
            $this->consumer->ackThrough($last);
            $consumed += count($batch);
        }
    }

    private function consumeSingle(int $count): void
    {
        $consumed = 0;
        $consecutiveNulls = 0;
        while ($consumed < $count) {
            $delivery = $this->consumer->tryNext();
            if ($delivery === null) {
                $delivery = $this->consumer->next(1000);
                if ($delivery === null) {
                    $consecutiveNulls++;
                    if ($consecutiveNulls >= 3) {
                        break;
                    }
                    continue;
                }
            }
            $consecutiveNulls = 0;

            $payload = $delivery->payload();
            $metadata = $delivery->metadata();
            $this->recordReceived($metadata['message_id']);
            $this->recordLatencyFromPayload($payload);

            if ($this->scenarioMode !== ScenarioMode::AUTO_ACK) {
                $delivery->ack();
            }
            $consumed++;
        }
    }

    public function tearDown(): void
    {
        if ($this->consumer !== null) {
            try {
                $this->consumer->close();
            } catch (\Throwable) {
                // Best-effort: ignore errors during cleanup/teardown.
            }
            $this->consumer = null;
        }
        if ($this->pool !== null) {
            $this->pool->close();
            $this->pool = null;
        }
    }
}
