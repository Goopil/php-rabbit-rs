# Operations

This guide covers operating Rabbit RS in production: diagnostics, supervisor configuration, Kubernetes deployment, and monitoring.

## Diagnostics

### rabbit-rs:status

The `rabbit-rs:status` command provides a read-only snapshot of the native pool:

```bash
php artisan rabbit-rs:status
```

Output includes:

- **Pool state** — handle ID, PID, closed flag
- **Publisher metrics** — publishes, confirmations, returns, backpressure, reconnects
- **Consumer metrics** — deliveries, acks, rejects
- **Latency** — confirmation and settlement latency at p50/p95/p99

For machine-readable output (useful for monitoring and CI):

```bash
php artisan rabbit-rs:status --format=json
```

The status command is read-only. It does not reconnect, modify topology, or consume messages.

### Verifying the extension

```bash
php --ri rabbit_rs
```

This shows the extension version and configuration. If the extension is not loaded, check your PHP configuration:

```bash
php -m | grep rabbit_rs
```

## rabbit-rs:work supervisor

The `rabbit-rs:work` command supervises `queue:work` child processes across connections. With no flags it **fans out**: one `queue:work` child per rabbit-rs connection, each consuming every queue defined on its connection (its `queue` key first, then its `subscriptions` queues); `--workers` spawns children per connection:

```bash
php artisan rabbit-rs:work --workers=4
```

`--queue=x,y` resolves each name **by definition**: a name matches a connection's `queue` key or one of its `subscriptions` aliases, every (connection, queue) pair whose definition matches is consumed, and an unknown name fails with a typed error listing the available queues. Combining `--connection` and `--queue` intersects both filters.

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `--connection` | Comma-separated connection names | Every rabbit-rs connection |
| `--queue` | Comma-separated queue names, resolved by definition (connection `queue` key or `subscriptions` alias) | Every defined queue |
| `--workers` | Child workers per connection | `1` |
| `--max-restarts` | Max restarts per worker before giving up | `3` |
| `--backoff` | Base backoff in seconds (doubles on each restart, max 60) | `1` |
| `--timeout`, `--tries`, `--memory`, `--max-jobs`, `--max-time` | Propagated to each `queue:work` child | `60`, `—`, `128`, `—`, `—` |
| `--rabbit-rs-worker` | Worker index (set by the supervisor, not by users) | — |

### Signal handling

| Signal | Behavior |
|--------|----------|
| `SIGTERM` | Graceful shutdown — stop all children, wait for current jobs |
| `SIGINT` | Same as `SIGTERM` |

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Clean shutdown |
| `1` | Max restarts exceeded |
| `130` | Signal received |

### How it works

