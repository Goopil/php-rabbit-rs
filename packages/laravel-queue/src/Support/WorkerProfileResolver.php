<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use InvalidArgumentException;

final class WorkerProfileResolver
{
    /** Prefix used for profiles built on the fly by the auto-subscribe pop path. */
    private const AUTO_PROFILE_PREFIX = '__auto__.';

    /** Name of the single subscription of an auto-subscribe profile. */
    private const AUTO_SUBSCRIPTION_NAME = 'auto';

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

    public function profileForQueue(string $queue): ?string
    {
        foreach ($this->profiles as $profile => $subscriptions) {
            if (in_array($queue, $subscriptions, true)) {
                return $profile;
            }
        }

        return null;
    }

    /**
     * Whether the given name is a known worker profile.
     */
    public function hasProfile(string $profile): bool
    {
        return isset($this->profiles[$profile]);
    }

    /**
     * Registers an implicit worker profile subscribing to the given queue and
     * returns its name.
     *
     * Profiles are cached per queue name (process-local): subsequent lookups
     * through profileForQueue() resolve the same implicit profile, and the
     * consumer cache in the queue reuses it. Configured profiles are never
     * overridden: implicit registrations only fill the gaps.
     */
    public function registerAutoProfile(string $queue): string
    {
        $profile = self::AUTO_PROFILE_PREFIX.$queue;

        $this->profiles[$profile] ??= [self::AUTO_SUBSCRIPTION_NAME => $queue];

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
