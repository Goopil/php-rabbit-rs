# Using the RabbitMQ management API

The management API is optional. Rabbit RS can publish and consume jobs without it. Configure it when you need broker-side counters or more complete topology checks.

## Configure the management API

Add the API base URL to your queue connection. Use a management-enabled RabbitMQ broker and credentials with the required permissions:

```php
// config/queue.php, inside the rabbit-rs connection
'management_url' => env('RABBIT_RS_MANAGEMENT_URL', 'http://127.0.0.1:15672'),
```

The driver uses the connection's `username` and `password` for the management API. `RABBIT_RS_MANAGEMENT_URL` is an example: the variable is read only because this connection explicitly wires it with `env()`. For remote brokers, use an HTTPS URL and credentials appropriate to your environment.

## What changes when it is configured?

| Task | Management API configured | No management API |
|---|---|---|
| Publish and consume jobs | AMQP | AMQP |
| Inspect queue counters | Broker-side counters available to `rabbit-rs:status` | Native counters are local to each PHP process |
| Verify topology | Can check bindings and queue arguments | Can check queue and exchange existence; bindings and arguments cannot be verified |
| Detect the delayed-message plugin | HTTP probe | Native AMQP probe |
| Check dead-letter routing | Doctor uses the API to inspect its canary | Doctor uses native AMQP operations |

Configure `management_url` per connection when using more than one broker or vhost. For a first connection, see [Getting started](getting-started.md). For diagnostic commands, see [Diagnostics](reference.md#diagnostics).

## Delayed-message plugin

In `auto` mode the driver checks for the plugin once per connection per process. When `management_url` is configured it checks the management API; otherwise it declares and removes a temporary exchange over AMQP. If the probe is inconclusive, `auto` selects TTL buckets. An explicit `plugin` mode uses the same cached verdict and warns if the result is inconclusive. See [Delayed messages](reference.md#delayed-messages).

The AMQP probe needs permission to declare and delete its temporary exchange. Restart the relevant PHP processes after changing plugin availability so they obtain a fresh verdict.

## Feature details

The table below describes the individual paths and their limits.

## Behavior matrix

| Feature | With `management_url` | Without `management_url` |
|---|---|---|
| Delay plugin probe (`DelayPluginGuard`) | HTTP `GET /api/overview` — `exchange_types` lists `x-delayed-message` when the plugin is enabled | Native probe: `Pool::probeDelayPlugin()` declares the throwaway `rabbit-rs.probe.delayed` exchange — it succeeds only with the plugin (`true`), fails with an unknown-exchange-type error (`false` = provably absent; RabbitMQ 4.x words it `PRECONDITION_FAILED - unknown exchange type 'x-delayed-message'`, verified against the lab no-plugin broker), anything else is inconclusive |
| Doctor dead-letter canary (`DoctorProbe::deadLetterCanary`) | Management API declares/verifies/tears down the doctor-owned canary DLQ | Same sequence over `Pool` operations: `declareQueue` + `bindQueue` for the canary DLQ, `getMessage(requeue: true)` for the tiered DLQ verification, `clear` + `deleteQueue` for the teardown |
| Topology exchange checks (`rabbit-rs:topology`) | Management API listing: exchanges, bindings, queue arguments | Native passive `Pool::verifyExchange()` per promised route exchange and the dead-letter exchange; bindings and queue arguments are **not verified** (see gaps) |
| Queue existence checks | Native `Pool::size()` (passive probe) either way — never HTTP | Native `Pool::size()` |
| `--fix` topology declaration | Native transient-consumer bring-up either way — never HTTP | Native |
| Queue depth sampling (`QueueDepthSampler`) | Management API per queue: `messages_ready` + `messages_unacknowledged` (the drain check keeps the full pending reading, #308; the scaler's admission gauge reads ready-only, #318) | Native passive `Pool::size()` fallback (since #272) — ready-only, no unacked gauge: the unacked asymmetry applies to the native leg only, see gaps |
| Status command (`rabbit-rs:status`) | Management API for cross-process queue counters | **API-only**: no native equivalent |
| Unroutable stats (`rabbit-rs:doctor` publish outcomes) | Management API `return_unroutable` counter | **API-only**: no native equivalent |
| Connection readiness / broker reachability | Native pool either way | Native |

## Information unavailable through AMQP

These checks require the management API because AMQP does not expose the required information:

1. **Cumulative broker counters** — status `deliver_get`/`ack`/`redeliver` and
   the doctor's `return_unroutable`. AMQP exposes no broker-accumulated
   statistics; the only in-AMQP view is the process-local metrics facade.
2. **Binding enumeration** — topology's binding verification (route binding
   and dead-letter binding checks) lists broker bindings over HTTP. AMQP can
   verify exchange existence passively, but cannot ask "which bindings does
   this exchange have?".
3. **Unacked gauge** — the drain check counts ready + unacked through the
   management API. The native path counts ready only (`basic.get` cannot see
   unacknowledged deliveries); after workers exit, unacked messages are
   redelivered and become ready, so the native reading is approximate for a
   redelivery window after a drain.

## Operational notes

- The native delay probe declares and deletes one throwaway exchange
  (`rabbit-rs.probe.delayed`) per probe. The verdict is cached per connection
  for the process lifetime, so the cost is paid once per process — enabling
  the plugin is a broker administration action, not something a running
  process re-probes.
- The dead-letter canary declares, purges, and deletes its own canary DLQ on
  either path (doctor-owned hygiene, #288): nothing accumulates, and a
  backlog can never wall the verification window off.
- The native canary's configured-DLQ scan window (tier 2) sees the queue head
  only: `basic.get` + requeue returns the head message, so unlike the bulk
  HTTP pull it cannot page past foreign backlog. The decisive tier is the
  doctor-owned canary DLQ, which is empty by construction on both paths.
