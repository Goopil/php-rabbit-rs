# Queue/Profile Resolution Design

## Problem

`RabbitMqQueue::pop($queue)` treats its argument as a **worker profile name**, not a **queue name**. This violates the Laravel `QueueContract` contract, where `pop($queue)` is expected to consume from the named queue. The Laravel `QueueWorker` passes queue names (e.g., `default`, `high`, `low`), not profile names.

Additionally, `defaultQueue` serves double duty:
- In `push()`/`later()`: used as the default queue name for routing (`{queue}` placeholder substitution)
- In `pop()`: used as the default worker profile name for consuming

This makes it impossible to call `push()` without an explicit queue argument when the declared queue has a unique name.

## Solution

Resolve the queue name to a worker profile automatically inside `pop()`.

### Changes

#### `WorkerProfileResolver`

Add `profileForQueue(string $queue): ?string` — returns the first worker profile that has a subscription pointing to the given queue, or `null` if none match.

#### `RabbitMqQueue::pop($queue = null, $index = 0)`

1. If `$queue` is `null`, use `$this->defaultQueue` as the queue name (existing behavior for push, now also for pop).
2. Resolve the queue name to a worker profile via `WorkerProfileResolver::profileForQueue()`.
3. If no profile matches the queue, fall back to `$this->defaultQueue` as the profile name.
4. Use the resolved profile to get/create the consumer.

This means `pop('orders')` will find the worker profile that subscribes to the `orders` queue and consume from it.

#### Test integration

Tests can now set `queue => $this->queueName` in the connector config. This makes `defaultQueue = $this->queueName`, so:
- `push('stdClass', $data)` routes to `$this->queueName` (via `{queue}` substitution)
- `pop()` resolves `$this->queueName` to the `default` worker profile (which subscribes to `$this->queueName`)

No more need to pass `$this->queueName` explicitly to `push()` or `pop()`.

### Non-changes

- Config format stays the same (no new `worker_profile` key)
- `WorkerProfileResolver::resolve()` stays for backward compatibility
- `push()`/`later()`/`bulk()` routing logic unchanged
- `route()` fallback to `routes['default']` unchanged

### Testing

1. Unit test: `WorkerProfileResolver::profileForQueue()` returns correct profile for known queue, null for unknown queue.
2. Unit test: `RabbitMqQueue::pop()` resolves queue name to profile.
3. Integration tests: `push()` and `pop()` work without explicit queue argument when `defaultQueue` matches the subscription queue.
