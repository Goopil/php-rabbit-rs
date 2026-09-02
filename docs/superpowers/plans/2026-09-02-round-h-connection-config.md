# Round H — Connection-first config (v0.1.0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three-namespace `config/rabbit-rs.php` (brokers/routes/workers) with connection-first `queue.connections.*` config, compiled lazily per connection — closing F-27 (boot blast radius), F-28 (Octane stale config), and delivering the §5 `rabbit-rs:work` fan-out.

**Architecture:** A new `ConnectionCompiler` turns one `queue.php` connection array (+ package defaults) into exactly today's normalized native shape, so `RabbitMqQueue`, `NativePoolFactory`, `MessageMapper`, and the extension are untouched. `ConfigNormalizer` and the `rabbit-rs.config` singleton are deleted; compilation happens inside `RabbitMqConnector::connect()` per connection, per process.

**Tech Stack:** PHP 8.4+, Laravel queue API, Pest. Spec: `docs/superpowers/specs/2026-08-31-laravel-config-redesign-design.md` (§5 amended after owner review).

## Global Constraints

- No core (`crates/rabbit-rs-core`) or extension (`crates/rabbit-rs-php`) changes. The compiler emits exactly today's native shape (verified against `ConfigNormalizer::normalize()` output before deletion).
- Breaking 0.x change: NO compatibility shim for the old `rabbit-rs.php` shape. Documented as v0.1.0 in `CHANGELOG.md` with a migration table.
- All config errors: `InvalidArgumentException` with fully qualified path `queue.connections.<name>.<key>`.
- Env-string casting (spec §4): booleans `true/"1"/"true"/"on"/"yes"` / `false/"0"/"false"/"off"/"no"/""/null→default`; integers `/^-?\d+$/` then range-checked; anything else → typed error.
- Laravel Unit/Feature tests run WITHOUT the extension (the "missing extension" assertion in `RabbitMqServiceProviderTest` must keep passing). Integration tests need ext-rabbit_rs + lab.
- English only. Conventional commits ≤70 chars. Gate: `rtk ./scripts/check.sh` green at every task end. Do not edit `docs/plans/ROADMAP.md` (coordinator consolidates).
- Rust side untouched: `rtk cargo test --workspace` must stay green throughout.

## Design decisions locked (from owner review of spec §5)

