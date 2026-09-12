# Flush-timer lone-publish latency (issue #255)

Measurement of the publish buffer's age-flush timer contract
(`publisher.flush_interval`, default 1 ms): how long a lone publish takes to
reach the broker when the process never publishes, pops, or flushes again.

Measured on 2026-09-12 against main (post v0.3.1, merge e26d9f5): release
extension, RabbitMQ 4.2.9 3-node lab, PHP 8.4 CLI, single connection.

## Method

Two observation points per run:

- **arrival** (ground truth): a native consumer timestamps the moment the
  broker hands the lone publication out — sub-ms precision.
- **mgmt depth** (the v0.2.2 report's methodology): poll
  `GET /api/queues/{vhost}/{queue}` until `messages_ready >= 1`, fresh queue
  per run.

Scenarios per publisher safety mode (`safe`, `blind`): cold (15 runs, fresh
pool), warm (200 runs, same pool), under concurrent suite load (100 runs
while `./scripts/test-laravel.sh tests/Integration` runs), mgmt depth (25
runs).

## Results (consumer arrival — ground truth)

| scenario          | safe p50 | safe p99 | safe max | blind p50 | blind p99 | blind max |
|-------------------|----------|----------|----------|-----------|-----------|-----------|
| cold (15)         | 1.94 ms  | 2.15 ms  | 2.27 ms  | 2.26 ms   | 3.85 ms   | 7.27 ms   |
| warm (200)        | 1.87 ms  | 4.07 ms  | 4.77 ms  | 1.88 ms   | 5.38 ms   | 6.22 ms   |
| suite load (100)  | 1.57 ms  | 3.18 ms  | 5.92 ms  | 1.84 ms   | 6.34 ms   | 6.39 ms   |

Zero runs over 100 ms, zero over 1 s, zero timeouts — 630 measured runs
total. The timer holds its contract with a wide margin (p99 ≤ 6.4 ms against
a 100 ms acceptance bar from issue #255).

## The v0.2.2 report's 4–5 s tail was an observation artifact

The management-API depth methodology never observed the messages on this
RabbitMQ version: the sampled `messages_ready` metric does not surface
promptly for a queue with content — it stayed ABSENT for 20+ s on both
quorum and classic queues holding a message, on the default
statistics-collection interval. The live
`POST /api/queues/{vhost}/{queue}/get` endpoint (basic.get through the
management API) proves landing at t+114 ms on the first poll.

The v0.2.2 report's "~3.9–4.6 s in some runs, ~0.0 s in others — no pattern
identified" is consistent with sampled-stats quantization, not timer
behavior. Documented as a measurement caveat in `docs/reference.md`
("Lone-publish landing latency").

## Raw data

- `report-safe-203712.json` — safe mode: all scenarios + raw per-run arrays.
- `report-blind-204323.json` — blind mode: same shape.
