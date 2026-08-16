# Troubleshooting

## Common errors and solutions

### Extension not loaded

**Error:**
```
The Rabbit RS Laravel driver requires ext-rabbit_rs ^1.0 to be loaded.
```

**Solution:**

Install the native extension:

```bash
pie install goopil/rabbit-rs-native
```

Verify it is loaded:

```bash
php -m | grep rabbit_rs
php --ri rabbit_rs
```

If the extension is installed but not loaded, check your PHP configuration:

```bash
# Find the PHP config directory
php --ini

# Check if the extension is enabled
grep rabbit_rs /path/to/php.ini
# Should show: extension=rabbit_rs
```

### Connection failures

**Error:**
```
ConnectionException: Failed to connect to broker
```

**Diagnosis:**

1. Verify RabbitMQ is reachable:

```bash
# Check TCP connectivity
nc -zv rabbit-host 5672

# Check AMQP handshake (if rabbitmqadmin is installed)
rabbitmqadmin --host=rabbit-host --port=5672 list vhosts
```

2. Check credentials and vhost:

```bash
# Verify vhost exists
rabbitmqctl list_vhosts | grep '/your-vhost'

# Verify user permissions
rabbitmqctl list_permissions -p /your-vhost
```

3. Check TLS configuration:

```bash
# Test TLS connection
openssl s_client -connect rabbit-host:5671 -CAfile /path/to/ca.pem
```

4. Check the status command:

```bash
php artisan rabbit-rs:status
```

**Common causes:**

| Cause | Solution |
|-------|----------|
| Wrong host/port | Check `RABBIT_RS_HOSTS` env var |
| Wrong vhost | Check `RABBIT_RS_VHOST` — vhosts are case-sensitive |
| Wrong credentials | Check `RABBIT_RS_USERNAME` and `RABBIT_RS_PASSWORD` |
| TLS mismatch | Ensure `RABBIT_RS_TLS=true` when broker requires TLS |
| Firewall | Ensure port 5672 (or 5671 for TLS) is open |
| RabbitMQ not running | Start the RabbitMQ service |

### Topology errors

**Error:**
```
PRECONDITION_FAILED - inequivalent arg 'x-queue-type' for queue 'orders'
```

**Cause:** The queue exists with different arguments than what Rabbit RS is trying to declare.

**Solutions:**

1. **Use `verify` mode** to check without modifying:

```bash
RABBIT_RS_TOPOLOGY_MODE=verify
```

2. **Use `external` mode** if an external system manages topology:

```bash
RABBIT_RS_TOPOLOGY_MODE=external
```

3. **Delete and recreate the queue** (data loss — use with caution):

```bash
rabbitmqctl delete_queue orders
```

4. **Align your config** with the existing queue arguments (type, durability, delivery_limit).

**Error:**
```
NOT_FOUND - no exchange 'laravel.jobs'
```

**Cause:** In `external` or `verify` mode, the exchange does not exist.

**Solution:** Switch to `declare` mode, or create the exchange manually:

```bash
rabbitmqadmin declare exchange name=laravel.jobs type=direct durable=true
```

### Permission errors

**Error:**
```
ACCESS_REFUSED - access to queue 'orders' refused
```

**Cause:** The configured user lacks permissions on the vhost or queue.

**Solution:**

```bash
# Grant permissions
rabbitmqctl set_permissions -p /your-vhost username ".*" ".*" ".*"

# For read-only (verify mode)
rabbitmqctl set_permissions -p /your-vhost username "^amq\.|^laravel\." "^amq\.|^laravel\." ".*"
```

**Error:**
```
ACCESS_REFUSED - access to vhost '/production' refused
```

**Cause:** The user does not have access to the vhost.

**Solution:**

```bash
# Grant vhost access
rabbitmqctl set_vhost_permissions -p /production username ".*" ".*" ".*"
```

### Recovery diagnostics

**Symptom:** The worker reconnects frequently (high `reconnects_total`).