1. **Fan-out mechanism**: `rabbit-rs:work` spawns **one `queue:work` child per targeted connection** (framework constraint: `WorkCommand` resolves a single connection; `QueueManager::resolve()` calls `connect($config)` without the name). Within a connection, the native worker profile spans all its queues (native `weighted_fair` scheduling). Cross-connection fairness is process-level, same as every Laravel driver. The spec §5 sentence "one native profile spanning them" is amended at execution time to reflect this (behavior unchanged).
2. **Connection name at compile time**: `QueueManager::resolve()` does `getConnector($config['driver'])->connect($config)->setConnectionName($name)` — the creator closure is invoked with no arguments. The connector recovers the name by reverse lookup (`array_search($config, config('queue.connections'), true)`); two connections with identical arrays compile identically (same fingerprint/pool — a spec §8 feature), so sharing the first-found name is harmless and documented.
3. **Subscription alias** of the derived single subscription: `'default'` (escape-hatch aliases are the subscription keys). Native consumer tag becomes `rabbit-rs.default` for derived profiles.
4. **Routes**: one route keyed `'default'` per compiled connection (`route()` falls back to `routes['default']` for every queue on the connection). `{queue}` placeholder keeps per-queue routing keys. The per-queue `routes.*.exchange` capability is dropped with the old shape (§6).
5. **Publisher flags**: `confirms`/`mandatory` are no longer config keys (§6, #78). The compiler derives them from `safety`: `safe → confirms=true, mandatory=true`; `unsafe → confirms=true, mandatory=false`; `blind → confirms=false, mandatory=false`.
6. **`management_url`** (from #81) moves to the connection: optional, validated, Laravel-only (never propagated to native config); `rabbit-rs:status` reads it from `queue.connections.<name>`.

---

### Task 1: ConnectionCompiler — connection keys, env casting, derived profile

**Files:**
- Create: `packages/laravel-queue/src/Config/ConnectionCompiler.php`
- Test: `packages/laravel-queue/tests/Unit/ConnectionCompilerTest.php`

**Interfaces:**
- Produces: `ConnectionCompiler::compile(string $name, array $config, array $defaults = []): array` returning exactly the shape `ConfigNormalizer::normalize()` returns today (`native`, `routes`, `publisher`, `topology`, `best_effort`, `auto_subscribe`) with one broker, one route, one worker profile. `$defaults` is unused until Task 2 (parameter exists from day one).

**Reference shape to emit** (from today's normalize(), single-connection view):

```php
// native.brokers[0]
['name' => $name, 'hosts' => [['host' => '127.0.0.1', 'port' => 5672]], 'vhost' => '/',
 'credentials' => ['username' => 'guest', 'password' => 'guest'],
 'tls' => ['enabled' => false, 'ca_cert' => null, 'client_cert' => null, 'client_key' => null],
 'heartbeat' => 30]
// native.workers[0]
['name' => $name, 'subscriptions' => [['name' => 'default', 'broker' => $name, 'queue' => 'default',
  'weight' => 1, 'priority_class' => 0, 'prefetch' => 64, 'starvation_after' => 30,
  'early_ack' => false, 'no_ack' => false]], 'scheduler' => ['strategy' => 'weighted_fair']]
// routes
['default' => ['broker' => $name, 'exchange' => 'laravel.jobs', 'routing_key' => '{queue}']]
// publisher (derived from safety — see decision 5)
['safety' => 'safe', 'confirms' => true, 'mandatory' => true, 'confirm_timeout' => 30000]
// topology
['queue' => ['type' => 'quorum', 'durable' => true, 'delivery_limit' => null], 'dead_letter' => null]
// plus native keys: topology_mode, delay, dead_letter, delivery_limit, consumer, queue_type, queue_durable
// plus top-level best_effort, auto_subscribe
```

Reuse the endpoint parser (IPv6 brackets, port validation), `tls`, `delay`, `topology` (dead_letter + delivery_limit cross-rule), and `consumers` (wait_timeout/max_attempts) validation logic from `ConfigNormalizer` by **copying the private methods into the compiler** (the normalizer is deleted in Task 4; keep the exact messages, but paths become `queue.connections.<name>....`). Do not require ConfigNormalizer at runtime.

- [ ] **Step 1: Write the failing unit tests** (`ConnectionCompilerTest`) covering, each with exact input + expected output array:
  - full connection compiles to the exact reference shape above;
  - minimal connection (`driver`, `queue` only) applies defaults for every omitted key;
  - `hosts` as flat string `"a:5672,b:5672"` → two endpoints sorted; array of strings accepted; `[::1]:5672` IPv6; port out of range → error `queue.connections.orders.hosts.0`;
  - env booleans: `true`, `"1"`, `"true"`, `"on"`, `"yes"`, `false`, `"0"`, `"false"`, `"off"`, `"no"`, `""`, `null` on `best_effort`/`after_commit`-adjacent boolean keys; `"maybe"` → typed error;
  - env integers: `"64"`, `"-1"` where ranges allow, `30000` for confirm_timeout bounds (>= 1000); `"abc"` → typed error;
  - `safety` enum + derived `confirms`/`mandatory` per decision 5; invalid enum → error;
  - unknown key inside a known section (e.g. `hosts_extra`) → rejected with exact path;
  - `queue` non-empty string validation; `wait_timeout` bounds 1000..86400000; `prefetch` 1..65535;
  - `management_url`: null/blank ok (not propagated), non-string → error `queue.connections.<name>.management_url`.
- [ ] **Step 2: Run** `rtk vendor/bin/pest tests/Unit/ConnectionCompilerTest.php` — expect FAIL (class not found).
- [ ] **Step 3: Implement** `ConnectionCompiler` minimally (copy validators, adapt paths, derive single profile/route/publisher).
- [ ] **Step 4: Run** focused test — expect PASS.
- [ ] **Step 5: Commit** `feat(laravel): connection compiler for queue connections`

### Task 2: ConnectionCompiler — package defaults deep-merge

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConnectionCompiler.php`
- Test: `packages/laravel-queue/tests/Unit/ConnectionCompilerTest.php` (new cases)

**Interfaces:**
- Consumes: `compile()` from Task 1. `$defaults` = the package config minus `brokers`/`routes`/`workers` (Task 4 feeds it from the rewritten `config/rabbit-rs.php`).

Merge rule (spec §2): per top-level key the connection value wins; for `tls`, `delay`, `dead_letter` the merge is per sub-key (a connection setting only `delay.mode` inherits package `buckets`). Unknown keys in defaults are passed through only for connections that do not define the key themselves and are otherwise ignored.

- [ ] **Step 1: Write failing tests**: defaults fill every gap; per-sub-key `delay` merge (`['mode' => 'ttl']` + package buckets); per-sub-key `tls` merge; `dead_letter` sub-key merge; connection key overrides default wholesale for scalars; defaults ignored when connection defines the key; defaults with unknown top-level key passed through when connection omits it.
- [ ] **Step 2: Run** — expect new cases FAIL.
- [ ] **Step 3: Implement** the merge (recursive only for the three known nested sections).
- [ ] **Step 4: Run** — expect PASS. **Commit** `feat(laravel): deep-merge package defaults in compiler`

### Task 3: ConnectionCompiler — subscriptions escape hatch

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConnectionCompiler.php`
- Test: `packages/laravel-queue/tests/Unit/ConnectionCompilerTest.php` (new cases)

**Interfaces:**
- Produces: when `subscriptions` is present, `native.workers[0].subscriptions` is the escape-hatch list (fields `queue`, `weight`, `priority_class`, `prefetch`, `starvation_after`, `early_ack`, `no_ack`; alias = subscription key; `broker` = the connection name; no `enabled` flag — an empty/disabled set is a config error today, keep that strictness by requiring at least one entry).

- [ ] **Step 1: Write failing tests**: escape hatch replaces the derived subscription (aliases preserved, broker rewritten to the connection); `no_ack` requires `early_ack` + `best_effort` (exact messages from today); `early_ack` without `best_effort` rejected; `weight` 1..65535; `priority_class` i16 bounds; duplicate `queue` across two subscriptions → error; empty `subscriptions` array → error.
- [ ] **Step 2: Run** — expect FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat(laravel): subscriptions escape hatch in compiler`

### Task 4: Cutover — connector, provider, config rewrite, test migration

**Files:**
- Modify: `packages/laravel-queue/src/Connectors/RabbitMqConnector.php`
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php`
- Modify: `packages/laravel-queue/src/Octane/OctaneLifecycle.php`
- Modify: `packages/laravel-queue/src/Console/RabbitMqStatusCommand.php` (raw-config read)
- Rewrite: `packages/laravel-queue/config/rabbit-rs.php` (~40 lines)
- Delete: `packages/laravel-queue/src/Config/ConfigNormalizer.php` (+ its `tests/Unit/ConfigNormalizerTest.php`)
- Migrate: every Unit/Feature test referencing `brokers`/`routes`/`workers` shapes (`grep -rl "brokers\|workers\|'routes'" packages/laravel-queue/tests`)

**Connector change** — lazy compile, per connection:

```php
final class RabbitMqConnector implements ConnectorInterface
{
    public function __construct(
        private readonly NativePoolFactory $pools,
        private readonly array $defaults, // package config minus brokers/routes/workers
        private readonly ?Closure $inProductionEnvironment = null,
        private readonly bool $productionWarningEnabled = true,
    ) {}

    public function connect(array $config): RabbitMqQueue
    {
        $name = $this->connectionName($config); // reverse lookup, decision 2
        $compiled = ConnectionCompiler::compile($name, $config, $this->defaults);
        // same wiring as today but sourced from $compiled:
        return new $class(
            $this->pools->make($compiled['native']),
            $compiled['routes'],
            $defaultQueue,           // from $config['queue'] (framework key, unchanged)
            $dispatchAfterCommit,
            workerProfiles: new WorkerProfileResolver($compiled['native']['workers']),
            blockForMilliseconds: ($blockFor ?? 0) * 1000,
            publisherConfig: $compiled['publisher'],
            autoSubscribe: $compiled['auto_subscribe'],
            hasDeadLetter: $compiled['topology']['dead_letter'] !== null,
        );
    }
}
```

`auto_subscribe`, `worker` (class selection), `queue`, `after_commit`, `block_for`, `production_warning` stay read from the raw `$config` exactly as today (framework keys).

**Provider change**: `register()` keeps `mergeConfigFrom` + `NativePoolFactory` singleton; **deletes** `normalizeBrokerHosts()` and the `rabbit-rs.config` singleton. The extend closure captures `$defaults = Arr::except(config('rabbit-rs'), ['brokers', 'routes', 'workers'])` and passes it to the connector.

**OctaneLifecycle::reload()**: replace `$this->container->forgetInstance('rabbit-rs.config')` with `$this->container->make('queue')->forgetConnections()` (after `flushPoolFactory()`) — resolved connections recompile from current config on next resolution (F-28).

**Status command**: `rawConfig()` currently reads `rabbit-rs.brokers`/`workers` — rewrite `collectQueueStats()` to iterate `queue.connections` where `driver === 'rabbit-rs'`: `management_url` from the connection, credentials from `username`/`password`, vhost from `vhost`; queue list = the connection's defined queues (`queue` key + `subscriptions.*.queue`).

**Config rewrite** (`config/rabbit-rs.php`, ~40 lines): header comment + a single flat defaults array with every cross-cutting key from spec §1 (heartbeat, tls, safety, confirm_timeout, prefetch, wait_timeout, topology_mode, queue_type, queue_durable, delivery_limit, dead_letter, delay, worker, auto_subscribe, production_warning, best_effort). No `brokers`/`routes`/`workers` namespaces.

- [ ] **Step 1: Write failing feature tests first**: connector resolves two connections → two distinct native configs (different brokers); same-config connections produce byte-identical compiled natives (fingerprint); provider registers without the extension (existing assertion untouched); connection with env-string scalars compiles; `rabbit-rs:status` reads `queue.connections.<name>.management_url`.
- [ ] **Step 2: Run** — expect FAIL (shape unchanged so far).
- [ ] **Step 3: Implement** all file changes above; delete normalizer.
- [ ] **Step 4: Migrate every Unit/Feature test** to `queue.connections.*` shape (mechanical: `brokers.default.*` → connection keys; `workers.default.subscriptions` → `subscriptions` escape hatch or omit; `routes.*` → `exchange`/`routing_key` on the connection).
- [ ] **Step 5: Run** full Laravel Unit+Feature without extension — all green. Run `rtk cargo test --workspace` — green (nothing touched).
- [ ] **Step 6: Gate** `rtk ./scripts/check.sh` — PASS. **Commit** `feat(laravel)!: connection-first config (v0.1.0)`

### Task 5: rabbit-rs:work fan-out

**Files:**
- Create: `packages/laravel-queue/src/Console/WorkPlanResolver.php`
- Modify: `packages/laravel-queue/src/Console/RabbitMqWorkCommand.php` (`--connection`/`--queue` defaults change)
- Modify: `packages/laravel-queue/src/Console/WorkerSupervisor.php` (children per connection)
- Test: `packages/laravel-queue/tests/Feature/WorkPlanResolverTest.php` (new), existing work-command tests updated

**Interfaces:**
- Produces: `WorkPlanResolver::resolve(?string $connections, ?string $queues): list<array{connection: string, queues: list<string>}>` — one entry per targeted connection (spec §5 amended semantics).

Resolution rules:
- No flags: targeted = every `queue.connections.*` with `driver === 'rabbit-rs'`, in config order; queues = the connection's defined queues (`queue` key first, then `subscriptions.*.queue` not already listed).
- `--connection=a,b`: targeted = those connections; unknown name → `InvalidArgumentException` listing available rabbit-rs connections.
- `--queue=x,y`: each name resolved BY DEFINITION across targeted connections (connection `queue` key or `subscriptions` key); unknown → error listing all defined queue names; a queue defined on two targeted connections is consumed on both; combining flags intersects (only listed connections that define the listed queues).
- No rabbit-rs connection at all → error listing none.
- Supervisor: children are `queue:work --connection=<c> --queue=<q1,q2>` — one child per plan entry; `--workers` now means per-connection children (N plan entries × `--workers` children). `--workers=1` default keeps one child per connection.
- Command signature defaults: `--connection=null`, `--queue=null` (fan-out default). The `handle()` info line lists the plan.

- [ ] **Step 1: Write failing tests**: no-flag plan over a two-connection config; `--connection` filtering + unknown error; `--queue` by-definition resolution + unknown error with available list; same-name two connections → both entries; intersection; empty rabbit-rs set error; supervisor `buildChildCommands()` returns one command per plan entry with propagated options.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Gate** + **Commit** `feat(laravel): rabbit-rs:work fan-out across connections`

### Task 6: Integration — two connections, two brokers

**Files:**
- Test: `packages/laravel-queue/tests/Integration/TwoConnectionsTest.php` (new)
- Modify: lab fixtures if the second broker/vhost needs provisioning (`lab/` only — no core/ext)

**Interfaces:**
- Consumes: compiled connections from Task 4. Lab creds: `rabbit_rs`/`rabbit_rs_lab`, vhosts `/orders-eu` (+ second vhost from lab definitions).

- [ ] **Step 1: Write failing integration test**: app with two rabbit-rs connections (two distinct brokers/vhosts) → dispatch on connection A, consume on A; dispatch B, consume B; nothing crosses. Second test: two `queue:work` processes consuming one queue (multi-consumer, no tag collision).
- [ ] **Step 2: Run against lab** (`php -d extension=<artifact> vendor/bin/pest tests/Integration/TwoConnectionsTest.php`) — expect FAIL (features not wired) → fix plan-level gaps if any surface.
- [ ] **Step 3: Run** full `./scripts/test-integration.sh` — green.
- [ ] **Step 4: Commit** `test(laravel): two-connection end-to-end integration`

### Task 7: Docs + CHANGELOG v0.1.0

**Files:**
- Rewrite: `docs/configuration.md` (connections with runnable copy-paste examples: single broker, multi-broker, env strings, worker targeting)
- Update: `docs/octane.md` (reload now automatic via `forgetConnections`), `README.md` quickstart, `docs/reliability.md` (status command config key location)
- Update: `CHANGELOG.md` (v0.1.0 entry: breaking config change + migration table old key → new location), `packages/laravel-queue/CHANGELOG.md`

Migration table rows (old → new): `brokers.<b>.hosts/credentials/tls/heartbeat` → connection `hosts`/`username`/`password`/`tls`/`heartbeat`; `routes.<q>.exchange/routing_key` → connection `exchange`/`routing_key` (`{queue}` placeholder unchanged); `workers.<w>.subscriptions.*` → connection `subscriptions`; `workers.<w>` profile targeting → `rabbit-rs:work --connection=<name>`; `publisher.confirms/mandatory` → `safety` only; `scheduler.strategy`, `prefetch.mode` → deleted (dead knobs, §6); `management_url` → connection key.

- [ ] **Step 1: Rewrite docs** (every example copy-pasteable; queue.php connection snippet mirrors spec §1).
- [ ] **Step 2: Write CHANGELOG v0.1.0** with the migration table.
- [ ] **Step 3: Gate** `rtk ./scripts/check.sh` + `rtk composer validate --strict` — PASS. **Commit** `docs(laravel): connection-first configuration guide (v0.1.0)`

---

## Self-review checklist (run before handoff)

- Spec §1 connection keys → Task 1; §2 defaults → Task 2; §3 compiler/connector/provider → Tasks 1-4; §4 env strings → Task 1; §5 work command → Task 5; §6 dropped keys → Task 4 (config rewrite) + Task 7 (table); §7 errors/strictness → Tasks 1-3; §8 testing → Tasks 1-6; §9 docs → Task 7.
- Type consistency: `compile(string $name, array $config, array $defaults = []): array` used identically in Tasks 2-4; plan resolver returns `list<array{connection: string, queues: list<string>}>` in Task 5 both for resolver and supervisor.
- No placeholders: every task names exact files, exact test cases with expected values, and exact commands.
