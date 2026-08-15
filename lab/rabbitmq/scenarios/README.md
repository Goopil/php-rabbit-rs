# Chaos / Fault Injection Scenarios

This directory contains scenario definitions for the at-least-once chaos
test suite. Each scenario injects a specific failure into the RabbitMQ
cluster and verifies that no messages are lost.

## Running

```bash
./scripts/test-chaos.sh
```

The script starts the lab if needed, resets Toxiproxy, runs each Rust
scenario individually (to avoid interference), then runs the Laravel chaos
tests, and tears down the lab if it started it.

## Delivery contract

The contract is **at-least-once**:

- `missing = 0` is mandatory — a missing message is a test failure.
- Duplicates are permitted only in documented ambiguous windows (e.g. TCP
  reset after the broker accepted a message but before the publisher confirm
  reached the client).

## Scenarios

| # | Scenario (FR) | English | Fault | Expected |
|---|---|---|---|---|
| 1 | reset TCP avant confirm | TCP reset before publisher confirm | Toxiproxy `reset_peer` on proxy-1 | Message delivered after recovery |
| 2 | reset TCP après confirm avant ACK | TCP reset after confirm before ACK | Toxiproxy `reset_peer` on proxy-1 | Unacked message redelivered |
| 3 | arrêt du leader quorum | Quorum leader shutdown | `docker stop` leader node | Messages survive failover |
| 4 | redémarrage d'un nœud | Node restart | `docker stop` + `docker start` | Messages survive restart |
| 5 | partition du consumer | Consumer network partition | Toxiproxy `timeout` on proxy-1 | Unacked message redelivered |
| 6 | channel fermé pour erreur de topologie | Channel closed for topology error | Force reconnection via pool close | New channel delivers messages |
| 7 | plugin delay indisponible | Delay plugin unavailable | Verify with/without plugin profile | Regular publish/consume works |
| 8 | credentials refusés | Credentials rejected | Bad password in config | Typed error; good creds still work |
| 9 | SIGTERM du worker avec jobs non acquittés | Worker SIGTERM with unacked jobs | Close pool without ACK | Unacked message redelivered |

## Toxiproxy

The lab runs Toxiproxy with 3 proxies:

| Proxy | Listen | Upstream | Maps to |
|---|---|---|---|
| `rabbitmq-1-toxiproxy` | `:5672` | `rabbitmq-1:5672` | Node 1 |
| `rabbitmq-2-toxiproxy` | `:5673` | `rabbitmq-2:5672` | Node 2 |
| `rabbitmq-3-toxiproxy` | `:5674` | `rabbitmq-3:5672` | Node 3 |

Toxiproxy API: `http://localhost:8474`

### Available toxic types

- `reset_peer` — sends a TCP reset after `timeout` ms
- `timeout` — blocks all traffic (simulates partition)
- `latency` — adds `latency` ms delay
- `bandwidth` — limits bandwidth to `rate` KB/s

## Per-scenario assertions

Each scenario counts:
- **expected** — the set of message IDs that should be delivered
- **unique** — distinct message IDs actually received
- **duplicates** — total received minus unique
- **missing** — expected IDs that were never received

The assertion is: `missing.len() == 0`.
