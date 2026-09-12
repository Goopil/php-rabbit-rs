<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

/**
 * Pure scaling decision for one connection (one work-plan entry): when to
 * add children (broker depth outpaces the live fleet) and when to release
 * idle ones (depth empty for long enough). No I/O — the supervisor samples
 * the depth and enacts the decision.
 *
 * One decision per pass: at most {@see MAX_SHIFT} children are added or
 * released, no sooner than the cooldown after the previous action, clamped
 * to [minWorkers, maxWorkers]. Scale-down requires the depth to have stayed
 * at zero for the whole idle window (hysteresis, so a queue draining in a
 * burst does not flap the fleet).
 */
final class WorkScalePolicy
{
    /** Scale up when depth exceeds this multiple of the live workers. */
    private const UP_RATIO = 2.0;

    /** Maximum children added or released per pass. */
    private const MAX_SHIFT = 2;

    public function __construct(
        private readonly int $minWorkers,
        private readonly int $maxWorkers,
        private readonly float $cooldownSeconds,
        private readonly int $idleSeconds,
    ) {}

    /**
     * Decide one scaling step for a connection. The state accumulator is
     * updated in place: the last-action timestamp and the continuous
     * zero-depth streak (tracked even while the cooldown blocks acting).
     *
     * @return array{up: int, down: int}
     */
    public function decide(float $now, int $depth, int $live, ScaleState $state): array
    {
        if ($depth > 0) {
            $state->emptySince = null;
        } elseif ($state->emptySince === null) {
            $state->emptySince = $now;
        }

        if ($now - $state->lastScaleAt < $this->cooldownSeconds) {
            return ['up' => 0, 'down' => 0];
        }

        $up = $this->upCount($depth, $live);
        if ($up > 0) {
            $state->lastScaleAt = $now;

            return ['up' => $up, 'down' => 0];
        }

        $down = $this->downCount($now, $depth, $live, $state);
        if ($down > 0) {
            $state->lastScaleAt = $now;

            return ['up' => 0, 'down' => $down];
        }

        return ['up' => 0, 'down' => 0];
    }

    private function upCount(int $depth, int $live): int
    {
        if ($depth <= 0 || $live >= $this->maxWorkers || $depth <= $live * self::UP_RATIO) {
            return 0;
        }

        return min(self::MAX_SHIFT, $this->maxWorkers - $live);
    }

    private function downCount(float $now, int $depth, int $live, ScaleState $state): int
    {
        if ($depth > 0 || $live <= $this->minWorkers || $state->emptySince === null) {
            return 0;
        }

        if ($now - $state->emptySince < $this->idleSeconds) {
            return 0;
        }

        return min(self::MAX_SHIFT, $live - $this->minWorkers);
    }
}
