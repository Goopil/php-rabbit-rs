# Operations Alert Rules

Example Prometheus rules for the Rabbit RS signals. They reference only the
documented collection surfaces — the sidecar exporter polling
`php artisan rabbit-rs:status --format=json` (per-process `Pool::stats()`
fields, exposed below as `rabbit_rs_<field>`) and the RabbitMQ Prometheus
plugin / management API (cluster-level `rabbitmq_*` series). Playbooks for
each rule live in [runbook.md](runbook.md).

**Conventions and caveats.**

- Per-process native metrics are per PHP worker; aggregate across instances
  with `sum()` / `max()` as noted per rule. Values read zero in a fresh CLI
  process — a restarted worker resets its counters by design.
- The `rabbitmq_*` per-queue gauges require the management metrics collector
  ENABLED in production (the `rabbitmq_prometheus` plugin, or the management
  API with stats collection active). The lab's `/api/queues` gauge can lag or
  be omitted on fresh queues until the collector emits a first sample — the
  same caveat as the FPM harness observer in `scripts/test-fpm.sh`. An absent
  gauge is not a zero: prefer `absent()` alerts over treating gaps as idle.
- Duplicate signals are deliberately nuanced: `duplicates_total` is
  per-process and exact; the broker's `messages_redelivered` is an
  APPROXIMATE cross-process duplicate signal that also counts crash requeues
  and stale-ACK redeliveries. Never alert on redeliveries alone.

