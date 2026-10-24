# Management API usage: hybrid behavior

Rabbit RS uses the RabbitMQ management API (`management_url`) when a
connection configures it, and falls back to native AMQP operations through
the `rabbit_rs` extension when it does not. This page records, per feature,
which path runs with and without `management_url`, and the gaps that are
protocol-impossible in pure AMQP.

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

## Protocol-impossible gaps

These cannot be replicated in pure AMQP, so they stay management-API-only:

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
