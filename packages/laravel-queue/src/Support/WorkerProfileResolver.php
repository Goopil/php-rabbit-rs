<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use InvalidArgumentException;

final class WorkerProfileResolver
{
    /** @var array<string, array<string, string>> */
    private array $profiles = [];

    /**
     * @param list<array<string, mixed>> $workers
     */
    public function __construct(array $workers)
    {
        foreach ($workers as $worker) {
            $profile = $worker['name'];
            $subscriptions = [];

            foreach ($worker['subscriptions'] as $subscription) {
                $subscriptions[$subscription['name']] = $subscription['queue'];
            }

            $this->profiles[$profile] = $subscriptions;
        }
    }

    public function resolve(mixed $profile, string $defaultProfile): string
    {
        $profile ??= $defaultProfile;
        if (! is_string($profile) || $profile === '') {
            throw new InvalidArgumentException('worker profile must be a non-empty string');
        }
        if (! isset($this->profiles[$profile])) {
            throw new InvalidArgumentException("workers.{$profile}: unknown worker profile");
        }

        return $profile;
    }

    public function queue(string $profile, mixed $subscription): string
    {
        if (! is_string($subscription) || $subscription === '') {
            throw new InvalidArgumentException(
                "workers.{$profile}.subscriptions: delivery has no subscription alias",
            );
        }
        if (! isset($this->profiles[$profile][$subscription])) {
            throw new InvalidArgumentException(
                "workers.{$profile}.subscriptions.{$subscription}: unknown subscription",
            );
        }

        return $this->profiles[$profile][$subscription];
    }
}