```yaml
groups:
  - name: rabbit-rs-data-safety
    rules:
      # PAGE IMMEDIATELY — silent loss after confirmed-path acceptance is a
      # data-loss bug by contract (runbook incident 3). Any increase on any
      # worker instance blocks everything else.
      - alert: RabbitRsDroppedPublications
        expr: sum(increase(rabbit_rs_dropped_publications_total[5m])) > 0
        for: 0m
        labels:
          severity: critical
        annotations:
          summary: "rabbit-rs dropped publications (data loss under the confirmed path)"
          description: >-
            A publication accepted by the confirmed publish path was dropped.
            Capture Pool::stats() and drainErrors() records before restarting
            the worker. See docs/operations/runbook.md incident 3.

      # Buffer must quiesce to zero when the process is idle or after
      # flush(): a plateau is a re-buffer leak or a confirm stall (incident 4).
      - alert: RabbitRsPublishBufferStuck
        expr: min_over_time(sum by (instance) (rabbit_rs_publish_buffered)[30m:5m]) > 0
        for: 30m
        labels:
          severity: critical
        annotations:
          summary: "rabbit-rs publish buffer has not drained for 30m"
          description: >-
            publish_buffered stayed above zero across 30 minutes. Compare with
            publish_buffered_bytes and confirmations_total to distinguish a
            confirm stall from payload accumulation. See runbook incident 4.

  - name: rabbit-rs-capacity
    rules:
      # Producers hit the bounded publisher budget. Warning on first hits,
      # critical when it keeps climbing (incident 2).
      - alert: RabbitRsBackpressure
        expr: sum(increase(rabbit_rs_backpressure_total[10m])) > 0
        for: 0m
        labels:
          severity: warning
        annotations:
          summary: "rabbit-rs publishers hit backpressure"
          description: >-
            backpressure_total increased: the publisher budget (1024
            publications / 64 MiB by default) was exhausted. Slow producers or
            scale consumers; check confirmation_latency_p99 for broker
            saturation. See runbook incident 2.

      - alert: RabbitRsBackpressureSustained
        expr: sum(increase(rabbit_rs_backpressure_total[30m])) > 0
        for: 30m
        labels:
          severity: critical
        annotations:
          summary: "rabbit-rs backpressure is sustained"
          description: >-
            Backpressure repeated over 30 minutes — steady-state load exceeds
            the confirm pipeline. See runbook incident 2.

  - name: rabbit-rs-connectivity
    rules:
      # Reconnect storm: occasional reconnects are expected; a cluster of
      # them inside 15m means broker or network instability (incident 1).
      # Each recovery can also produce duplicates via replay.
      - alert: RabbitRsReconnectStorm
        expr: sum(increase(rabbit_rs_reconnects_total[15m])) > 5
        for: 0m
        labels:
          severity: warning
        annotations:
          summary: "rabbit-rs reconnect storm (more than 5 recoveries in 15m)"
          description: >-
            reconnects_total is climbing across workers. Probe with
            `php artisan rabbit-rs:doctor`, check broker health, and expect a
            correlated duplicates_total bump. See runbook incident 1.

      # Publications re-armed after a recovery suspension: acceptable in
      # small numbers, a replay-path problem in bulk (incident 1).
      - alert: RabbitRsPublicationRetries
        expr: sum(increase(rabbit_rs_publication_retries_total[1h])) > 100
        for: 0m
        labels:
          severity: warning
        annotations:
          summary: "rabbit-rs re-armed publications after recovery suspensions"
          description: >-
            publication_retries_total grew: publications whose deadline
            expired while parked during a recovery were re-armed with a fresh
            deadline. Bulk retries mean recovery windows are eating the
            publication deadlines. See runbook incident 1.

  - name: rabbit-rs-duplicates
    rules:
      # Duplicates spike (incident 5). duplicates_total is per-process and
      # exact; sum across workers, and correlate with reconnects_total and
      # consumer crash logs before treating it as a driver bug — at-least-once
      # delivery produces duplicates by design after recoveries and crashes.
      - alert: RabbitRsDuplicatesSpike
        expr: |
          sum(increase(rabbit_rs_duplicates_total[15m]))
            /
          clamp_min(sum(increase(rabbit_rs_deliveries_total[15m])), 1)
            > 0.05
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: "rabbit-rs duplicates exceeded 5% of deliveries"
          description: >-
            Per-process duplicates_total vs deliveries_total. CAVEAT: the
            cross-process broker signal messages_redelivered is approximate
            (it also counts crash requeues and stale-ACK redeliveries) — use
            it for correlation only, never as the alert condition. Verify job
            idempotency. See runbook incident 5.

  - name: rabbit-rs-latency
    rules:
      # Settlement latency p99 (ms gauge exported from the sidecar). Warning
      # at a deliberately generous 5s so only real stalls fire (incident 7).
      - alert: RabbitRsSettlementLatencyHigh
        expr: max(rabbit_rs_settlement_latency_p99) > 5000
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: "rabbit-rs settlement p99 above 5s"
          description: >-
            settlement_latency_p99 is high across worker instances. Scale
            consumers or raise prefetch; correlate with reconnects_total —
            recovery suspensions show up here. See runbook incident 7.

  - name: rabbitmq-broker
    rules:
      # Broker availability (incident 1). Requires the RabbitMQ Prometheus
      # plugin or the management metrics collector ENABLED — see the caveat
      # at the top of this file.
      - alert: RabbitmqNodeDown
        expr: up{job="rabbitmq"} == 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "RabbitMQ node unreachable"
          description: >-
            The broker scrape is down. rabbit-rs workers buffer publications
            in bounded memory during the outage; watch publish_buffered and
            expect replays after recovery. Probe workers with
            `php artisan rabbit-rs:doctor`. See runbook incident 1.

      # Sustained queue depth growth: consumers are not keeping up. The
      # per-queue gauge requires the management collector enabled; a fresh
      # queue can omit the gauge until the first stats sample.
      - alert: RabbitmqQueueDepthGrowing
        expr: >-
          rabbitmq_queue_messages{queue=~"queues\..*"} > 10000
        for: 30m
        labels:
          severity: warning
        annotations:
          summary: "queue depth above 10k for 30m"
          description: >-
            Depth is climbing on the broker across all processes. Check
            consumer throughput and incident 7; note the collector caveat —
            an absent gauge is not an empty queue.

      # DLQ growth: poison messages terminal-settled (incident 6).
      - alert: RabbitmqDeadLetterQueueGrowing
        expr: >-
          rabbitmq_queue_messages{queue=~".*\.dlq"} > 0
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: "dead-letter queue is not empty"
          description: >-
            Messages hit their attempts cap and were terminal-settled to the
            DLQ. Inspect payloads and requeue or discard deliberately. See
            runbook incident 6.