**Diagnosis:**

```bash
php artisan rabbit-rs:status
```

Check:
- `reconnects_total` — if increasing, the connection is unstable
- `backpressure_total` — if high, the broker may be overloaded
- `confirmation_latency_p99` — if high, the broker is slow to confirm

**Common causes:**

| Cause | Diagnostic | Solution |
|-------|------------|----------|
| Heartbeat timeout | Check `RABBIT_RS_HEARTBEAT` (default 30s) | Increase heartbeat or check network |
| Network instability | Check for packet loss, DNS issues | Fix network or use direct IP |
| Broker overload | Check RabbitMQ management UI | Scale RabbitMQ or reduce publish rate |
| Firewall idle timeout | Connection drops after N seconds of idle | Reduce heartbeat below the idle timeout |

**Symptom:** Messages are redelivered after recovery.

**This is expected behavior.** When a connection drops, RabbitMQ redelivers unacked messages. The worker may process the same message twice. Ensure your jobs are idempotent. See [Reliability — Duplicates](reliability.md#duplicates).

### Debug logging

Enable debug logging by listening to events:

```php
// In a service provider or EventServiceProvider
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;
use Illuminate\Support\Facades\Log;

Event::listen(ConnectionStateChanged::class, function (ConnectionStateChanged $event) {
    Log::debug("Rabbit RS: broker {$event->broker} → {$event->state} (gen {$event->generation})");
});

Event::listen(BackpressureDetected::class, function (BackpressureDetected $event) {
    Log::warning("Rabbit RS: backpressure on {$event->broker}: {$event->inFlight}/{$event->capacity}");
});
```

### Queue depth monitoring

Check queue depth:

```bash
# Via Rabbit RS
php artisan tinker
>>> Queue::connection('rabbit-rs')->size('orders.high')

# Via rabbitmqctl
rabbitmqctl list_queues -p /your-vhost name messages
```

### Stale ACK rejection

**Symptom:** Log shows stale generation warnings during recovery.

**This is expected behavior.** After a connection recovery, ACKs from the old generation are rejected to prevent double-settlement. RabbitMQ redelivers the message. The job may execute twice — ensure idempotency. See [Reliability — Stale ACK rejection](reliability.md#delivery-tokens-and-stale-ack-rejection).

### Backpressure

**Symptom:** `BackpressureException` thrown during publish.

**Cause:** The publisher's bounded capacity is full (in-flight confirms + replay buffer).

**Solutions:**

1. Reduce publish rate — batch jobs, add delays
2. Scale consumers — more workers drain queues faster
3. Increase `max_in_flight` (if memory allows)
4. Check broker health — high confirmation latency indicates broker saturation

See [Operations — Backpressure](operations.md#backpressure-detection-and-response).

### Delayed messages not arriving

**Symptom:** `later()` jobs are not delivered after the delay.

**Diagnosis:**

1. Check the delay mode:

```bash
RABBIT_RS_DELAY_MODE=auto
```

2. If using `plugin` mode, verify the plugin is installed:

```bash
rabbitmq-plugins list | grep delay
# Should show: rabbitmq_delayed_message_exchange
```

3. If using `ttl` mode, check the TTL queues exist:

```bash
rabbitmqctl list_queues -p /your-vhost name messages | grep delay
```

4. Check the delay buckets — delays are rounded up to the nearest bucket:

```bash
# With buckets [1, 5, 30, 120]
# A 3-second delay → bucket 5 (delivered after ~5 seconds)
```

### Getting help

If you cannot resolve an issue:

1. Run `php artisan rabbit-rs:status --format=json` and save the output
2. Run `php --ri rabbit_rs` and save the output
3. Check [Reliability](reliability.md) for delivery semantics
4. Check the [troubleshooting checklist](https://github.com/Goopil/rabbit-rs/issues) for known issues
5. Open an issue on [GitHub](https://github.com/Goopil/rabbit-rs/issues) with the diagnostic output