1. The supervisor spawns one child per targeted connection (× `--workers`), each running `php artisan queue:work <name> --queue=<q1,q2>` (the connection is `queue:work`'s positional argument)
2. Each child gets a unique `--name=worker-{i}` and the `RABBIT_RS_WORKER={i}` environment variable
3. The supervisor monitors child processes every 100ms
4. If a child exits unexpectedly, the supervisor waits (backoff seconds) and restarts it
5. On `SIGTERM`/`SIGINT`, the supervisor sends `SIGTERM` to each child and waits up to 10 seconds

## Supervisor (systemd) configuration

For production, run `rabbit-rs:work` under systemd or Supervisor to ensure it restarts on crash.

### systemd

```ini
[Unit]
Description=Rabbit RS Worker
After=network.target rabbitmq-server.service

[Service]
Type=simple
User=www-data
Group=www-data
WorkingDirectory=/var/www/html
ExecStart=/usr/bin/php artisan rabbit-rs:work --workers=4 --max-restarts=0
Restart=always
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=30

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=rabbit-rs-worker

# Resource limits
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

Set `--max-restarts=0` to disable the internal restart limit and let systemd handle restarts.

### Supervisor

```ini
[program:rabbit-rs-worker]
command=php /var/www/html/artisan rabbit-rs:work --workers=4 --max-restarts=0
directory=/var/www/html
user=www-data
autostart=true
autorestart=true
stopwaitsecs=30
stopasgroup=true
killasgroup=true
redirect_stderr=true
stdout_logfile=/var/log/rabbit-rs/worker.log
```

A complete Supervisor config is provided in [`examples/laravel/worker-supervisor.conf`](../examples/laravel/worker-supervisor.conf).

## Kubernetes deployment

### Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: rabbit-rs-worker
spec:
  replicas: 3
  selector:
    matchLabels:
      app: rabbit-rs-worker
  template:
    metadata:
      labels:
        app: rabbit-rs-worker
    spec:
      containers:
        - name: worker
          image: your-app:latest
          command: ["php", "artisan", "rabbit-rs:work", "--workers=2"]
          env:
            - name: RABBIT_RS_HOSTS
              value: "rabbitmq-0:5672,rabbitmq-1:5672,rabbitmq-2:5672"
            - name: RABBIT_RS_VHOST
              value: "/production"
            - name: RABBIT_RS_USERNAME
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: username
            - name: RABBIT_RS_PASSWORD
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: password
          resources:
            requests:
              cpu: 500m
              memory: 256Mi
            limits:
              cpu: 2000m
              memory: 512Mi
      terminationGracePeriodSeconds: 60
```

### Key considerations

- **`terminationGracePeriodSeconds`** — set to at least 60 seconds to allow graceful shutdown
- **Replicas** — each pod runs its own PHP process with its own connection pool; RabbitMQ handles load balancing across consumers
- **Resource limits** — each worker process uses ~50-100 MB; account for `--workers` multiplied by per-worker memory
- **Probes** — Rabbit RS does not expose HTTP health endpoints; use process liveness or a custom health check script

### Graceful shutdown in Kubernetes

Kubernetes sends `SIGTERM` to the container's PID 1. The supervisor handles this signal, stops child workers gracefully, and exits with code 0. The `terminationGracePeriodSeconds` should exceed the maximum job duration plus shutdown time.

## Monitoring with Prometheus

Rabbit RS does not include a Prometheus exporter in V1, but the status command provides the metrics needed. You can scrape them with a custom exporter or sidecar.

### Available metrics

| Metric | Description |
|--------|-------------|
| `publishes_total` | Total published messages |
| `confirmations_total` | Total publisher confirms (ACK) |
| `returns_total` | Total mandatory returns (unroutable) |
| `backpressure_total` | Times publisher capacity was reached |
| `reconnects_total` | Total connection recoveries |
| `deliveries_total` | Total deliveries received |
| `acks_total` | Total consumer ACKs |
| `rejects_total` | Total consumer rejects |
| `dropped_publications_total` | Publications discarded without confirmed delivery (deadline-expired flush retries, un-attempted batches on a closing pool, unconfirmed leftovers at teardown) |
| `publication_retries_total` | Publications whose deadline expired during a recovery suspension and were re-armed once |
| `confirmation_latency_p50/p95/p99` | Publisher confirmation latency (ms) |
| `settlement_latency_p50/p95/p99` | Consumer settlement latency (ms) |

### Sidecar exporter

```yaml
# A simple sidecar that polls rabbit-rs:status --format=json
# and exposes /metrics in Prometheus format
apiVersion: v1
kind: ConfigMap
metadata:
  name: rabbit-rs-exporter
data:
  exporter.sh: |
    #!/bin/bash
    while true; do
      php artisan rabbit-rs:status --format=json > /tmp/stats.json
      sleep 5
    done
```

Alternatively, listen for the `ConnectionStateChanged` and `BackpressureDetected` events and push metrics to your monitoring system. Native events fire during publish and consume operations (`publish()`, `publishBatch()`, consumer `next()`/`tryNext()`/`nextBatch()`, and `stats()`), so no polling is required:

```php
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;

Event::listen(ConnectionStateChanged::class, function (ConnectionStateChanged $e) {
    // Push to Prometheus, Datadog, etc.
});

Event::listen(BackpressureDetected::class, function (BackpressureDetected $e) {
    // Alert on backpressure
});
```

### RabbitMQ-native metrics

For cluster-level metrics (queue depth, consumer count, node health), use the [RabbitMQ Prometheus exporter](https://github.com/rabbitmq/rabbitmq-prometheus-plugin) that ships with RabbitMQ.

## Backpressure detection and response

Backpressure occurs when the publisher's bounded capacity is reached. This happens when the broker cannot confirm publications as fast as they are produced.

### Detection

The `BackpressureDetected` event is dispatched with:

- `broker` — the broker name
- `inFlight` — current in-flight publications
- `capacity` — maximum capacity

### Response strategies

1. **Reduce publish rate** — batch jobs, add delays between batches, or use rate limiting
2. **Scale workers** — add more consumer processes to drain queues faster
3. **Check broker health** — high backpressure may indicate broker overload or network issues
4. **Monitor confirmation latency** — rising `confirmation_latency_p95/p99` indicates broker saturation

### Backpressure vs. connection loss

Backpressure is not an error — the publisher continues accepting commands, but new publish calls receive a `BackpressureException` when capacity is full. This is a signal to slow down, not a failure. Connection loss, by contrast, triggers the recovery and replay mechanism.

See [Reliability](reliability.md) for the full publisher safety model.
