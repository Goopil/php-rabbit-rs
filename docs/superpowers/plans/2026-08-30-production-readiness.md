# Production Readiness — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Corriger les défauts bloquants identifiés par l'audit du 30 août 2026 et amener l'écosystème Rabbit RS (core Rust, extension PHP, package Laravel) au niveau production-ready / v1.0.

**Architecture:** Trois couches : `rabbit-rs-core` (crate Rust indépendant), `rabbit-rs-php` (extension ext-php-rs), `goopil/rabbit-rs-laravel` (driver Laravel). Chaque tâche respecte la séparation des couches : le core ne connaît pas PHP, l'extension ne transporte que des valeurs possédées, le package Laravel n'accède au natif que via l'API des stubs.

**Tech Stack:** Rust 1.96 (edition 2024, Tokio, Lapin 4.10, flume), ext-php-rs 0.15.15, PHP 8.4/8.5, Laravel 12/13, Pest, Orchestra Testbench, Docker Compose (lab RabbitMQ 3 nœuds).

**Audit source:** évaluation du 30 août 2026 (voir `docs/plans/ROADMAP.md`, Round F, pour le résumé par couche et les notes de maturité).

> **Réconciliation 2026-08-30 (après merge PR #35 / post-pump sur main) :** la
> composition multi-broker consumer a atterri sur main (Phase D, commit `585c534`,
> `ConsumerHandle` composé dans `consumer/composite.rs`, `ConsumerSetHandle` par
> broker dans `consumer/set.rs`) — l'ancienne Task 8 est marquée livrée. La pump v2
> a modifié les chemins publisher (`publish_blind` désormais à
> `publisher/actor.rs:229`, budget à `publisher/mod.rs:245`). La Task 1 sert
> d'hypothèse n°1 à l'investigation Round 2 P1 (stall ack-pipeline,
> `docs/plans/2026-08-30-consumer-stall-and-reliability.md`). L'ordre d'exécution
> convenu : Round 2 (avec Task 1) → Tasks 2-6 (P0) → Round C → Tasks 7-14 (P1) →
> Round E → Round D.

## Global Constraints

- Rust 1.96, edition 2024, `#![forbid(unsafe_code)]` — jamais d'unsafe, jamais d'affaiblissement des lints workspace.
- TDD obligatoire pour tout changement de comportement : test écrit d'abord, observé en échec, implémentation minimale, re-exécution.
- Aucun sleep réel dans les tests Rust : temps Tokio suspendu (`#[tokio::test(start_paused = true)]`) + mock transport scriptable.
- Aucune valeur Zend, objet PHP, callback ou état de conteneur Laravel retenu dans un thread Rust.
- Livraison at-least-once : aucune perte silencieuse ; les doublons sont autorisés, identifiés et mesurables.
- Les secrets (credentials, URI complète, certificats) ne fuient jamais dans `Debug`, erreurs, métriques ou logs.
- Avant de clore chaque tâche : `rtk cargo fmt --all` puis `rtk ./scripts/check.sh` vert (full quality gate).
- PHP : Pest (pas PHPUnit), `declare(strict_types=1)`, les tests Unit/Feature du package Laravel tournent **sans** l'extension.
- Un commit logique par tâche verte, message conventionnel (`feat:`/`fix:`/`test:`/`docs:`/`ci:`/`chore:`).

---

## Milestone P0 — Blocants production (correctness et sécurité)

### Task 1: Rendre le canal d'erreurs consumer non bloquant (drop-oldest)

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (11 sites : lignes 461, 471, 492, 502, 513, 544, 653, 666, 744, 774, 998)
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:218` (construction du canal flume)
- Test: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Produces: `ActorState::record_settlement_error(&mut self, error: SettlementError)` — méthode privée interne, aucune API publique ne change.

**Contexte:** `error_tx` est un `flume::bounded(256)` mais l'acteur utilise le send **bloquant** (`state.error_tx.send(...)`). Si PHP n'appelle jamais `drain_errors()`, après 256 erreurs de settlement l'acteur consumer bloque son thread : plus de dispatch, plus de settlement. La doc de `drain_errors` (`set.rs:309-311`) prétend un drop-oldest qui n'existe pas.

- [ ] **Step 1: Write the failing test**

Ajouter à la fin de `crates/rabbit-rs-core/tests/consumer.rs` (réutilise les helpers module-level `subscription`, `connection_key`, `delivery` déjà présents, cf. `settlement_error_surfaces_via_drain_errors` ligne 1473) :

```rust
#[tokio::test(start_paused = true)]
async fn settlement_errors_never_stall_the_actor_when_never_drained() {
    let transport = MockTransport::default();

    // 300 deliveries, chacune acquittée avec un ack en échec. Chaque échec
    // produit un SettlementError ; 300 > 256 (capacité du canal d'erreurs).
    for tag in 1..=300u64 {
        transport.push_delivery(Ok(delivery(tag, b"payload")));
        transport.push_consumer_result(Ok(())); // set_qos
        transport.push_consumer_result(Ok(())); // consume
        transport.push_consumer_result(Err(TransportError::connection("ack-failure")));
    }

    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .unwrap();

    // Consomme et acquitte les 300 messages sans jamais drainer les erreurs.
    for tag in 1..=300u64 {
        let delivery = handle.next().await.expect("delivery must keep flowing");
        assert_eq!(delivery.inner_token().delivery_tag(), tag);
        handle
            .try_settle(delivery.inner_token().clone(), Settlement::Ack)
            .expect("settle enqueued");
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
    }

    // L'acteur n'a pas stallé : le buffer d'erreurs est plein mais borné.
    let errors = handle.drain_errors();
    assert_eq!(errors.len(), 256, "oldest errors dropped, newest kept");
    assert_eq!(errors.last().expect("last error").delivery_tag, 300);

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer settlement_errors_never_stall`
Expected: FAIL (timeout de stall ou longueur d'erreurs ≠ 256) — le send bloquant fige l'acteur.

- [ ] **Step 3: Write minimal implementation**

Dans `crates/rabbit-rs-core/src/consumer/actor.rs`, ajouter au `ActorState` un receiver cloné (flume permet le clonage ; la capacité bornée porte sur les messages en file, pas sur le nombre de receivers) et la méthode helper :

```rust
/// Records a settlement error without ever blocking the actor.
///
/// The error channel is bounded (256). When full, the oldest error is
/// dropped to make room — the actor must never stall waiting for the PHP
/// side to drain, matching the documented contract of
/// `ConsumerHandle::drain_errors`.
fn record_settlement_error(&mut self, error: SettlementError) {
    if self.error_tx.is_full() {
        let _ = self.error_rx.try_recv();
    }
    let _ = self.error_tx.send(error);
}
```

Puis remplacer les 11 occurrences `let _ = state.error_tx.send(SettlementError { ... });` par `state.record_settlement_error(SettlementError { ... });`.

Dans la construction de `ActorState` (même fichier), conserver un clone du `error_rx` utilisé par `ConsumerHandle` :

```rust
let (error_tx, error_rx) = flume::bounded::<SettlementError>(ERROR_CHANNEL_CAPACITY);
// Le actor garde son propre receiver pour le drop-oldest.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS (tous les tests consumer, y compris le nouveau).

- [ ] **Step 5: Run the full quality gate and commit**

Run: `rtk cargo fmt --all && rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates/rabbit-rs-core
git commit -m "fix(core): make consumer settlement error channel non-blocking with drop-oldest"
```

---

### Task 2: Borner le publish buffer de l'extension PHP

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (constantes lignes 30-32, `publish_buffer` ligne 46, `publish()` lignes 103-134, re-buffer ligne 420-425)
- Modify: `crates/rabbit-rs-php/src/classes/exception.rs:35` (helper)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (docblock `publish()`)
- Test: `packages/` — non ; test Pest extension via `crates/rabbit-rs-php/tests/` (Pest, feature `extension-tests`)

**Interfaces:**
- Consomme: `conversion::NativePublish { broker: String, request: PublishRequest }` (`crates/rabbit-rs-php/src/conversion.rs:87-88`), payload accessible via `publish.request.payload.len()`.
- Produces: erreur `Goopil\RabbitRs\BackpressureException` quand le buffer est plein — contrat documenté dans le stub.

**Contexte:** `publish_buffer: std::sync::Mutex<Vec<NativePublish>>` (`pool.rs:46`) croît sans plafond : chaque flush échoué re-buffers ses messages (`pool.rs:420-425`). En outage prolongé avec trafic soutenu, croissance mémoire non bornée côté process PHP (le budget 64 MiB du core ne borne pas ce buffer applicatif).

- [ ] **Step 1: Write the failing test**

Ajouter dans la suite Pest de l'extension (fichier des tests lifecycle, cf. structure existante `crates/rabbit-rs-php/tests/` — le mock `testing_pool()` est injecté via la feature `extension-tests`) :

```php
<?php

use Goopil\RabbitRs\BackpressureException;
use Goopil\RabbitRs\testing_pool;

it('raises backpressure when the publish buffer is full and cannot flush', function () {
    $pool = testing_pool()->with_blocked_transport();

    // PUBLISH_BUFFER_MAX_MESSAGES = 4096 ; au-delà, publish() refuse.
    $message = ['broker' => 'default', 'exchange' => 'jobs', 'routing_key' => 'jobs',
        'payload' => str_repeat('x', 64)];

    $messageIds = [];
    for ($i = 0; $i < 4096; $i++) {
        $messageIds[] = $pool->publish($message);
    }

    expect(fn () => $pool->publish($message))
        ->toThrow(BackpressureException::class);
});
```

Adapter le nom du helper de mock au patron existant des tests Pest de l'extension (le pool de test expose un transport bloqué pour forcer l'échec du flush — cf. `crates/rabbit-rs-php/src/testing.rs` pour l'API réelle du mock).

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk ./scripts/test-extension.sh`
Expected: FAIL — soit le test n'a pas de transport bloqué, soit `publish()` n'atteint jamais `BackpressureException` (buffer non borné).

- [ ] **Step 3: Write minimal implementation**

Dans `crates/rabbit-rs-php/src/classes/exception.rs`, ajouter à côté de `client_exception` (ligne 35) :

```rust
pub(crate) fn backpressure_exception<T>(message: &str) -> PhpResult<T> {
    Err(PhpException::from_class::<BackpressureException>(
        message.to_owned(),
    ))
}
```

Dans `crates/rabbit-rs-php/src/classes/pool.rs`, ajouter les constantes :

```rust
/// Maximum number of buffered publish requests before flushing is forced.
const PUBLISH_BUFFER_MAX_MESSAGES: usize = 4096;
/// Maximum cumulative buffered payload bytes before flushing is forced.
const PUBLISH_BUFFER_MAX_BYTES: usize = 64 * 1024 * 1024;
```

Ajouter au struct `Pool` un compteur d'octets borné `publish_buffer_bytes: std::sync::Mutex<usize>` (initialisé à 0 dans `__construct`), maintenu à chaque push/re-buffer/drain du buffer.

Dans `publish()` (après la conversion, avant le push), vérifier la capacité :

```rust
let payload_bytes = publish.request.payload.len();
let mut buffer = self.publish_buffer.lock().expect("publish buffer mutex poisoned");

let at_capacity = buffer.len() >= PUBLISH_BUFFER_MAX_MESSAGES
    || *self.publish_buffer_bytes.lock().expect("publish buffer bytes mutex poisoned")
        + payload_bytes
        > PUBLISH_BUFFER_MAX_BYTES;

if at_capacity {
    drop(buffer);
    self.flush()?; // tente de faire de la place
    let mut buffer = self.publish_buffer.lock().expect("publish buffer mutex poisoned");
    let bytes = *self.publish_buffer_bytes.lock().expect("publish buffer bytes mutex poisoned");
    if buffer.len() >= PUBLISH_BUFFER_MAX_MESSAGES
        || bytes + payload_bytes > PUBLISH_BUFFER_MAX_BYTES
    {
        return backpressure_exception(&format!(
            "publish buffer is full ({} messages, {} buffered bytes); retry after flush",
            buffer.len(),
            bytes,
        ));
    }
}
buffer.push(publish);
```

Maintenir le compteur d'octets aux deux points où le buffer change : `publish()` (push) et le re-buffer de flush échoué (`pool.rs:420-425`). Le re-buffer des messages **déjà acceptés** est autorisé à dépasser la capacité (ils ont déjà reçu un `message_id` — les dropper serait une perte silencieuse) ; dans ce cas les nouveaux `publish()` reçoivent `BackpressureException` jusqu'à ce que le buffer repasse sous le plafond.

- [ ] **Step 4: Update the stub docblock**

Dans `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, section `publish()` :

```php
/**
 * Publishes one message, returning its stable message identifier.
 * ...
 * @throws \Goopil\RabbitRs\BackpressureException when the bounded publish
 *   buffer is full (outage with sustained traffic); retry with the same
 *   message later. Already-buffered messages are never dropped.
 */
```

- [ ] **Step 5: Run tests and check benchmark non-regression**

Run: `rtk ./scripts/test-extension.sh && rtk ./scripts/check.sh`
Expected: PASS.

Le plafond du buffer touche le hot path de publication : lancer le scénario publish
du driver-level bench (Phase E, mode blind + safe) et comparer au budget figé
(`benchmarks/results/benchmark-results.json`, cf. plan Task 40 initial) :

Run: `cd benchmarks/driver-bench && (voir README § run) ./run.sh --smoke rabbit-rs`
Expected: throughput dans la variance des archives (`runs/phase-e/`) — aucune
régression > 5 %.

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-php
git commit -m "fix(php-ext): bound the publish buffer with explicit backpressure"
```

---

### Task 3: Deadline et timeout sur l'attente de consumer

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs` (nouvelle section `ConsumerConfigSection`)
- Modify: `crates/rabbit-rs-core/src/client.rs:330-410` (boucle d'attente dans `consumer()`, désormais composée par broker)
- Modify: `crates/rabbit-rs-core/src/pool/key.rs` (fingerprint)
- Modify: `crates/rabbit-rs-php/src/conversion.rs` (mapping config) et `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
- Modify: `packages/laravel-queue/config/rabbit-rs.php` + `packages/laravel-queue/src/Config/ConfigNormalizer.php`
- Test: `crates/rabbit-rs-core/tests/consumer.rs` + `packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php`

**Interfaces:**
- Produces: `Config` gagne `consumer: ConsumerConfigSection { wait_timeout: Duration }` (serde `consumer.wait_timeout`, défaut 30 s, validé borné 1 s..=24 h) ; échéance → `ClientError` de kind `ClientErrorKind::Transport` — mappé en `ConnectionException` côté PHP (cf. `client_exception` dans `crates/rabbit-rs-php/src/classes/exception.rs:35`).

**Contexte:** `ClientPool::consumer()` (`client.rs:330+`) boucle indéfiniment quand le coordinator ne quitte jamais `Connecting`↔`Recovering` (broker black-holé, pas de connect timeout) : les workers FPM peuvent se figer sans échappatoire. Depuis la composition multi-broker (PR #35), la boucle d'attente `wait_for_state` se trouve dans la boucle de composition par broker (≈ lignes 371-410) — la deadline doit envelopper l'acquisition complète, toutes sources confondues.

- [ ] **Step 1: Write the failing test (core)**

Dans `crates/rabbit-rs-core/tests/consumer.rs` (ou un nouveau fichier `consumer_wait_deadline.rs`) :

```rust
#[tokio::test(start_paused = true)]
async fn consumer_wait_deadline_expires_when_the_broker_never_becomes_ready() {
    let transport = MockTransport::default();
    // Aucun connect result poussé : le connect gate reste fermé pour toujours.
    let _gate = transport.push_connect_gate();

    // Construire une config de base valide puis y injecter le timeout court,
    // comme les tests existants de client.rs (cf. la construction du profil
    // worker dans les tests unitaires de crates/rabbit-rs-core/src/client.rs).
    let base = Config {
        brokers: vec![helper::broker("b", "/")],
        workers: vec![worker_profile_with_subscription("main", "b", "main.jobs")],
        topology_mode: TopologyMode::Declare,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
        queue_type: QueueKind::Quorum,
        queue_durable: true,
        consumer: rabbit_rs_core::config::ConsumerConfigSection {
            wait_timeout: Duration::from_millis(500),
        },
    };

    let pool = rabbit_rs_core::pool::ClientPool::new(
        Arc::new(base.validate().expect("valid config")),
        Arc::new(MockTransport::default()),
    );

    let started = tokio::time::Instant::now();
    let result = pool.consumer("main").await;

    let error = result.expect_err("must not wait forever");
    assert!(
        matches!(error.kind(), rabbit_rs_core::pool::ClientErrorKind::Transport),
        "deadline expiry must surface as a typed transport error: {error:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_millis(500));
}
```

Adapter la construction du profil worker au helper existant (les tests de `client_pool` dans `crates/rabbit-rs-core/src/client.rs` montrent la construction complète).

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core consumer_wait_deadline`
Expected: FAIL — le champ `consumer` n'existe pas (compilation) puis la boucle ne termine jamais.

- [ ] **Step 3: Implement the config section**

Dans `crates/rabbit-rs-core/src/config.rs` :

```rust
/// Consumer acquisition settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct ConsumerConfigSection {
    /// Maximum wall-clock time PHP waits for a consumer handle to become
    /// ready (connection + topology + basic_consume) before a typed error
    /// is returned. Prevents unbounded blocking on black-holed brokers.
    #[serde(with = "humantime_serde")]
    pub wait_timeout: std::time::Duration,
}

impl Default for ConsumerConfigSection {
    fn default() -> Self {
        Self { wait_timeout: std::time::Duration::from_secs(30) }
    }
}
```

Ajouter `pub consumer: ConsumerConfigSection` au struct `Config` (avec `#[serde(default)]`) et le champ correspondant dans `ValidatedConfig`. Validation : `wait_timeout` borné `1 s..=24 h` avec `ConfigError` à chemin `consumer.wait_timeout`. Mettre à jour `ConnectionKey::from_config` / `ConfigFingerprint` pour inclure la valeur.

Mettre à jour les littéraux `Config { ... }` des helpers de tests (`tests/consumer.rs::helper::connection_key` et autres sites) en ajoutant `consumer: ConsumerConfigSection::default(),`.

- [ ] **Step 4: Bound the acquisition**

Dans `crates/rabbit-rs-core/src/client.rs::consumer()`, envelopper l'acquisition complète (la composition par broker et ses boucles `wait_for_state`, ≈ lignes 355-410) :

```rust
let wait_timeout = self.config.consumer.wait_timeout;
let consumer = tokio::time::timeout(wait_timeout, async {
    // ... boucle existante inchangée (coordinator.consumer / wait_for_state /
    // is_closed / FailedPermanent / Closed)
})
.await
.map_err(|_elapsed| {
    ClientError::transport(&TransportError::connection(format!(
        "consumer profile '{profile}' did not become ready within {wait_timeout:?}"
    )))
})??;
```

Avec le temps Tokio suspendu, `timeout` respecte `advance()` — le test reste déterministe.

- [ ] **Step 5: Wire through PHP and Laravel**

- `crates/rabbit-rs-php/src/conversion.rs` : mapper la clé `consumer.wait_timeout` (entier ms, optionnel) vers la config native.
- `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` : documenter la clé de config.
- `packages/laravel-queue/config/rabbit-rs.php` : ajouter `'wait_timeout' => 30_000` sous une section `consumers` (ms).
- `packages/laravel-queue/src/Config/ConfigNormalizer.php` : valider (int > 0, ≤ 86 400 000) et mapper vers `consumer.wait_timeout`.

- [ ] **Step 6: Write the failing Laravel test, then pass it**

Dans `packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php` :

```php
it('maps consumer wait_timeout to the native config', function () {
    $config = validConfig(['consumers' => ['wait_timeout' => 5_000]]);
    $native = (new Goopil\RabbitRs\Laravel\Config\ConfigNormalizer)->normalize($config);

    expect($native['consumer']['wait_timeout'])->toBe(5_000);
});

it('rejects a consumer wait_timeout outside the 1s..24h bound', function () {
    validConfig(['consumers' => ['wait_timeout' => 0]]);
})->throws(Goopil\RabbitRs\Laravel\Exceptions\ConfigurationException::class);
```

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/ConfigNormalizerTest.php`
Expected: FAIL puis PASS après implémentation Step 5.

- [ ] **Step 7: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates packages
git commit -m "feat(core): bound consumer acquisition wait with a configurable deadline"
```

---

### Task 4: Échecs bruyants sur les fichiers TLS illisibles

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:351-372` (`build_tls_config`, `build_tls_identity`)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (appelant de `build_tls_config` dans `connect()`)
- Test: `crates/rabbit-rs-core/tests/transport_tuning.rs` (ou nouveau `tls_errors.rs`)

**Contexte:** `fs::read_to_string(path).ok()` et `fs::read(path).ok()?` retombent silencieusement quand un CA cert ou un couple client cert/key est illisible : la connexion part sans la CA prévue (sécurité dégradée en silence). `TlsVerify::None` et `server_name` (SNI) restent des champs validés mais non câblés — reportés à la Task 12 avec l'intégration TLS réelle ; cette tâche garantit qu'aucun fichier TLS configuré ne peut être silencieusement ignoré.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn unreadable_tls_files_fail_loudly_instead_of_connecting_unprotected() {
    let mut broker = helper::broker("tls-b", "/");
    broker.tls = rabbit_rs_core::config::TlsConfig {
        enabled: true,
        server_name: None,
        ca_cert: Some(std::path::PathBuf::from("/nonexistent/ca.pem")),
        client_cert: None,
        client_key: None,
        verify: rabbit_rs_core::config::TlsVerify::Peer,
    };

    let transport = rabbit_rs_core::transport::lapin::LapinTransport::default();
    let error = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(transport.connect(&broker))
        .expect_err("unreadable CA cert must fail loudly");

    assert!(
        error.to_string().contains("/nonexistent/ca.pem"),
        "error must identify the exact file path: {error}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core unreadable_tls_files_fail_loudly`
Expected: FAIL — l'erreur actuelle est une erreur de connexion réseau (CA ignorée), pas une erreur identifiant le fichier.

- [ ] **Step 3: Write minimal implementation**

Dans `crates/rabbit-rs-core/src/transport/lapin.rs` :

```rust
fn build_tls_config(config: &BrokerConfig) -> TransportResult<lapin::tcp::OwnedTLSConfig> {
    let tls = &config.tls;
    let identity = build_tls_identity(tls)?;
    let cert_chain = match tls.ca_cert() {
        Some(path) => Some(
            std::fs::read_to_string(path).map_err(|error| {
                TransportError::config(format!(
                    "tls.ca_cert: cannot read '{}': {error}",
                    path.display()
                ))
            })?,
        ),
        None => None,
    };

    Ok(lapin::tcp::OwnedTLSConfig { identity, cert_chain })
}

fn build_tls_identity(
    tls: &crate::config::TlsConfig,
) -> TransportResult<Option<lapin::tcp::OwnedIdentity>> {
    let (Some(cert_path), Some(key_path)) = (tls.client_cert(), tls.client_key()) else {
        return Ok(None);
    };

    let pem = std::fs::read(cert_path).map_err(|error| {
        TransportError::config(format!(
            "tls.client_cert: cannot read '{}': {error}",
            cert_path.display()
        ))
    })?;
    let key = std::fs::read(key_path).map_err(|error| {
        TransportError::config(format!(
            "tls.client_key: cannot read '{}': {error}",
            key_path.display()
        ))
    })?;

    Ok(Some(lapin::tcp::OwnedIdentity::PKCS8 { pem, key }))
}
```

Adapter l'appelant dans `connect()` pour propager `TransportResult` (`?`). Vérifier que `TransportError` expose une variante de config (`config(...)`) — sinon ajouter une variante `Configuration { message }` dans `transport.rs` en suivant le style existant des erreurs typées.

- [ ] **Step 4: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core unreadable_tls_files_fail_loudly && rtk cargo test -p rabbit-rs-core`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates/rabbit-rs-core
git commit -m "fix(core): fail loudly on unreadable TLS certificate files"
```

---

### Task 5: Horizon — respecter after-commit et câbler bulk()

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:261,289` (`prepareBatch` et `publishBatch` : `private` → `protected`)
- Modify: `packages/laravel-queue/src/Horizon/RabbitMqQueue.php` (push/later via `enqueueUsing`, override `prepareBatch`)
- Test: `packages/laravel-queue/tests/Feature/HorizonAfterCommitTest.php` (nouveau)

**Contexte:** En mode Horizon, `push()`/`later()` contournent `enqueueUsing` (appellent `createPayload` + `pushRaw`/`laterRawFromPayload` directement) : `after_commit` est ignoré — des jobs sont publiés alors que la transaction SQL n'est pas commitée (perte de jobs transactionnels). `bulk()` n'est pas surchargé : jobs bulk sans `JobPayload::prepare()` ni events Horizon, invisibles au dashboard.

- [ ] **Step 1: Write the failing test**

`packages/laravel-queue/tests/Feature/HorizonAfterCommitTest.php` (avec les fakes Horizon du bootstrap existant) :

```php
<?php

use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue;
use Illuminate\Support\Facades\DB;
use Laravel\Horizon\Events\JobPushed;

it('defers Horizon job publication until the transaction commits', function () {
    $queue = $this->app->make('queue')->connection('rabbit-rs-horizon');
    expect($queue)->toBeInstanceOf(RabbitMqQueue::class);

    $published = [];
    $queue->swapNativePool(function (array $message) use (&$published) {
        $published[] = $message['payload'];
        return $message;
    });

    DB::transaction(function () use ($queue) {
        dispatch(new Fixtures\CommitJob)->onConnection('rabbit-rs-horizon');
        expect($published)->toBeEmpty('job must not be published inside the transaction');
    });

    expect($published)->toHaveCount(1);
});

it('pushes Horizon bulk jobs with prepared payloads and events', function () {
    $queue = $this->app->make('queue')->connection('rabbit-rs-horizon');
    Event::fake([JobPushed::class]);

    $queue->bulk([new Fixtures\BulkJob, new Fixtures\BulkJob], '', 'bulk');

    Event::assertDispatchedTimes(JobPushed::class, 2);
});
```

(Le mécanisme `swapNativePool` est un exemple : utiliser le patron de mock natif existant de `tests/bootstrap.php` — les fakes `Pool`/`Consumer` fidèles au contrat.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/HorizonAfterCommitTest.php`
Expected: FAIL — le job est publié dans la transaction (after_commit ignoré) et bulk ne déclenche pas les events.

- [ ] **Step 3: Make the base batch helpers overridable**

Dans `packages/laravel-queue/src/RabbitMqQueue.php`, remplacer `private function prepareBatch(...)` (ligne 261) et `private function publishBatch(...)` (ligne 289) par `protected function`. Aucun changement de signature ni de logique.

- [ ] **Step 4: Rewrite the Horizon push/later/bulk path**

Dans `packages/laravel-queue/src/Horizon/RabbitMqQueue.php` :

```php
public function push($job, $data = '', $queue = null)
{
    $queueName = $this->queueName($queue);

    return $this->enqueueUsing(
        $job,
        (new JobPayload($this->createPayload($job, $queueName, $data)))->prepare($job)->value,
        $queue,
        null,
        fn (string $payload, ?string $queue): string => $this->publishHorizonPayload($payload, $queue),
    );
}

public function later($delay, $job, $data = '', $queue = null)
{
    $queueName = $this->queueName($queue);

    return $this->enqueueUsing(
        $job,
        (new JobPayload($this->createPayload($job, $queueName, $data, $delay)))->prepare($job)->value,
        $queue,
        $delay,
        fn (string $payload, ?string $queue, mixed $delay): string => $this->publishHorizonPayload(
            $payload, $queue, $this->delayMilliseconds($delay),
        ),
    );
}

protected function prepareBatch(array $jobs, mixed $data, mixed $queue): array
{
    return array_map(function (array $prepared) use ($queue) {
        $payload = (new JobPayload($prepared['payload']))->prepare($prepared['job'])->value;
        $this->event($this->queueName($queue), new JobPending($payload));

        return [...$prepared, 'payload' => $payload];
    }, parent::prepareBatch($jobs, $data, $queue));
}

private function publishHorizonPayload(string $payload, ?string $queue, ?int $delayMs = null): string
{
    $queueName = $this->queueName($queue);

    $result = $delayMs === null
        ? $this->publish($payload, $queue, ['content_type' => self::CONTENT_TYPE_JSON])
        : $this->publish($payload, $queue, ['content_type' => self::CONTENT_TYPE_JSON], $delayMs);

    $this->event($queueName, new JobPushed($payload));

    return $result;
}
```

Supprimer la propriété `$lastPushed` et l'ancien `pushRaw` surchargé (le payload Horizon est maintenant préparé au niveau `push`/`later`/`prepareBatch`, avant `enqueueUsing`, de sorte que le callback publié au commit transporte déjà le payload préparé).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/HorizonAfterCommitTest.php && php vendor/bin/pest`
Expected: PASS (nouveau test + aucune régression sur la suite existante y compris les tests Horizon H1-H6).

- [ ] **Step 6: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add packages/laravel-queue
git commit -m "fix(laravel): honor after-commit and Horizon events for push, later and bulk"
```

---

### Task 6: Alerte poison-message sur les défauts permissifs

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php` (boot)
- Test: `packages/laravel-queue/tests/Feature/PoisonMessageWarningTest.php` (nouveau)

**Contexte:** Par défaut `topology.queue.delivery_limit => null` et `topology.dead_letter => null` (`config/rabbit-rs.php:329-331`) : un message qui crashe le worker avant settlement est redelivré à l'infini. La protection est opt-in sans aucun signal. On n'impose pas de nouveau défaut (breaking change) mais on alerte en production.

- [ ] **Step 1: Write the failing test**

```php
<?php

use Illuminate\Support\Facades\Log;

it('warns when delivery_limit and dead_letter are both unset in production', function () {
    config(['queue.connections.rabbit-rs.production_warning' => true]);
    Log::shouldReceive('warning')->once()->withArgs(
        fn (string $message) => str_contains($message, 'delivery_limit') && str_contains($message, 'dead_letter'),
    );

    $this->app->make('queue')->connection('rabbit-rs');
});

it('does not warn when delivery_limit is configured', function () {
    config([
        'queue.connections.rabbit-rs.production_warning' => true,
        'queue.connections.rabbit-rs.topology.queue.delivery_limit' => 20,
    ]);
    Log::shouldReceive('warning')->never();

    $this->app->make('queue')->connection('rabbit-rs');
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/PoisonMessageWarningTest.php`
Expected: FAIL — aucun warning émis.

- [ ] **Step 3: Write minimal implementation**

Dans `packages/laravel-queue/src/RabbitMqServiceProvider.php::boot()`, à la première résolution d'une connexion `rabbit-rs` (via le connector, une fois par empreinte de config) :

```php
if (
    ($config['topology']['queue']['delivery_limit'] ?? null) === null
    && ($config['topology']['dead_letter'] ?? null) === null
    && (bool) ($config['production_warning'] ?? true)
    && $this->app->environment('production')
) {
    Log::warning(
        'rabbit-rs: delivery_limit and dead_letter are both unset for this connection. '
        .'A poison message (worker crash before settlement) will be redelivered forever. '
        .'Set topology.queue.delivery_limit with topology.dead_letter, or silence this '
        .'with production_warning => false.'
    );
}
```

Déclencher le warning une seule fois par process (flag statique ou propriété du connector partagé). La clé `production_warning` (défaut `true`) est ajoutée à `config/rabbit-rs.php` avec un commentaire.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/PoisonMessageWarningTest.php && php vendor/bin/pest`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue
git commit -m "feat(laravel): warn on unbounded redelivery defaults in production"
```

---

## Milestone P1 — Durcissement

### Task 7: Établissement paresseux des consumers par profile demandé

**Files:**
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:406` (`recover_generation`)
- Modify: `crates/rabbit-rs-core/src/client.rs` (registre des profiles demandés)
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (nouveau test)

**Contexte:** `recover_generation` boucle sur **tous** les `worker_profiles()` de la config (`recovery_coordinator.rs:406`) : un process purement publisher déclare des worker profiles ouvre des channels + `basic_consume` sur toutes les queues à chaque reconnexion et retient des messages non-ackés (jusqu'à prefetch par queue) — blocage invisible de queues et redeliveries inutiles.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn only_requested_worker_profiles_are_consumed() {
    // Config avec deux worker profiles : "main" (queue main.jobs) et
    // "side" (queue side.jobs), sur le même broker mock.
    let transport = MockTransport::default();
    // ... construction du pool via les helpers existants de client.rs ...

    // Le process demande uniquement le profil "main".
    let _handle = pool.consumer("main").await.expect("main consumer");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let operations = transport.operations();
    let consumed_queues: Vec<&str> = operations.iter().filter_map(|operation| match operation {
        TransportOperation::Consume { queue, .. } => Some(queue.as_str()),
        _ => None,
    }).collect();

    assert!(consumed_queues.contains(&"main.jobs"), "requested profile consumed: {consumed_queues:?}");
    assert!(
        !consumed_queues.contains(&"side.jobs"),
        "unrequested profile must not be consumed: {consumed_queues:?}"
    );
}
```

(Vérifier le variant exact de `TransportOperation` pour `basic_consume` dans `crates/rabbit-rs-core/src/transport/mock.rs` et adapter le pattern matching.)

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core only_requested_worker_profiles`
Expected: FAIL — `side.jobs` est consommé malgré l'absence de demande.

- [ ] **Step 3: Write minimal implementation**

1. Dans `crates/rabbit-rs-core/src/client.rs`, ajouter `requested_profiles: std::sync::Mutex<std::collections::HashSet<String>>` au `ClientPool`. `consumer(profile)` insère le profile dans le set **avant** de déclencher les coordinators.
2. Partager le set avec chaque coordinator (passé à la construction, `Arc<Mutex<HashSet<String>>>`).
3. Dans `recover_generation` (`recovery_coordinator.rs:406`), filtrer `worker_profiles()` : ne traiter que les profiles présents dans le set demandé. Un profile ajouté après une reconnexion est établi au prochain appel `coordinator.consumer(profile)` (la boucle d'attente de `client.consumer()` retente déjà).

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core && rtk ./scripts/check.sh`
Expected: PASS (attention aux tests existants qui attendaient un établissement eager — les adapter si leur intention est préservée).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core
git commit -m "feat(core): lazily establish consumers only for requested worker profiles"
```

---

### Task 8: Composition multi-broker du consumer — LIVRÉE SUR MAIN

**Statut : terminée en amont.** Livrée par la Phase D post-pump (PR #35, commit
`585c534` « compose multi-broker consumers from all coordinators ») :

- `crates/rabbit-rs-core/src/consumer/composite.rs` — `pub struct ConsumerHandle` :
  le handle composé qui merge les livraisons des sources multi-brokers avec une
  sélection équitable et route chaque settlement vers le broker source.
- `crates/rabbit-rs-core/src/consumer/set.rs:284` — `pub struct ConsumerSetHandle` :
  le handle par broker (renommé depuis l'ancien `ConsumerHandle`).
- `ClientPool::consumer()` (`client.rs:330`) retourne désormais le handle composé.
- Semantics documentées : `docs/` — commit `39ced65` « document multi-broker
  consumer semantics ».

**Vérification d'adéquation avec l'audit :** l'écart identifié (« seul le 1er
broker est consommé, `client.rs:414-417` ») n'existe plus. Le test multi-brokers
prévu par cette task reste pertinent comme non-régression : si un scenario similaire
est souhaité, l'écrire en s'appuyant sur `composite.rs` et les tests existants
(`tests/consumer.rs`, section composite). Aucun code à écrire pour cette task.

---

### Task 9: Mesurer les doublons de livraison

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (chemin de dispatch, où `attempts` est résolu)
- Modify: `crates/rabbit-rs-core/src/metrics.rs:145-147` (aucun changement de signature — wiring uniquement)
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (nouveau test)

**Contexte:** `record_duplicate()` (`metrics.rs:145`) n'est jamais appelé : `duplicate_count` est toujours 0 alors que le contrat projet exige des doublons « identifiables and measurable ». Le snapshot expose un compteur mort, trompeur pour l'exploitation.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn redelivered_messages_are_counted_as_duplicates() {
    let transport = MockTransport::default();
    let mut redelivered = helper::delivery(1, b"payload");
    redelivered.redelivered = true;
    transport.push_delivery(Ok(redelivered));
    transport.push_delivery(Ok(delivery(2, b"fresh")));

    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .unwrap();

    let _ = handle.next().await.unwrap();
    let _ = handle.next().await.unwrap();

    let snapshot = handle.metrics_snapshot();
    assert_eq!(snapshot.duplicate_count, 1, "one redelivery counted");
    assert_eq!(snapshot.deliveries_total, 2, "both deliveries counted");

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core redelivered_messages_are_counted`
Expected: FAIL — `duplicate_count == 0`.

- [ ] **Step 3: Write minimal implementation**

Dans le chemin de dispatch de `consumer/actor.rs` (là où `attempts` est résolu via `AttemptsResolver`), après résolution : si `attempts > 1` (redelivered flag, `x-acquired-count`, ou `x-delivery-count` > 1 — la source exacte est déjà centralisée dans `consumer/attempts.rs`), appeler `self.metrics.record_duplicate()` (le `Metrics` partagé de l'acteur). Un seul appel par livraison redelivrée.

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core && rtk ./scripts/check.sh`
Expected: PASS. Compléter l'assertion du test (`duplicate_count == 1`, `deliveries_total == 2`).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core
git commit -m "feat(core): count redelivered messages as duplicates in metrics"
```

---

### Task 10: Drainer les events natifs depuis publish() et next()

**Files:**
- Create: `crates/rabbit-rs-php/src/classes/bridge.rs`
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (déplace les callbacks/états vers le bridge)
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` (déclenche le bridge dans `next()`/`tryNext()`/`nextBatch()`)
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php` + README (drain au pop)
- Test: `crates/rabbit-rs-php/tests/` (Pest) + `packages/laravel-queue/tests/Feature/NativeEventDispatchTest.php`

**Contexte:** Les callbacks `onConnectionState`/`onBackpressure` ne sont invoqués que pendant `stats()` (`pool.rs:263-264`) — or le driver n'appelle jamais `stats()` en régime normal. `ConnectionStateChanged`/`BackpressureDetected` sont inopérants en production alors que le README (`packages/laravel-queue/README.md:17`) et `docs/operations.md:231` promettent le contraire.

- [ ] **Step 1: Write the failing test (extension)**

```php
it('invokes connection state callbacks during publish and consume without stats()', function () {
    $pool = testing_pool()->with_failing_transport(); // transport qui tombe

    $states = [];
    $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$states) {
        $states[] = [$broker, $state, $generation];
    });

    $pool->publish([...]);
    expect($states)->not->toBeEmpty('callback must fire on publish, not only on stats()');
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk ./scripts/test-extension.sh`
Expected: FAIL — `$states` vide sans `stats()`.

- [ ] **Step 3: Extract an EventBridge**

`crates/rabbit-rs-php/src/classes/bridge.rs` :

```rust
/// Shared event bridge: owns the PHP callbacks and last-seen state so both
/// `Pool` (publish path) and `Consumer` (pop path) can drain native events
/// on the PHP thread. Callbacks are invoked only on the PHP thread, never
/// from a Rust thread; mutexes are released before invocation.
pub(crate) struct EventBridge {
    connection_state_callback: CallbackSlot,
    backpressure_callback: CallbackSlot,
    last_connection_states: std::sync::Mutex<HashMap<String, (String, i64)>>,
    last_backpressure_total: std::sync::Mutex<u64>,
    client: std::sync::Weak<ClientPool>,
}
```

Déplacer `invoke_connection_state_callbacks` et `invoke_backpressure_callbacks` (actuellement méthodes de `Pool`, `pool.rs:443-505`) vers `EventBridge` en implémentations `Arc<EventBridge>`. `Pool` et `Consumer` détiennent `Arc<EventBridge>` (constructeur `Consumer::new` gagne un paramètre `bridge: Arc<EventBridge>` — mise à jour de tous les appels). Invariant préservé : mutex relâchés avant invocation des callbacks (anti-deadlock, cf. `callbacks.rs:1-24` et `CallbackDeadlockTest.php`).

Déclencher `bridge.drain(...)` :
- dans `Pool::publish()` et `Pool::publishBatch()` (après flush),
- dans `Consumer::next()`/`tryNext()`/`nextBatch()` (avant de bloquer sur l'attente),
- toujours dans `stats()` (comportement existant).

- [ ] **Step 4: Wire the Laravel driver**

Dans `packages/laravel-queue/src/RabbitMqQueue.php::pop()`, avant le `next()` : aucun changement nécessaire côté PHP (l'extension draine nativement) ; en revanche **corriger les docs** : `README.md:17` et `docs/operations.md:231` deviennent exacts avec ce comportement — vérifier et ajuster la formulation (« events fire during publish and consume operations »).

- [ ] **Step 5: Run tests to verify they pass**

Run: `rtk ./scripts/test-extension.sh && cd packages/laravel-queue && php vendor/bin/pest`
Expected: PASS (y compris `CallbackDeadlockTest` et `NativeEventDispatchTest`).

- [ ] **Step 6: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates/rabbit-rs-php packages/laravel-queue
git commit -m "feat(php-ext): drain native events on publish and consume paths"
```

---

### Task 11: Borner le mode Blind en octets

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:229` (`publish_blind`, post-pump v2)
- Modify: `crates/rabbit-rs-core/src/publisher/pump.rs` (intake de la pump blind)
- Test: `crates/rabbit-rs-core/tests/blind_pump.rs`

**Contexte:** `publish_blind` n'acquiert ni sémaphore ni byte budget : la borne mémoire est un nombre de messages (1024 intake + 2048 in-flight). 1024 payloads de 50 MB passent — incohérent avec Safe/Unsafe qui bornent nombre et octets. Le builder `with_byte_budget` existe déjà (`publisher/mod.rs:245`) pour les modes confirmés — le réutiliser.

- [ ] **Step 1: Write the failing test**

Dans `crates/rabbit-rs-core/tests/blind_pump.rs` :

```rust
#[tokio::test(start_paused = true)]
async fn blind_publish_respects_the_byte_budget() {
    // Capacity de la pump réduite pour le test ; payload de taille
    // volontairement > budget_bytes / capacité.
    let pump = BlindPump::spawn_with_budget(/* budget_bytes: */ 1024 * 1024, /* capacity: */ 4);

    let oversized = vec![0u8; 512 * 1024];
    // 3 x 512 KiB = 1.5 MiB > 1 MiB : la 4e publication doit être rejetée.
    for _ in 0..3 {
        pump.try_publish_blind(/* request avec payload oversized */).expect("within budget");
    }
    let error = pump.try_publish_blind(/* request */).expect_err("over byte budget");
    assert!(matches!(error, PublishError::Backpressure { .. }));
}
```

(Adapter aux constructeurs réels de `publisher/pump.rs` et aux types `PublishRequest`/`PublishError`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core blind_publish_respects_the_byte_budget`
Expected: FAIL — la 4e publication est acceptée.

- [ ] **Step 3: Write minimal implementation**

Ajouter un budget bytes atomique à la pump blind (même sémantique que le budget
des modes confirmés — réutiliser le builder `with_byte_budget` de
`publisher/mod.rs:245`) : incrémenté à l'intake (`checked_add`, overflow →
`Backpressure`), décrémenté à la sortie du transport. Appliquer dans
`publish_blind` (`publisher/actor.rs:229`) AVANT l'insertion dans l'intake flume.

- [ ] **Step 4: Run tests and check benchmark non-regression**

Run: `rtk cargo test -p rabbit-rs-core && rtk ./scripts/check.sh`
Expected: PASS.

Le mode blind est le chemin le plus rapide du publish — vérifier la non-régression
sur le scénario fire-and-forget du driver bench (comparer aux archives
`runs/phase-e/`, tolérance 5 %).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core
git commit -m "fix(core): enforce byte budget on blind publish pump"
```

---

### Task 12: TLS d'intégration — SNI et verify, lab TLS

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (`server_name` → SNI, `verify` → mode de vérification)
- Modify: `lab/rabbitmq/` (profil TLS : certificates auto-signés, ports amqps 5675+)
- Modify: `crates/rabbit-rs-core/tests/` (nouveau `tls_integration.rs`, feature `integration`)
- Modify: `crates/rabbit-rs-core/src/config.rs` (docblock `TlsVerify`/`server_name` : contract documenté)

**Contexte:** `TlsVerify::None` et `effective_server_name()` (`config.rs:194`) sont validés et hachés dans le fingerprint mais jamais lus par le transport (`lapin.rs:351-372`). Changer `verify`/`server_name` change le fingerprint → nouveau pool → comportement inchangé. Aucun test TLS réel n'existe.

**Décision d'API:** Lapin 4.10 avec rustls ne consomme qu'`OwnedTLSConfig { identity, cert_chain }` ; le SNI et la désactivation de vérification nécessitent un connecteur TLS custom (`lapin::tcp::TLSBackend` / `ConnectionProperties::with_ssl` ou connecteur rustls injecté). Vérifier l'API exacte de lapin 4.10 dans `~/.cargo/registry/src/*/lapin-4.10*/src/tcp/` avant d'implémenter ; si lapin ne permet pas d'injecter un `rustls::ClientConfig` custom, implémenter :
1. `verify = Peer` (défaut) : comportement rustls par défaut (vérification hostname = `effective_server_name()`). **Documenter explicitement dans le stub et `config.rs` que la vérification utilise le nom du premier host à défaut de `server_name`.**
2. `verify = None` : non supporté en V1 → `ConfigError` explicite « tls.verify: 'none' requires a custom TLS connector, not yet supported » (au lieu d'être silencieusement ignoré). Retirer de la surface exposée côté Laravel si non câblé.

- [ ] **Step 1: Write the failing integration tests**

`crates/rabbit-rs-core/tests/tls_integration.rs` (marqué `#[cfg(feature = "integration")]`, lab requis) :

```rust
#[tokio::test]
async fn tls_handshake_succeeds_against_the_lab_certificate() {
    // Broker configuré en amqps avec le CA auto-signé du lab TLS profile.
    // Connecte, ouvre un publisher channel, publie avec confirms activés.
    // Assert: Confirmed.
}

#[tokio::test]
async fn tls_handshake_fails_against_an_untrusted_ca() {
    // Même broker mais CA différent (fichier CA de mauvaise confiance).
    // Assert: erreur de connexion typée, pas de message en clair.
}

#[tokio::test]
async fn server_name_overrides_sni() {
    // Certificat délivré pour 'rabbit.internal' ; hosts = ['127.0.0.1'];
    // tls.server_name = 'rabbit.internal'. Assert: handshake réussit.
    // Variante inverse: server_name = 'wrong.host' → handshake échoue.
}
```

- [ ] **Step 2: Add the TLS profile to the lab**

- `lab/rabbitmq/compose.yaml` : profil `with-tls` — ports `5675-5677` en amqps, volumes des certificats.
- Générer les certificats (CA auto-signé + cert serveur SAN `rabbit.internal`, `127.0.0.1`) via un script `lab/rabbitmq/tls/generate.sh` (openssl, épinglé par digest d'image ou openssl local), commits des `.gitignore`d PEMs hors du repo (générés au `lab-up`).
- `scripts/lab-ready.sh` : vérifier l'écoute amqps.
- `scripts/test-integration.sh` : inclure le profil TLS quand `--with-tls`.

- [ ] **Step 3: Implement and verify**

Implémenter le SNI/verify selon la décision d'API ci-dessus (et `ConfigError` explicite pour `verify: none` non supporté). Run: `rtk cargo test -p rabbit-rs-core --features integration --test tls_integration && ./scripts/test-integration.sh --with-tls`
Expected: PASS.

- [ ] **Step 4: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates lab scripts
git commit -m "feat(core): wire TLS SNI and verify with lab integration tests"
```

---

### Task 13: Validation PIE de bout en bout

**Files:**
- Modify: `scripts/package-pie-binary.sh:273` (naming)
- Modify: `.github/workflows/release.yml:161` (naming — un seul source de vérité)
- Create: `.github/workflows/verify-pie.yml` ou job dans `release.yml`
- Test: run CI sur une release draft

**Contexte:** Incohérence de naming : `package-pie-binary.sh` produit `...-linux-glibc-nts.zip` (suffixe `-nts`) tandis que `release.yml` produit `...-linux-glibc.zip` (sans suffixe — c'est ce qui est publié dans v0.0.7). La résolution d'asset par PIE dépend du pattern de nommage ; la chaîne n'a jamais été validée par un `pie install` réel.

- [ ] **Step 1: Determine the PIE-expected naming empirically**

Sur une machine locale avec PIE 1.5+ et PHP 8.4 :

```bash
pie download goopil/rabbit-rs-native@0.0.7 --dry-run 2>&1 || true
# puis télécharger manuellement l'asset v0.0.7 et installer localement :
gh release download v0.0.7 -p 'php_rabbit_rs-v0.0.7_php8.4-x86_64-linux-glibc.zip' -D /tmp/pie-test
pie install /tmp/pie-test/php_rabbit_rs-v0.0.7_php8.4-x86_64-linux-glibc.zip
php -m | grep rabbit_rs
```

Si `pie install` résout le suffixe attendu (via la convention de nommage PIE documentée : `php_rabbit_rs-1.2.0_php8.5-x86_64-linux-glibc-nts.zip`), documenter le pattern obligatoire. Le nom **sans** suffixe `-nts` est-il résolu par PIE ? Si oui, le pattern actuel v0.0.7 est correct ; si non, corriger.

- [ ] **Step 2: Unify the naming**

Selon le résultat du Step 1, corriger `scripts/package-pie-binary.sh` OU `.github/workflows/release.yml` pour que **les deux produisent exactement le même pattern**, documenté dans `docs/distribution.md` avec un test de convention (script `scripts/verify-pie-naming.sh` vérifiant que chaque asset de la matrice respecte le pattern attendu par PIE).

- [ ] **Step 3: Add the CI verification job**

Dans `release.yml` (après le job de packaging, avant la publication) :

```yaml
verify-pie-install:
  name: PIE install end-to-end (glibc NTS x86_64 PHP 8.4)
  runs-on: ubuntu-latest
  needs: [build]
  steps:
    - uses: actions/checkout@v4
    - uses: php/pie-setup-action@v1
    - name: Download the drafted NTS asset
      run: gh release download "${{ needs.build.outputs.tag }}" -p '*php8.4-x86_64-linux-glibc*.zip' -D ./pie-assets
      env:
        GH_TOKEN: ${{ github.token }}
    - name: Install via PIE and smoke-test
      run: |
        pie install ./pie-assets/*.zip
        php -m | grep rabbit_rs
        php -r "echo phpversion('rabbit_rs'), PHP_EOL;"
```

(Versionner le job pour tester aussi musl et ZTS si PIE le permet sur le runner — au minimum NTS glibc bloquant.)

- [ ] **Step 4: Verify on a draft release**

Tagguer `v0.0.8` (ou similaire), lancer le workflow release complet, vérifier le job `verify-pie-install` vert, puis `pie install` local depuis la release publiée.

- [ ] **Step 5: Commit**

```bash
git add scripts .github docs/distribution.md
git commit -m "ci: validate PIE asset resolution end-to-end with unified naming"
```

---

### Task 14: Compléter les contrats Laravel (ClearableQueue, auto-subscribe)

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:31` (implements)
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:359-371` (pop fallback)
- Modify: `packages/laravel-queue/src/Support/WorkerProfileResolver.php`
- Modify: `packages/laravel-queue/config/rabbit-rs.php`
- Test: `packages/laravel-queue/tests/Unit/RabbitMqQueueAdminTest.php` + nouveau `AutoSubscribeTest.php`

**Contexte:** (1) `queue:clear rabbit-rs` échoue car `ClearableQueue` n'est pas déclaré alors que `clear()` existe. (2) `pop()` lève si la queue demandée n'est pas une subscription d'un worker profile — déviation majeure vs convention Laravel (`queue:work --queue=emails`).

- [ ] **Step 1: Write the failing tests**

```php
// tests/Unit/ClearableQueueTest.php
it('implements ClearableQueue so queue:clear works', function () {
    expect($this->app->make('queue')->connection('rabbit-rs'))
        ->toBeInstanceOf(Illuminate\Contracts\Queue\ClearableQueue::class);
});

// tests/Feature/AutoSubscribeTest.php
it('pops a plain queue by auto-subscribing when enabled', function () {
    config(['queue.connections.rabbit-rs.auto_subscribe' => true]);
    // fakes natifs du bootstrap : pop('emails') doit créer/réutiliser un
    // profil implicite contenant la subscription 'emails'
    $job = $this->app->make('queue')->connection('rabbit-rs')->pop('emails');
    // assert: le consumer natif a été demandé avec un profil dédié 'emails'
});

it('rejects a plain queue without auto_subscribe', function () {
    config(['queue.connections.rabbit-rs.auto_subscribe' => false]);
    $this->app->make('queue')->connection('rabbit-rs')->pop('emails');
})->throws(RuntimeException::class);
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/ClearableQueueTest.php tests/Feature/AutoSubscribeTest.php`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

1. `class RabbitMqQueue extends Queue implements QueueContract, ClearableQueue` (`Illuminate\Contracts\Queue\ClearableQueue`).
2. `pop($queue, $index = 0)` : si la valeur `queue` n'est ni un profil connu ni une subscription, et que `auto_subscribe => true` : construire à la volée un profil implicite `{name: "__auto__", subscriptions: [{broker: default, queue: $queue, weight: 1, prefetch: défaut}]}` (cache process-local par nom de queue, réutilisé aux pops suivants) et poursuivre le chemin existant. Si `auto_subscribe => false` : conserver l'erreur actuelle avec un message amélioré (« configure workers.*.subscriptions.*.queue=emails or enable auto_subscribe »).
3. `config/rabbit-rs.php` : `'auto_subscribe' => false` (opt-in, documenté).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest && rtk ./scripts/check.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue
git commit -m "feat(laravel): implement ClearableQueue and optional auto-subscribe pop"
```

---

## Milestone P2 — Écosystème et DX

### Task 15: Façade de log, erreurs typées, audit des panics

**Files:**
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:47-66` (`CoordinatorError` typé) et `:256,295` (expect/eprintln)
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:169-187` (`wait_for_state` panic → erreur)
- Modify: `crates/rabbit-rs-core/Cargo.toml` (dépendance `log`)
- Modify: `crates/rabbit-rs-php/src/lib.rs` (init d'un logger minimal vers `error_log` quand config `debug`)
- Test: `crates/rabbit-rs-core/tests/recovery.rs` (adaptation) + audit manuel documenté

**Contexte:** `eprintln!` non capturable en prod, `CoordinatorError = String`, `.expect("connection actor started")` dans une tâche spawnée et un panic documenté dans `wait_for_state` : en contexte FFI PHP, une unwind non interceptée est un abort process.

- [ ] **Step 1: Typify CoordinatorError**

Remplacer `pub type CoordinatorError = String;` par :

```rust
#[derive(Debug)]
pub enum CoordinatorError {
    Topology(crate::topology::TopologyPlanError),
    Transport(TransportError),
    Internal(&'static str),
}
```

Adapter les sites de construction. Les `String` de raison deviennent des messages structurés portés par les variantes.

- [ ] **Step 2: Remove panics reachable from PHP**

- `run_coordinator:256` : `.expect("connection actor started")` → propagation d'erreur (`CoordinatorError::Transport`) et terminaison propre de la task avec log.
- `wait_for_state:169-187` : retourner `Result<(), CoordinatorError>` quand le watch channel meurt au lieu de paniquer ; les appelants traitent l'erreur.
- `eprintln!(recovery_coordinator.rs:295)` : remplacer par `log::warn!("recovery generation {generation} failed: {error}")`.

- [ ] **Step 3: Audit complet des panics atteignables**

Run: `rtk cargo test --workspace 2>&1 | true; rg -n 'unwrap\(\)|expect\(|panic!\(todo' crates/rabbit-rs-core/src crates/rabbit-rs-php/src --type rust`
Pour chaque `expect`/`unwrap` atteignable depuis une opération PHP (frontière FFI) : soit justification documentée en commentaire (invariant prouvé), soit conversion en erreur typée. Documenter la liste dans `docs/reliability.md` (section « Panic policy »).

- [ ] **Step 4: Wire the log facade to PHP**

Ajouter `log = "0.4"` au core (sans subscriber — le core n'impose pas de backend). Dans l'extension, `MINIT`/premier usage : installer un logger minimal (crate `env_logger` ou writer custom) qui route vers `error_log()` PHP quand la clé `debug => true` est présente dans la config du Pool ; sinon no-op. Aucun zval capté dans le logger.

- [ ] **Step 5: Verify and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates
git commit -m "refactor(core): typed coordinator errors, no reachable panics, log facade"
```

---

### Task 16: Aligner les versions et introduire le CHANGELOG

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php:123` (« ^1.0 » → contrainte réelle)
- Modify: `packages/laravel-queue/composer.json` + `composer.json` racine (synchronisation)
- Modify: `docs/installation.md:42` (version affichée)
- Create: `CHANGELOG.md` (racine) + `packages/laravel-queue/CHANGELOG.md`
- Test: `packages/laravel-queue/tests/Unit/ExtensionVersionTest.php` (nouveau)

**Contexte:** composer exige `ext-rabbit_rs: ^0.0`, l'exception parle de « ^1.0 », la doc affiche 1.0.0, le workspace vaut 0.0.7. Pas de CHANGELOG ni de notes d'upgrade.

- [ ] **Step 1: Write the failing test**

```php
it('states the same extension version constraint everywhere', function () {
    $composer = json_decode(file_get_contents(__DIR__.'/../../composer.json'), true);
    $constraint = $composer['require']['ext-rabbit_rs'];

    expect($constraint)->toBe('^0.0'); // aligné au workspace 0.0.x jusqu'à la 1.0
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/ExtensionVersionTest.php`
Expected: FAIL ou PASS selon l'état — le but est de verrouiller la cohérence.

- [ ] **Step 3: Align and document**

1. Décision : la contrainte `ext-rabbit_rs` suit la version du workspace (`^0.0` jusqu'à 1.0). Corriger le message de `RabbitMqServiceProvider.php:123` pour refléter la contrainte réelle.
2. Corriger `docs/installation.md` (version => 0.0.7, ou rendre l'exemple générique `php -i | grep rabbit_rs`).
3. Créer `CHANGELOG.md` (Keep a Changelog, sections Added/Changed/Fixed pour v0.0.3..v0.0.7 depuis les tags git) et `packages/laravel-queue/CHANGELOG.md` (miroir simplifié).
4. Ajouter au job de release une vérification de cohérence version (déjà présente : `release.yml` vérifie tag↔Cargo.toml — étendre à la contrainte ext du package Laravel).

- [ ] **Step 4: Verify and commit**

Run: `rtk ./scripts/check.sh && rtk composer validate --strict`
Expected: PASS.

```bash
git add CHANGELOG.md packages/laravel-queue docs
git commit -m "chore: align extension version constraints and add changelogs"
```

---

### Task 17: Réduire la friction d'installation et ajouter l'analyse statique PHP

**Files:**
- Modify: `packages/laravel-queue/composer.json:9` (`require` → `suggest` + `conflict`?)
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php:51` (validation runtime)
- Modify: `scripts/check.sh` (Pint + PHPStan)
- Create: `packages/laravel-queue/pint.json` + `phpstan.neon`
- Test: run Pint/PHPStan — fix iteratif

**Contexte:** `ext-rabbit_rs` en `require` dur fait échouer tout `composer install` (CI, builds d'artefacts, dev sans extension) alors que le check runtime existe déjà (`extension_loaded`, ligne 51). Aucune analyse statique PHP dans le quality gate.

- [ ] **Step 1: Decide and apply the dependency policy**

Passer `ext-rabbit_rs` de `require` à `suggest` avec message explicite, ET ajouter une validation runtime **bloquante à la première utilisation** (résolution de connexion) avec un message actionnable (« install the extension via `pie install goopil/rabbit-rs-native` »). Vérifier que `RabbitMqServiceProviderTest` (assertion « missing extension ») reste vert — adapter si nécessaire. Attention : les tests Unit/Feature tournent sans extension par contrat AGENTS.md — la validation ne doit pas s'exécuter au boot mais à la résolution du driver.

- [ ] **Step 2: Add Pint and PHPStan to the gate**

```bash
cd packages/laravel-queue
composer require --dev laravel/pint phpstan/phpstan larastan/larastan --with-all-dependencies
```

`packages/laravel-queue/pint.json` : preset laravel. `packages/laravel-queue/phpstan.neon` : level 6, analyse `src/`.

Dans `scripts/check.sh`, après `composer validate` :

```bash
(cd packages/laravel-queue && vendor/bin/pint --test)
(cd packages/laravel-queue && vendor/bin/phpstan analyse --no-progress --error-format=table)
```

- [ ] **Step 3: Fix all reported issues iterativement**

Run: `(cd packages/laravel-queue && vendor/bin/pint -v) && (cd packages/laravel-queue && vendor/bin/phpstan analyse)`
Fixer chaque violation (commits séparés par catégorie si volumineux). Boucler jusqu'à 0 erreur.

- [ ] **Step 4: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add packages/laravel-queue scripts/check.sh
git commit -m "chore(laravel): soft-depend on the extension and add pint/phpstan to the gate"
```

---

### Task 18: Durcir le superviseur de workers

**Files:**
- Modify: `packages/laravel-queue/src/Console/WorkerSupervisor.php:125-178`
- Test: `packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php` (extension)

**Contexte:** `run()` exige pcntl même avec `--workers=1` (le message d'erreur « Install it or run with --workers=1 » est faux) ; le `sleep($backoff)` (ligne 168) bloque la boucle de supervision de **tous** les enfants pendant le backoff d'un seul.

- [ ] **Step 1: Write the failing tests**

```php
it('runs a single worker inline without pcntl', function () {
    $supervisor = new WorkerSupervisor(workers: 1, options: [...]);
    // simuler l'absence de pcntl (la classe expose déjà le hook)
    $supervisor->shouldReceive('hasPcntl')->andReturn(false); // ou sousclasse de test
    // assert: le worker tourne en avant-plan (proc_open artisan queue:work)
    // sans exception SupervisorException
});

it('keeps supervising other children while one is in backoff', function () {
    // 2 enfants ; l'enfant 0 crashe ; pendant son backoff de N s,
    // l'enfant 1 doit être supervisé (poll non bloquant)
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/WorkerSupervisorIntegrationTest.php`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

1. `--workers=1` sans pcntl : chemin direct `proc_open('php artisan queue:work ...')` + wait + codes de sortie, sans fork. Corriger le message d'erreur pcntl (« ext-pcntl is required for --workers>1 »).
2. Backoff non bloquant : remplacer `sleep($this->backoffSeconds(...))` par un tableau `restartAt[$index] = microtime(true) + $backoff` ; la boucle de supervision consulte `restartAt` et ignore les tentatives de redémarrage tant que `microtime(true) < restartAt[$index]` (le reste du loop continue : `usleep(100_000)` existant à la ligne 178 assure déjà le polling).
3. Corriger les défauts identifiés dans l'évaluation : propagation `--sleep`, `--stop-when-empty`, supervision des logs enfants (option `--log-children` ou stdout mux) — selon la surface déjà présente dans `RabbitMqWorkCommand`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest && rtk ./scripts/check.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue
git commit -m "fix(laravel): pcntl-free single worker path and non-blocking restart backoff"
```

---

### Task 19: Aligner la documentation et le stub stats()

**Files:**
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php:82` (`@return` de `stats()` : documenter les 17 clés réelles)
- Modify: `packages/laravel-queue/README.md` (prefetch 16 vs 64 lignes 113/220 ; suites de test ; events)
- Modify: `docs/laravel.md:40` (retirer `dispatchBatch`)
- Modify: `packages/laravel-queue/tests/Pest.php:13-37` (retirer le helper `validConfig()` obsolète si inutilisé, sinon le migrer au nouveau schéma)
- Modify: `docs/operations.md` (prometheus/OTel : préciser « adaptateurs à venir »)
- Test: PHPT reflection existant

**Contexte:** Incohérences docs/code identifiées par l'audit : stub `stats()` documente 8 clés pour 17 réelles (`pool.rs:211-261`) ; prefetch annoncé 16, défaut 64 ; README renvoie à des suites PHPUnit inexistantes (« Rabbit RS Laravel ») alors que le projet utilise Pest (suite `default`) ; `docs/laravel.md` documente une API inexistante.

- [ ] **Step 1: Align the stats() stub**

Documenter chaque clé retournée (17) avec son type : `closed`, `pid`, `handle`, `publishes_total`, `confirmations_total`, `returns_total`, `backpressure_total`, `reconnects_total`, `deliveries_total`, `acks_total`, `rejects_total`, `confirmation_latency_p50|p95|p99`, `settlement_latency_p50|p95|p99` (vérifier la liste exacte depuis `pool.rs:204-261` et `insert_percentile`).

- [ ] **Step 2: Fix the package README and docs**

1. Unifier prefetch : soit corriger le README vers 64 (défaut réel `config/rabbit-rs.php:208`), soit corriger le défaut vers 16 (décision produit — défaut config prime, README suit).
2. Section Testing : référencer `php vendor/bin/pest` et les suites réelles (`default`, Integration).
3. `docs/laravel.md:40` : remplacer l'exemple `ProcessOrder::dispatchBatch($jobs)` par `$queue->bulk([...])` ou `Bus::batch`.
4. `docs/operations.md:231` : après Task 10, les events se déclenchent sur publish/consume — reformuler précisément.
5. `tests/Pest.php` : retirer `validConfig()` si aucun test ne l'utilise (`rg validConfig packages/laravel-queue/tests`), sinon migrer.

- [ ] **Step 3: Verify**

Run: `rtk ./scripts/check.sh && rtk ./scripts/test-extension.sh && cd packages/laravel-queue && php vendor/bin/pest`
Expected: PASS (le PHPT de réflexion valide le stub — `php -l` sur le stub).

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-php/stubs packages/laravel-queue docs
git commit -m "docs: align stats stub, README, guides with the implementation"
```

---

### Task 20: Trancher et appliquer la décision ZTS

**Files:**
- Modify: `composer.json` (racine, méta PIE : `support-zts`)
- Modify: `release/pie-matrix.json` (16 → 8 combinaisons)
- Modify: `.github/workflows/release.yml` (matrice build `ts: ["nts", "zts"]` → `["nts"]`)
- Modify: `docs/distribution.md` + `docs/installation.md`
- Modify: `.github/workflows/ci.yml:192-196` (job ZTS advisory — suppression ou passage bloquant selon option)

**Contexte:** `support-zts: true` est annoncé sans preuve : le `RuntimeRegistry` global est partagé entre threads PHP sous ZTS (course potentielle sur refcount Zend via `shallow_clone()` des callbacks), le job CI ZTS est advisory-only (`continue-on-error: true`), et les 8 artefacts ZTS de release ne subissent qu'un smoke-test `extension_loaded`. Expédier des binaires ZTS non testés fonctionnellement est le risque mémoire le plus concret du projet.

**Décision (recommandée — Option A) :** retirer ZTS du périmètre V1 et le réintroduire en V2 avec isolation par thread + CI bloquante + tests de concurrence réels.

- [ ] **Step 1: Write the failing consistency check**

Ajouter à `scripts/verify-pie-naming.sh` (créé par la Task 13) une vérification que la matrice déclarée dans `release/pie-matrix.json` ne contient aucune entrée `zts` tant que `support-zts` est retiré :

```bash
# fail if any zts entry remains while support-zts is false
if grep -qi 'zts' release/pie-matrix.json; then
  echo "ERROR: zts entries found in pie-matrix.json after ZTS removal (Task 20)"; exit 1
fi
```

- [ ] **Step 2: Apply Option A**

1. `composer.json` racine : `"support-zts": false` dans la section `php-ext`.
2. `release/pie-matrix.json` : retirer les 8 entrées ZTS.
3. `.github/workflows/release.yml` : `ts: ["nts"]` (et simplifier la logique conditionnelle `TS_SUFFIX` lignes 158-159).
4. `.github/workflows/ci.yml` : supprimer le job ZTS advisory.
5. `docs/distribution.md` + `docs/installation.md` : documenter « NTS only in V1; ZTS planned for V2 » avec la justification (registry process-global, isolation TSRM non implémentée).

- [ ] **Step 3: Verify the release matrix**

Run: `./scripts/verify-pie-naming.sh && rtk composer validate --strict`
Expected: PASS. Vérifier aussi `release/validate-distribution.sh` s'il référence ZTS.

- [ ] **Step 4: Commit**

```bash
git add composer.json release .github docs
git commit -m "build: drop unproven ZTS from the V1 release matrix (revisit in V2)"
```

> **Option B (rejetée pour V1, à documenter dans le PR) :** implémenter l'isolation
> par thread (registry TSRM-aware), passer le job CI ZTS en bloquant et ajouter des
> tests de concurrence — coût estimé plusieurs semaines, reporté.

---

## Critères de sortie vers 1.0

- [ ] Toutes les tâches P0 livrées et vérifiées en CI.
- [ ] Toutes les tâches P1 livrées ; la chaîne `pie install` validée sur une release réelle (Task 13).
- [ ] Task 12 (TLS) validée sur le lab 3 nœuds avec handshake, CA non fiable et SNI.
- [ ] `./scripts/check.sh` vert + Pint/PHPStan 0 erreur + coverage non régressée (Codecov).
- [ ] ZTS : décision tranchée et appliquée (Task 20 — Option A par défaut).
- [ ] CHANGELOG 1.0 rédigé, contraintes de versions alignées, docs cohérentes.
- [ ] Certification CLI, FPM et les 4 serveurs Octane annoncés (débordement : `scripts/test-octane.sh` sur chaque runtime).
- [ ] Round 2 (stall/pre-fill/clear) root-caused et re-bench comparé aux archives Phase E.
