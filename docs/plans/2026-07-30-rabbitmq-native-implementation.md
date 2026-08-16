# Rabbit RS Native PHP Extension and Laravel Queue Driver Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Livrer l'écosystème Rabbit RS : l'extension PHP rabbit_rs et le package goopil/rabbit-rs-laravel, performants, at-least-once et capables de mutualiser publication et consommation sur plusieurs vhosts avec reconnexion automatique.

**Architecture:** Un workspace Rust contient rabbit-rs-core et l'extension rabbit-rs-php construisant ext-rabbit_rs. Le package Composer goopil/rabbit-rs-laravel adapte cette API aux contrats Laravel Queue sans remplacer Illuminate\Queue\Worker. Les connexions et channels sont pilotés par des acteurs Tokio par processus PHP, tandis qu'un laboratoire RabbitMQ reproductible valide performances et scénarios de panne.

**Tech Stack:** Rust stable, Tokio, Lapin, ext-php-rs, PHP 8.4/8.5, PIE 1.5+, Composer, Packagist, Laravel 12/13, PHPUnit, Orchestra Testbench, RabbitMQ 4.3, Docker Compose, Prometheus, Toxiproxy, Criterion.

---

## Règles d'exécution

- Appliquer @superpowers:test-driven-development à chaque comportement.
- Utiliser @superpowers:systematic-debugging à tout échec inattendu.
- Exécuter @superpowers:verification-before-completion avant chaque jalon.
- Ne jamais transporter de valeur Zend dans un thread Rust.
- Garder Lapin derrière l'interface Transport.
- Préserver les changements utilisateur non liés.
- Un commit logique après chaque tâche verte.
- Ne pas figer les valeurs de batching ou de prefetch avant le jalon benchmark.
- Conserver dans une capacité globale bornée les publications non envoyées ou ambiguës pendant une coupure, puis les rejouer automatiquement avec le même message_id et la deadline originale après recovery.
- Ne pas présenter cette rétention mémoire comme durable : un crash du processus nécessite un outbox externe pour garantir le replay.

## Avancement

**Dernière mise à jour :** 16 août 2026

**Branche d'implémentation :** feature/laravel-package

**Prochaine étape :** Milestone E — Performance — Task 31 — Câbler le TLS end-to-end.

- [x] Task 1 — Workspace Rust/PHP reproductible (`4f2a997`).
- [x] Task 2 — Configuration normalisée et validée (`c324929`).
- [x] Task 3 — Scheduler pondéré sans famine (`17804d0`).
- [x] Task 4 — Runtime par processus sûr après fork (`ca5dd36`).
- [x] Task 5 — Abstraction Transport, mock scriptable et Lapin (`71680e1`).
- [x] Task 6 — Acteur de connexion et recovery déterministe (`70d5b59`).
- [x] Task 7 — Topologie declare, verify et external (`7ff2de9`).
- [x] Task 8 — Publisher borné, batching, confirms et mandatory returns (`90d3089`).
- [x] Task 9 — Délais par plugin et fallback TTL (`bae220b`).
- [x] Task 9 bis — Rétention bornée et replay publisher après reconnexion (`241f77d`).
- [x] Task 10 — ConsumerSet et jetons de delivery (`380a95d`).
- [x] Task 11 — Compteurs attempts et poison-message (`eb35412`).
- [x] Task 12 — Snapshot de métriques et gate du Milestone A (`21aedee`).
- [x] Task 13 — Définir l'API et les stubs PHP du Milestone B.
- [x] Task 14 — Tester conversions, erreurs et transitions PHP.
- [x] Task 15 — Certifier le cycle de vie CLI, fork et FPM.
- [x] Task 16 — Initialiser le package et sa configuration.
- [x] Task 17 — Enregistrer le connecteur et le pool partagé.
- [x] Task 18 — Implémenter push, later et bulk.
- [x] Task 19 — Implémenter RabbitMqJob.
- [x] Task 20 — Brancher pop sur un profil multi-vhost.
- [x] Task 21 — Implémenter size, clear et monitoring (`d8bafcf`).
- [x] Task 22 — Ajouter événements natifs et commande de diagnostic (`950819b`).
- [x] Task 23 — Ajouter la commande multiprocessus progressive (`de8d8bf`).
- [x] Task 24 — Certifier Octane (`4f04b63`).
- [x] Task 25 — Créer le cluster RabbitMQ de test.
- [x] Task 26 — Écrire les tests d'intégration end-to-end.
- [x] Task 27 — Écrire les scénarios de panne (chaos/fault injection).
- [x] Task 28 — Implémenter le coordinateur de recovery.
- [x] Task 29 — Implémenter le delay routing côté éditeur.
- [x] Task 30 — Câbler la DLQ et les arguments de queue génériques.

## Milestone D2 — Recovery, delay et topology (gaps d'implémentation)

Ce jalon corrige les gaps identifiés par l'audit du 16 août 2026 : le coordinateur de recovery manquant, le delay routing côté éditeur non branché, et la DLQ/arguments génériques non câblés.

### Task 28: Implémenter le coordinateur de recovery

**Files:**
- Create: crates/rabbit-rs-core/src/pool/recovery_coordinator.rs
- Modify: crates/rabbit-rs-core/src/client.rs
- Modify: crates/rabbit-rs-core/src/pool/mod.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Modify: crates/rabbit-rs-core/src/consumer/set.rs
- Create: crates/rabbit-rs-core/tests/recovery_coordinator.rs

**Contexte :**

Les primitives de recovery sont complètes (`ConnectionActor` avec backoff/génération, `PublisherActor` avec replay buffer borné, `TopologyReconciler` avec replay par génération, `ConsumerActor` avec `UpdateGeneration`), mais aucun coordinateur ne les relie. Le `ClientPool` ouvre les connections paresseusement et n'observe jamais leur perte. Les tests de chaos recréent les pools manuellement après chaque panne.

**Step 1: Write failing recovery coordinator tests**

Scénarios de test (mock transport, pas de broker réel) :

1. Une connection est perdue → `PublisherActor` reçoit `Recovering` → les unconfirmed enters dans le replay buffer → la connection est rétablie → `TopologyReconciler` rejoue → `PublisherActor` reçoit `Ready { topology_restored: true }` → replay est flushé → les messages sont délivrés.
2. Une connection est perdue → `ConsumerActor` reçoit `UpdateGeneration` après reconnexion → les deliveries de l'ancienne génération sont rejetées (`StaleGeneration`) → le broker redelivre.
3. Ordre déterministe vérifié : connection → channels → exchanges → queues → bindings → QoS → consumers → publisher replay.
4. Perte pendant le recovery → le coordinateur annule et relance.
5. Erreur permanente (credentials) → `FailedPermanent` → le coordinateur ne boucle pas.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test recovery_coordinator

Expected: FAIL — le coordinateur n'existe pas encore.

**Step 3: Implement the recovery coordinator**

Le coordinateur est une task par broker qui :

1. Spawne un `ConnectionActor` et souscrit à son `watch::Receiver<ConnectionState>`.
2. Sur `ConnectionLost` (erreur de transport détectée) → `ConnectionActor::connection_lost(error)` → émet `PublisherConnectionEvent::Recovering` au `PublisherActor` du broker.
3. Sur `ConnectionState::Ready { generation }` → ouvre un nouveau `PublisherChannel`, exécute `TopologyReconciler::reconcile(channel, plan, generation)`, puis émet `PublisherConnectionEvent::Ready { generation, channel, topology_restored: true }` au `PublisherActor`.
4. Pour les consumers → ouvre de nouveaux `ConsumerChannel`, ré-applique QoS, ré-émet `basic_consume`, et appelle `ConsumerHandle::update_generation` pour chaque subscription.
5. Enforce l'ordre déterministe : connection → channels → exchanges → queues → bindings → QoS → consumers → publisher replay.

Le `ClientPool` doit :
- Spawner un coordinateur par broker à l'initialisation de la connection.
- Stocker le `ConnectionActorHandle` et le `JoinHandle` du coordinateur.
- Sur `close()`, annuler le coordinateur et l'acteur de connection.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test recovery_coordinator

Expected: PASS.

**Step 5: Update chaos tests to remove manual pool recreation**

Modifier `crates/rabbit-rs-core/tests/chaos/reconnect.rs` :
- Supprimer le pattern de recréation de `ClientPool` après chaque panne.
- Les tests doivent créer un seul `ClientPool`, injecter la panne, et vérifier que le pool se rétablit automatiquement.
- `missing = 0` doit être maintenu sans intervention manuelle.

Run: cargo test -p rabbit-rs-core --features integration --test chaos_reconnect

Expected: PASS.

**Step 6: Run full quality gate**

Run: ./scripts/check.sh

Expected: PASS.

**Step 7: Commit**

    git add crates
    git commit -m "feat(core): wire recovery coordinator end-to-end"

### Task 29: Implémenter le delay routing côté éditeur

**Files:**
- Modify: crates/rabbit-rs-core/src/transport.rs
- Modify: crates/rabbit-rs-core/src/transport/lapin.rs
- Modify: crates/rabbit-rs-core/src/publisher/actor.rs
- Modify: crates/rabbit-rs-core/src/publisher/mod.rs
- Modify: crates/rabbit-rs-core/src/client.rs
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: crates/rabbit-rs-core/src/topology/plan.rs
- Modify: crates/rabbit-rs-core/src/topology/reconciler.rs
- Modify: crates/rabbit-rs-core/src/topology/delay.rs
- Modify: crates/rabbit-rs-core/src/publisher/delay.rs
- Create: crates/rabbit-rs-core/tests/publisher_delay.rs
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Modify: packages/laravel-queue/tests/Integration/DelayedJobTest.php

**Contexte :**

Le `DelayRouter` existe et est testé, mais il n'est branché que dans `release()` du consumer. Côté éditeur, `later()` pose le header `x-delay` sur l'exchange original — effet no-op. Les exchanges `x-delayed-message` ne peuvent pas être déclarés car `ExchangeSpec` n'a pas d'arguments. Les TTL delay queues ne sont jamais déclarées. `DelayConfig` n'est pas dans `ValidatedConfig`. La config Laravel n'expose pas de section delay.

**Step 1: Write failing publisher delay tests**

Scénarios :

1. `publish()` avec `delay_ms > 0` en mode Plugin → le message est publié sur l'exchange `x-delayed-message` (pas l'exchange original) avec le header `x-delay`.
2. `publish()` avec `delay_ms > 0` en mode TTL → le message est publié sur une TTL queue avec `x-message-ttl` et dead-letter vers la destination originale.
3. `publish()` avec `delay_ms = 0` → pas de routing spécial (comportement normal).
4. L'exchange `x-delayed-message` est déclaré par le `TopologyReconciler` quand le mode Plugin est actif.
5. Les TTL queues sont déclarées paresseusement (on-demand) par le publisher.
6. `DelayConfig` est validé et désérialisé depuis la config.
7. `DelayMode::Auto` détecte le plugin et fallback TTL si absent.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test publisher_delay

Expected: FAIL.

**Step 3: Implement exchange arguments and delayed exchange support**

- Ajouter `arguments: BTreeMap<String, HeaderValue>` à `ExchangeSpec`.
- Ajouter `ExchangeKind::Delayed(ExchangeKind)` qui émet `x-delayed-message` comme type d'exchange et `x-delayed-type` comme argument (avec le type sous-jacent : direct, topic, etc.).
- Mettre à jour `lapin.rs::declare_exchange()` pour passer les arguments au lieu de `FieldTable::default()`.
- Le `TopologyReconciler` doit déclarer l'exchange `x-delayed-message` (nom `rabbit-rs.delayed` ou `{exchange}.delayed`) quand le mode Plugin est sélectionné.

**Step 4: Wire DelayRouter into the publisher path**

- Le `PublisherActor` (ou `ClientPool::publish()`) doit détecter `delay_ms > 0`, résoudre la `DelayStrategy` via `DelayStrategyResolver`, appeler `DelayRouter::route()`, et publier vers l'exchange/queue différée au lieu de l'original.
- En mode TTL, déclarer paresseusement la TTL queue avant la première publication différée (idempotent via cache).
- En mode Plugin, l'exchange différée est déclaré par le `TopologyReconciler` lors du recovery.

**Step 5: Add DelayConfig to ValidatedConfig**

- Ajouter `delay: DelayConfig` à `Config` et `ValidatedConfig`.
- Désérialiser `mode` (auto/plugin/ttl), `buckets`, `max_buckets`, `queue_expiry_margin`, `detection_timeout`.
- Valider : buckets non vide, ≤ max_buckets, sans zéro, detection_timeout borné.

**Step 6: Wire DelayConfig through ClientPool**

- Le `ClientPool` doit instancier un `DelayStrategyResolver` par broker et le passer au publisher et consumer.
- Le `ConsumerSet` doit recevoir la `DelayStrategy` résolue (au lieu du hardcoded `Plugin`).
- Le `ClientPool::consumer()` doit appeler `.delayed_publisher()` et `.delay_strategy()` sur chaque subscription.

**Step 7: Expose delay config in Laravel**

- Ajouter une section `delay` à `config/rabbit-rs.php` : `mode`, `buckets`, `max_buckets`, `queue_expiry_margin`, `detection_timeout`.
- `ConfigNormalizer` doit mapper cette section vers la config native.

**Step 8: Un-skip and fix the Laravel integration test**

- Supprimer `markTestSkipped` de `test_later_publishes_and_consumes_after_delay`.
- Le test doit publier avec `later(2, ...)` et vérifier que le job n'est pas immédiatement disponible, puis l'est après le délai.

**Step 9: Verify**

Run: cargo test -p rabbit-rs-core --test publisher_delay
Run: ./scripts/test-integration.sh

Expected: PASS.

**Step 10: Commit**

    git add crates packages
    git commit -m "feat(core): wire publisher-side delay routing and config"

### Task 30: Câbler la DLQ et les arguments de queue génériques

**Files:**
- Modify: crates/rabbit-rs-core/src/transport.rs
- Modify: crates/rabbit-rs-core/src/transport/lapin.rs
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: crates/rabbit-rs-core/src/topology/plan.rs
- Modify: crates/rabbit-rs-core/src/topology/reconciler.rs
- Create: crates/rabbit-rs-core/tests/dlq_topology.rs
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php

**Contexte :**

La compilation DLQ (`TopologyPlan::compile` avec `DeadLetterDefinition`) est implémentée et testée en Rust, mais n'est pas configurable via `ValidatedConfig`. La config Laravel expose `dead_letter => null` et `delivery_limit => 20` mais ces valeurs sont validées puis droppées. `QueueSpec` n'a pas d'arguments génériques pour `x-delivery-limit`, `x-max-priority`, etc.

**Step 1: Write failing DLQ config tests**

Scénarios :

1. Config avec `dead_letter` non-null → `ValidatedConfig` contient un `DeadLetterConfig` → `TopologyDefinition` est compilé avec `with_dead_letter` → le `TopologyReconciler` déclare le DLX, la DLQ et le binding.
2. Config avec `delivery_limit: 20` → `QueueSpec` contient `x-delivery-limit: 20` → le `TopologyReconciler` déclare la queue avec cet argument.
3. Config sans `dead_letter` → pas de DLQ (comportement par défaut).
4. `ConfigNormalizer` Laravel mappe `topology.dead_letter` et `topology.queue.delivery_limit` vers la config native.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test dlq_topology

Expected: FAIL.

**Step 3: Add generic queue arguments**

- Ajouter `arguments: BTreeMap<String, HeaderValue>` à `QueueSpec` (en plus des champs structurés existants).
- Mettre à jour `lapin.rs::declare_queue()` pour fusionner les arguments structurés (DLX, TTL, etc.) et les arguments génériques.
- Ajouter `delivery_limit: Option<u32>` à `QueueSpec` → émet `x-delivery-limit`.

**Step 4: Add DeadLetterConfig to ValidatedConfig**

- Créer `DeadLetterConfig` struct : `enabled: bool`, `exchange: String`, `queue: String`, `routing_key: Option<String>`.
- Ajouter `dead_letter: Option<DeadLetterConfig>` à `Config`/`ValidatedConfig`.
- Wire : `ValidatedConfig.dead_letter` → `TopologyDefinition::with_dead_letter` → `TopologyPlan::compile` → `TopologyReconciler::reconcile`.

**Step 5: Wire Laravel config to native config**

- `ConfigNormalizer` doit transformer `topology.dead_letter` (null ou array avec `exchange`, `queue`, `routing_key`) vers la config native `dead_letter`.
- `ConfigNormalizer` doit transformer `topology.queue.delivery_limit` vers la config native (field `delivery_limit` sur les queues).
- Le connector doit passer ces valeurs à la config native du `Pool`.

**Step 6: Verify**

Run: cargo test -p rabbit-rs-core --test dlq_topology
Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 7: Commit**

    git add crates packages
    git commit -m "feat(core): wire DLQ config and generic queue arguments"

### Task 31: Câbler le TLS end-to-end

**Files:**
- Modify: crates/rabbit-rs-core/src/transport.rs
- Modify: crates/rabbit-rs-core/src/transport/lapin.rs
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Create: crates/rabbit-rs-core/tests/tls.rs

**Contexte :**

`TlsConfig` existe avec `enabled` et `server_name`, mais `server_name` n'est jamais lu par le transport. Le scheme `amqps` est posé via l'URI, mais aucune configuration de connecteur TLS (SNI, CA certs, cert client, mode de vérification) n'est passée à Lapin. Aucun test TLS n'existe.

**Step 1: Write failing TLS tests**

Scénarios :

1. `tls.enabled = true` + `server_name = "rabbit.example.com"` → l'URI utilise `amqps://` et `server_name` est passé à Lapin pour SNI.
2. `tls.enabled = false` → l'URI utilise `amqp://`.
3. `tls.enabled = true` sans `server_name` → utilise le premier host comme SNI.
4. Config avec `ca_cert`, `client_cert`, `client_key` → passés au connecteur TLS.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test tls

Expected: FAIL.

**Step 3: Implement TLS connector configuration**

- Étendre `TlsConfig` : ajouter `ca_cert: Option<PathBuf>`, `client_cert: Option<PathBuf>`, `client_key: Option<PathBuf>`, `verify: Option<TlsVerify>` (default `Peer`).
- Mettre à jour `lapin.rs` : utiliser `ConnectionProperties::with_ssl` ou construire un `tls::Connector` rustls avec SNI (`server_name`), CA, cert client.
- Utiliser `server_name` pour SNI quand fourni, sinon le premier host.
- Garder le scheme `amqps` dans l'URI quand `enabled`.

**Step 4: Expose TLS settings in Laravel config**

- Ajouter `ca_cert`, `client_cert`, `client_key`, `verify` à `config/rabbit-rs.php` sous `brokers.default.tls`.
- `ConfigNormalizer` doit mapper ces champs vers la config native.

**Step 5: Verify**

Run: cargo test -p rabbit-rs-core --test tls
Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 6: Commit**

    git add crates packages
    git commit -m "feat(core): wire TLS connector configuration end-to-end"

### Task 32: Câbler le nettoyage des consumers et éviter les fuites de channels

**Files:**
- Modify: crates/rabbit-rs-core/src/consumer/set.rs
- Modify: crates/rabbit-rs-php/src/classes/consumer.rs
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Modify: packages/laravel-queue/src/Octane/OctaneLifecycle.php
- Create: crates/rabbit-rs-core/tests/consumer_cleanup.rs

**Contexte :**

`RabbitMqQueue` cache les `Consumer` dans `$this->consumers` mais n'appelle jamais `close()`. Pas de `__destruct`. `ConsumerHandle` n'a pas de `Drop` qui envoie `Close`. En process long (Octane, daemons), les channels AMQP fuient.

**Step 1: Write failing consumer cleanup tests**

Scénarios :

1. `RabbitMqQueue::__destruct()` → appelle `$consumer->close()` pour chaque consumer caché → les channels sont fermés.
2. `ConsumerHandle::Drop` → envoie `Close` au actor (best-effort) → les channels sont fermés même si PHP ne appelle pas `close()`.
3. `OctaneLifecycle::flush()` → ferme les consumers de la queue courante (pas seulement le pool factory).
4. Après `close()`, `pop()` retourne `null` ou lève une erreur typée (pas de panic).

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test consumer_cleanup

Expected: FAIL.

**Step 3: Implement Rust-side Drop safety net**

- Implémenter `Drop` pour `ConsumerHandle` : envoie `ConsumerCommand::Close` (best-effort, non-bloquant via `try_send`).
- Assurer que le `Close` handler dans l'actor ferme les channels même si reçu via Drop.

**Step 4: Implement PHP-side cleanup**

- `Consumer` PHP : ajouter `__destruct()` qui appelle `close()` si pas déjà fermé.
- `RabbitMqQueue` : ajouter `closeConsumers()` qui ferme tous les consumers cachés, et `__destruct()` qui l'appelle.
- `OctaneLifecycle::flush()` : appeler `closeConsumers()` sur la queue courante (si disponible).

**Step 5: Verify**

Run: cargo test -p rabbit-rs-core --test consumer_cleanup
Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 6: Commit**

    git add crates packages
    git commit -m "fix(core): wire consumer cleanup and prevent channel leaks"

### Task 33: Dispatcher les events Laravel depuis l'extension native

**Files:**
- Modify: crates/rabbit-rs-php/src/classes/pool.rs
- Modify: crates/rabbit-rs-php/src/lib.rs
- Create: crates/rabbit-rs-php/src/callbacks.rs
- Modify: crates/rabbit-rs-core/src/metrics.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php
- Modify: packages/laravel-queue/src/Events/ConnectionStateChanged.php
- Modify: packages/laravel-queue/src/Events/BackpressureDetected.php
- Create: packages/laravel-queue/tests/Feature/NativeEventDispatchTest.php

**Contexte :**

`ConnectionStateChanged` et `BackpressureDetected` sont définis mais jamais dispatchés. Il n'existe pas de mécanisme FFI pour signaler les changements d'état de Rust vers PHP. Les events sont du dead code.

**Step 1: Write failing event dispatch tests**

Scénarios :

1. Une connection est perdue → l'event `ConnectionStateChanged` est dispatché avec `state = "recovering"`.
2. La connection est rétablie → l'event `ConnectionStateChanged` est dispatché avec `state = "ready"` et `generation` incrémenté.
3. Le publisher atteint la capacité → l'event `BackpressureDetected` est dispatché avec `inFlight` et `capacity`.
4. Les events sont dispatchés via le système d'events Laravel (Event::dispatch).

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/NativeEventDispatchTest.php

Expected: FAIL.

**Step 3: Implement FFI callback mechanism**

- Côté Rust (PHP extension) : enregistrer des callbacks PHP (closures) via `Pool::onConnectionState(callback)` et `Pool::onBackpressure(callback)`.
- Stocker les callbacks dans le `Pool` PHP (Zend objects, jamais en threads Rust — les callbacks sont invoqués sur le thread PHP via `block_on`).
- Le `ConnectionActor` publie `ConnectionState` via `watch` ; le `Pool` PHP poll le `watch::Receiver` lors des opérations synchrones et invoque le callback si l'état a changé.
- Le `Metrics` atomic `backpressure_total` peut être comparé entre deux appels à `stats()` pour détecter le backpressure et invoquer le callback.

**Step 4: Wire events in Laravel**

- `RabbitMqServiceProvider` : enregistrer les callbacks par défaut qui dispatch les events Laravel.
- `RabbitMqQueue` : exposer `onConnectionState()` et `onBackpressure()` pour override.

**Step 5: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/NativeEventDispatchTest.php

Expected: PASS.

**Step 6: Commit**

    git add crates packages
    git commit -m "feat(laravel): dispatch native events for connection state and backpressure"

### Task 34: Exposer les métriques consumer et latences

**Files:**
- Modify: crates/rabbit-rs-php/src/classes/pool.rs
- Modify: crates/rabbit-rs-core/src/metrics.rs
- Modify: packages/laravel-queue/src/Console/RabbitMqStatusCommand.php
- Modify: packages/laravel-queue/tests/Feature/RabbitMqStatusCommandTest.php

**Contexte :**

3 compteurs (`deliveries_total`, `acks_total`, `rejects_total`) et 2 histogrammes (`confirmation_latency`, `settlement_latency`) sont collectés en Rust mais non exposés à PHP. Le status command ne montre que les métriques éditeur.

**Step 1: Write failing metrics tests**

Scénarios :

1. `Pool::stats()` inclut `deliveries_total`, `acks_total`, `rejects_total`.
2. `Pool::stats()` inclut `confirmation_latency_p50`, `confirmation_latency_p95`, `confirmation_latency_p99`.
3. `Pool::stats()` inclut `settlement_latency_p50`, `settlement_latency_p95`, `settlement_latency_p99`.
4. `RabbitMqStatusCommand` affiche les métriques consumer et les latences.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/RabbitMqStatusCommandTest.php

Expected: FAIL.

**Step 3: Expose metrics to PHP**

- `pool.rs::stats()` : ajouter `deliveries_total`, `acks_total`, `rejects_total` depuis `MetricsSnapshot`.
- `pool.rs::stats()` : calculer percentiles (p50/p95/p99) depuis les histogrammes atomiques et les exposer.
- `RabbitMqStatusCommand` : afficher les nouvelles métriques.

**Step 4: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/RabbitMqStatusCommandTest.php

Expected: PASS.

**Step 5: Commit**

    git add crates packages
    git commit -m "feat(metrics): expose consumer metrics and latency histograms to PHP"

### Task 35: Câbler la config publisher (confirms, mandatory, timeout)

**Files:**
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: crates/rabbit-rs-core/src/client.rs
- Modify: crates/rabbit-rs-php/src/classes/pool.rs
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Support/MessageMapper.php

**Contexte :**

`publisher.confirms` et `publisher.mandatory` dans la config Laravel sont normalisés mais jamais passés au native `Pool`. `normalized['publisher']` n'arrive pas à `Pool::__construct()`. `confirm_timeout` est hardcoded à 30s. `timeout_ms` n'est pas envoyé par défaut dans `MessageMapper::map()`.

**Step 1: Write failing config publisher tests**

Scénarios :

1. Config avec `publisher.confirms = false` → le publisher n'active pas `confirm_select`.
2. Config avec `publisher.confirms = true` → le publisher active `confirm_select`.
3. Config avec `publisher.mandatory = false` → `basic_publish` avec `mandatory = false`.
4. Config avec `publisher.confirm_timeout = 5000` → le timeout de confirm est 5s.
5. `MessageMapper::map()` inclut `timeout_ms` depuis la config publisher par défaut.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: FAIL.

**Step 3: Wire publisher config to native**

- `ConfigNormalizer` : inclure `publisher` dans `normalized['native']` (pas seulement dans `normalized['publisher']`).
- Le native `Config` doit désérialiser `publisher.confirms`, `publisher.mandatory`, `publisher.confirm_timeout`.
- `PublisherConfig` dans `config.rs` : désérialiser depuis config au lieu de hardcoder.
- `client.rs::publisher_config()` : lire depuis la config validée.
- `MessageMapper::map()` : inclure `timeout_ms` par défaut depuis `publisher.confirm_timeout` quand pas explicitement fourni.

**Step 4: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 5: Commit**

    git add crates packages
    git commit -m "fix(core): wire publisher config (confirms, mandatory, timeout) end-to-end"

### Task 36: Câbler le lifecycle Octane complet

**Files:**
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php
- Modify: packages/laravel-queue/src/Octane/OctaneLifecycle.php
- Modify: packages/laravel-queue/tests/Feature/OctaneLifecycleTest.php

**Contexte :**

Seul `flush()` (no-op) est branché via `$app->terminating()`. `reload()` et `stop()` ne sont pas hookés aux events Octane. Les consumers cachés dans `RabbitMqQueue::$consumers` ne sont pas nettoyés entre requêtes Octane.

**Step 1: Write failing Octane lifecycle tests**

Scénarios :

1. Quand Octane reload est déclenché → `OctaneLifecycle::reload()` est appelé → les pools sont flushés.
2. Quand Octane worker stop est déclenché → `OctaneLifecycle::stop()` est appelé → les pools sont flushés et fermés.
3. Après `flush()` en fin de requête Octane → les consumers de la queue courante sont fermés (pas seulement le pool factory).
4. Le service provider enregistre les hooks Octane correctement quand `Laravel\Octane\Octane::class` existe.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/OctaneLifecycleTest.php

Expected: FAIL.

**Step 3: Wire Octane hooks**

- `RabbitMqServiceProvider::registerOctaneLifecycle()` :
  - Enregistrer `flush()` sur `Octane::tick()` ou `terminating` (déjà fait).
  - Enregistrer `reload()` sur l'event `WorkerReload` d'Octane.
  - Enregistrer `stop()` sur l'event `WorkerStopping` d'Octane.
- `OctaneLifecycle::flush()` : appeler `closeConsumers()` sur la queue courante en plus du flush du pool factory (dépend de Task 32).

**Step 4: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/OctaneLifecycleTest.php

Expected: PASS.

**Step 5: Commit**

    git add packages
    git commit -m "fix(laravel): wire full Octane lifecycle (reload, stop, consumer cleanup)"

### Task 37: Câbler le WorkCommand et tester le supervisor end-to-end

**Files:**
- Modify: packages/laravel-queue/src/Console/RabbitMqWorkCommand.php
- Modify: packages/laravel-queue/src/Console/WorkerSupervisor.php
- Create: packages/laravel-queue/src/Console/RabbitMqWorkCommandExtension.php
- Modify: packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php
- Create: packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php

**Contexte :**

`--rabbit-rs-worker={i}` est émis par le supervisor mais jamais consommé. La méthode `run()` (supervision, crash detection, restart, signaux) n'est pas testée end-to-end.

**Step 1: Write failing supervisor integration tests**

Scénarios :

1. Le supervisor spawn N workers → chaque worker reçoit `--rabbit-rs-worker={i}` → l'option est consommée pour le logging/metrics.
2. Un worker crash → le supervisor le redémarre avec backoff.
3. SIGTERM au supervisor → les workers sont arrêtés proprement.
4. `maxRestarts` atteint → le supervisor retourne `EXIT_MAX_RESTARTS`.
5. `--rabbit-rs-worker` est visible dans les logs du worker.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/WorkerSupervisorIntegrationTest.php

Expected: FAIL.

**Step 3: Implement option consumption**

- Créer un `WorkCommandExtension` (ou override `getOptions()` sur le `WorkCommand`) qui reconnait `--rabbit-rs-worker` et l'utilise pour le logging/metrics.
- Le `WorkerSupervisor::buildChildCommand()` doit passer l'option correctement.

**Step 4: Implement end-to-end tests**

- Tester `run()` avec mock processes (ou real processes avec un script PHP minimal).
- Vérifier crash detection, restart, backoff, signal handling, graceful shutdown.

**Step 5: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/WorkerSupervisorIntegrationTest.php

Expected: PASS.

**Step 6: Commit**

    git add packages
    git commit -m "fix(laravel): wire WorkCommand option and test supervisor end-to-end"

Ce lot a été exécuté dans le worktree dédié `.worktrees/strict-audit-stabilization` sur la branche `fix/strict-audit-stabilization`. Les corrections déterministes issues des audits des 31 juillet et 1er août sont terminées ; les qualifications nécessitant un environnement de production représentatif sont reportées aux jalons dédiés et ne bloquent pas le démarrage de la Task 16.

Périmètre initial des constats actifs :

- dispatch transactionnel des deliveries face aux waiters expirés ou annulés ;
- propagation de `message_id` et `correlation_id`, consumer tag canonique, rollback partiel de `ConsumerSet`, deadline de delayed release et état terminal après erreur de settlement ;
- commit générationnel de `ClientPool` face à `close()` sans mutex tenu pendant les opérations réseau ;
- budget global de 2 s pour la fermeture des clients, acteurs et runtime Tokio ;
- alignement des configurations et defaults core : scheduler, famine, attempts, jitter et publication mandatory ;
- confidentialité de `ConnectionKey` dans les identifiants, statistiques, erreurs et sorties `Debug` ;
- bornes PHP sur batches, payloads cumulés, headers, profondeur et timeouts, avec chemins d'erreur précis ;
- headers AMQP typés et propriétés de delivery conservées jusqu'à l'API PHP ;
- profils PHPT/FPM séparés, avec qualifications RabbitMQ-chaos, plateformes et performance reportées aux jalons dédiés.

État du lot clôturé au 1 août 2026 :

- [x] dispatch et cycle de vie des deliveries sécurisés, propriétés AMQP conservées et rollback partiel appliqué ;
- [x] `ClientPool` atomique face à `close()` avec initialisations réseau hors des registres ;
- [x] shutdown à budget global de 2 s, defaults core alignés et identités publiques expurgées ;
- [x] bornes et types de la frontière PHP ;
- **Report non bloquant** — laboratoire RabbitMQ-chaos et matrice plateformes ;
- **Report non bloquant** — baseline de performance.

Qualifications différées et critères de reprise :

- **RabbitMQ réel et chaos — Milestone D, Tasks 25–27 :** reprendre après le package Laravel du Milestone C, en créant d'abord le cluster, puis les tests d'intégration et enfin les scénarios de panne. La qualification at-least-once exige `missing = 0` et le comptage explicite des messages attendus, uniques, dupliqués et manquants ;
- **Performance — Milestone E, Tasks 28–30 :** établir d'abord la baseline de microbenchmarks FFI, conversion et batch issue de l'audit, puis exécuter les comparaisons Laravel et calibrer les defaults et budgets. Aucun seuil ne doit être fixé avant mesure sur une machine de référence documentée ;
- **Plateformes — Milestone F, Tasks 31–32 :** qualifier PHP 8.4/8.5, x86_64/ARM64, glibc/musl et NTS/ZTS avec la matrice PIE de 16 combinaisons, puis construire et smoke-tester chaque combinaison en CI.

Constats déjà corrigés à verrouiller par non-régression :

- transition publisher `Recovering` vers `Ready` pour une même génération ;
- historique `source_errors` borné ;
- settlement `Reject` disponible ;
- credentials Lapin construits sans concaténation d'URI exposable.

Baseline initiale du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk ./scripts/check.sh` : PASS, 112 tests Rust et validation Composer stricte ;
- `rtk cargo build -p rabbit-rs-php --release --features extension-tests` : PASS ;
- `rtk ./scripts/test-extension.sh` : PASS, 9 PHPT sur 9 ;
- `rtk ./scripts/test-fpm.sh` : PASS, laboratoire FPM à deux workers ;
- la build de distribution sans `extension-tests` reste distincte de la build PHPT afin de ne pas exposer `testing_pool()` dans l'artefact publié.

Checkpoint après sécurisation core du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk ./scripts/check.sh` : PASS, 141 tests Rust, Clippy sans warning et validation Composer stricte ;
- `rtk ./scripts/test-extension.sh` : PASS, 9 PHPT sur 9 ;
- `rtk ./scripts/test-fpm.sh` : PASS, laboratoire FPM à deux workers ;
- les tests couvrent le budget de shutdown partagé, la fermeture post-fork sans réacquisition, les courses de fermeture, la reprise publisher de même génération, la borne `source_errors`, le scheduler canonique et sa migration legacy, les defaults attempts/jitter/mandatory et l'absence d'empreinte de credentials dans les identifiants publics.

Checkpoint après bornage de la frontière PHP du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk ./scripts/check.sh` : PASS, 153 tests Rust dont 143 core, Clippy sans warning et validation Composer stricte ;
- `rtk ./scripts/test-extension.sh` : PASS, 11 PHPT sur 11 ;
- `rtk ./scripts/test-fpm.sh` : PASS, laboratoire FPM à deux workers ;
- les batches sont bornés à 256 messages et 1 Mio de payload cumulé, les headers à 128 entrées et 64 Kio cumulés par appel, et `timeout_ms` à 24 h avec addition contrôlée ;
- les types AMQP scalaires sont conservés, les headers PHP publiés restent plats et les structures broker imbriquées comme `x-death` sont omises des métadonnées sans masquer les scalaires ;
- les PHPT couvrent ACK, retour mandatory, timeout de confirmation, erreur transport typée, backpressure, settlements, fermeture active et chemins d'erreur `messages[index]`.

Checkpoint après initialisation du package Laravel du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk composer validate --strict` dans `packages/laravel-queue` : PASS ;
- PHPUnit avec Laravel 13.23, Testbench 11 et PHPUnit 12 : PASS, 12 tests et 34 assertions ;
- PHPUnit avec Laravel 12.64, Testbench 10 et PHPUnit 11 : PASS, 12 tests et 34 assertions ;
- `rtk ./scripts/check.sh` : PASS ;
- la configuration publiée applique les defaults confirms/mandatory, quorum durable et absence de DLQ applicative, puis normalise brokers, routes et workers vers le format natif avec erreurs par chemin et sans fuite de secrets.

Checkpoint après enregistrement du connecteur Laravel du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- PHPUnit avec Laravel 13.23, Testbench 11 et PHPUnit 12 : PASS, 24 tests et 53 assertions ;
- PHPUnit avec Laravel 12.64, Testbench 10 et PHPUnit 11 : PASS, 24 tests et 53 assertions ;
- `rtk ./scripts/check.sh` : PASS ;
- le connecteur `rabbit-rs` partage un pool natif process-local par empreinte de configuration normalisée, invalide son cache après fork et ne conserve pas les valeurs liées à une requête ;
- `RabbitMqQueue` est introduit comme squelette contractuel afin que `Queue::connection()` puisse appliquer immédiatement le conteneur et le nom de connexion ; ses opérations restent réservées à la Task 18.

Checkpoint après implémentation des publications Laravel du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk composer validate --strict` dans `packages/laravel-queue` : PASS ;
- PHPUnit avec Laravel 13.23, Testbench 11 et PHPUnit 12 : PASS, 38 tests et 100 assertions ;
- PHPUnit avec Laravel 12.64, Testbench 10 et PHPUnit 11 : PASS, 38 tests et 100 assertions ;
- `rtk ./scripts/check.sh` : PASS ;
- `push`, `pushRaw`, `later` et `bulk` transmettent des enveloppes natives à identifiant UUID stable, résolvent les routes et les placeholders de queue, préservent les payloads bruts et utilisent un seul appel natif par batch immédiat ou différé ;
- la publication reste pilotée par `Illuminate\Queue\Queue` pour les payloads, événements et transactions, avec délais en millisecondes, erreurs natives génériques traduites en `QueueException` et backpressure/connexion conservées comme erreurs dédiées.

Checkpoint après adaptation des deliveries en jobs Laravel du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk composer validate --strict` dans `packages/laravel-queue` : PASS ;
- PHPUnit avec Laravel 13.23, Testbench 11 et PHPUnit 12 : PASS, 46 tests et 135 assertions ;
- PHPUnit avec Laravel 12.64, Testbench 10 et PHPUnit 11 : PASS, 46 tests et 135 assertions ;
- `rtk ./scripts/check.sh` : PASS ;
- `RabbitMqJob` met en cache le payload, le `message_id` et `attempts`, acquitte ou libère la delivery une seule fois et abandonne le handle natif uniquement après une transition réussie ;
- les tests couvrent la remise immédiate par `basic.reject(requeue=true)`, la republication différée en millisecondes, la remontée d'une erreur d'ACK et la séquence Laravel ACK, callback `failed`, puis événement `JobFailed` ; `pop` reste réservé à la Task 20.

Checkpoint après branchement de la consommation multi-vhost Laravel du 1 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk composer validate --strict` dans `packages/laravel-queue` : PASS ;
- PHPUnit avec Laravel 13.23, Testbench 11 et PHPUnit 12 : PASS, 57 tests et 159 assertions ;
- PHPUnit avec Laravel 12.64, Testbench 10 et PHPUnit 11 : PASS, 57 tests et 159 assertions ;
- `rtk ./scripts/check.sh` : PASS ;
- `RabbitMqQueue::pop()` résout la valeur Laravel `queue` comme un profil worker, réutilise son consumer natif agrégé et délègue en un appel `next()` la sélection pondérée entre brokers et vhosts ;
- les subscriptions `enabled=false` sont exclues avant la création du pool, `block_for` est converti de secondes en millisecondes avec borne d'overflow, et l'alias natif de subscription restitue le vrai nom de queue au `RabbitMqJob` ;
- les tests couvrent deux vhosts, trois subscriptions actives, un profil inconnu, une subscription désactivée, un timeout sans job et la traduction des erreurs natives ; la sélection fine de plusieurs aliases reste réservée à `rabbit-rs:work` et les opérations d'administration à la Task 21.

Checkpoint après administration et monitoring du 15 août 2026 sur macOS ARM64 avec PHP 8.4.21 :

- `rtk cargo fmt --all -- --check` : PASS ; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings` : PASS ; `rtk cargo test --workspace --all-targets` : PASS, 153 tests Rust ;
- `rtk composer validate --strict` dans `packages/laravel-queue` : PASS ; PHPUnit (sans ext-rabbit_rs) : PASS, 65 tests et 172 assertions ;
- `queue_size` et `purge_queue` ajoutés au trait `TopologyChannel` avec implémentations Lapin (passive declare / queue_purge) et Mock ; `ClientPool::queue_size` et `ClientPool::purge_queue` exposent les opérations au niveau client ;
- `Pool::size()` et `Pool::clear()` ajoutés à l'extension native PHP et au stub ;
- `RabbitMqQueue::size()` et `RabbitMqQueue::clear()` résolvent la route configurée et délèguent au pool natif ; `pendingSize` délègue à `size`, `delayedSize` et `reservedSize` retournent 0, `creationTimeOfOldestPendingJob` retourne null (AMQP ne distingue pas ces états) ;
- les tests couvrent size par route et par défaut, clear par route et par défaut, size à zéro, échec native traduit en QueueException, et refus sans route configurée.

Checkpoint après le cluster RabbitMQ de test du 15 août 2026 sur macOS ARM64 (Colima/Docker) :

- `rtk ./scripts/check.sh` : PASS, 153 tests Rust et validation Composer ;
- cluster 3 nœuds RabbitMQ 4.2.9 (Alpine) avec peer discovery `rabbit_peer_discovery_classic_config`, Erlang cookie partagé, `cluster_partition_handling = pause_minority` et quorum queues opérationnelles ;
- plugin `rabbitmq_delayed_message_exchange` v4.2.0 (SHA-256 vérifié) pour le profil `with-plugin` ; profil `without-plugin` sans plugin pour tester le fallback TTL ;
- 2 vhosts (`/orders-eu`, `/billing`), utilisateur limité `rabbit_rs` (management) et admin `admin` (administrator) avec permissions restreintes ;
- Toxiproxy 2.12.0 intercepte les AMQP ports 5672–5674 pour l'injection de fautes ; Prometheus v3.5.0 scrape les 3 nœuds ;
- `./scripts/lab-up.sh` démarre le lab, `./scripts/lab-ready.sh` vérifie readiness (cluster, vhosts, quorum, permissions, Prometheus, Toxiproxy, plugin), `./scripts/lab-down.sh` arrête proprement ;
- toutes les images sont épinglées par digest SHA-256.

Checkpoint après les tests d'intégration end-to-end du 15 août 2026 sur macOS ARM64 (Colima/Docker) :

- `rtk cargo fmt --all -- --check` : PASS ; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings` : PASS ; `rtk cargo test --workspace --all-targets` : PASS, 153 tests Rust ;
- 8 tests d'intégration Rust via `cargo test -p rabbit-rs-core --features integration` : publish_confirm_then_consume_and_ack, release_zero_requeues_and_redispatches, two_vhosts_in_one_consumer_set, bulk_publish_then_consume_all, declare_quorum_queue_succeeds, declare_classic_queue_succeeds, verify_passive_does_not_create, external_mode_emits_no_commands ;
- tests d'intégration Laravel (QueueWorkerTest, DelayedJobTest) créés dans `tests/Integration/` avec testsuite dédiée ; skip automatique si ext-rabbit_rs n'est pas chargée ;
- `scripts/test-integration.sh` démarre le lab, attend readiness, exécute les tests Rust et Laravel, puis arrête le lab ;
- feature Cargo `integration` protège les tests Rust nécessitant un broker réel ; les tests déclarent les queues via `TopologyReconciler` avant publication ;
- permissions `rabbit_rs` mises à jour pour permettre la déclaration de queues de test (`^(amq\.|rabbit-rs-it-)`) ;
- phpunit.xml séparé en testsuites "Rabbit RS Laravel" et "Rabbit RS Integration" pour isoler les tests nécessitant un broker.

Checkpoint après correction des tests d'intégration Laravel du 16 août 2026 sur macOS ARM64 (Colima/Docker) :

- les tests `push()`, `later()` et `bulk()` passaient `null` comme queue, résolu en `"default"` par le connecteur, causant NO_ROUTE (AMQP 312) car aucune queue nommée `"default"` n'existait ; correction : passer le nom de queue unique explicitement ;
- `partitionJobsByAfterCommit` corrigé de `private` à `protected` pour la compatibilité Laravel 13 ;
- helpers `declareQueue()`/`deleteQueue()` ajoutés à `IntegrationTestCase` via l'API de management RabbitMQ ;
- `test_later_publishes_and_consumes_after_delay` marqué skipped car le `DelayRouter` n'est pas encore branché dans le chemin de publication (uniquement dans `release()` du consumer) ;
- `scripts/test-integration.sh` enrichi : build/install de ext-rabbit_rs, vérification du chargement, installe des dépendances composer ;
- résultat : 8 tests Rust + 7 tests Laravel (1 skipped) PASS, quality gate `./scripts/check.sh` PASS.

Le gate du Milestone A exécute `./scripts/check.sh` avec succès : formatage Rust, Clippy sans warning, 100 tests Rust et validation Composer. Le worktree est propre au commit `21aedee`.

Le checkpoint de la Task 13 vérifie 100 tests Rust et 2 tests PHPT, ainsi que le formatage Rust, Clippy sans warning, le lint du stub PHP et la validation Composer stricte.

Le checkpoint de la Task 14 vérifie 111 tests Rust et 7 tests PHPT, ainsi que le formatage Rust, Clippy sans warning et la validation Composer stricte. Les scénarios PHPT déterministes utilisent une feature Cargo de test et n'exposent aucune fixture dans le binaire distribué.

Le checkpoint de la Task 15 clôt le Milestone B avec 112 tests Rust, 9 tests PHPT et un laboratoire FPM à deux workers. Il vérifie la réutilisation des handles dans un processus, leur remplacement après fermeture, l'invalidation sans blocage après `pcntl_fork`, l'isolation des workers FPM et la fermeture du registre à l'arrêt du module.

## Arborescence cible

    Cargo.toml
    composer.json
    .gitattributes
    rust-toolchain.toml
    crates/
      rabbit-rs-core/
        Cargo.toml
        src/
          lib.rs
          config.rs
          error.rs
          runtime.rs
          transport.rs
          recovery.rs
          metrics.rs
          pool/
          topology/
          publisher/
          consumer/
        tests/
      rabbit-rs-php/
        Cargo.toml
        src/
          lib.rs
          classes/
        stubs/rabbit_rs.stub.php
        tests/phpt/
    packages/
      laravel-queue/
        composer.json
        config/rabbit-rs.php
        src/
          RabbitMqServiceProvider.php
          Config/
          Connectors/
          Exceptions/
          Jobs/
          Console/
          Support/
        tests/
    benchmarks/
      native/
      laravel/
    lab/
      rabbitmq/
    scripts/
    docs/

## Milestone A — Fondations et noyau Rust

### Task 1: Initialiser le workspace reproductible

**Files:**
- Create: Cargo.toml
- Create: Cargo.lock
- Create: composer.json
- Create: .gitattributes
- Create: rust-toolchain.toml
- Modify: .gitignore
- Create: crates/rabbit-rs-core/Cargo.toml
- Create: crates/rabbit-rs-core/src/lib.rs
- Create: crates/rabbit-rs-php/Cargo.toml
- Create: crates/rabbit-rs-php/src/lib.rs
- Create: scripts/check.sh

**Step 1: Write the failing workspace smoke check**

Créer scripts/check.sh avec :

    #!/usr/bin/env bash
    set -euo pipefail
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace --all-targets
    composer validate --strict

**Step 2: Run it to verify it fails**

Run: ./scripts/check.sh

Expected: FAIL parce que le workspace et les crates ne sont pas encore déclarés.

**Step 3: Add the minimal workspace**

Déclarer resolver = "2", les deux members et les dépendances partagées. Épingler une toolchain Rust stable connue dans rust-toolchain.toml. Le crate rabbit-rs-core doit compiler sans dépendance PHP. Le crate rabbit-rs-php doit être un cdylib dépendant du core.

Le composer.json racine représente le package PIE, pas le package Laravel :

    {
        "name": "goopil/rabbit-rs-native",
        "type": "php-ext",
        "description": "High-performance RabbitMQ transport for PHP and Laravel, powered by Rust",
        "license": "MIT",
        "require": {
            "php": "^8.4"
        },
        "php-ext": {
            "extension-name": "rabbit_rs",
            "priority": 80,
            "support-zts": true,
            "support-nts": true,
            "os-families": ["linux"],
            "download-url-method": ["pre-packaged-binary"]
        }
    }

.gitattributes exclut des archives Composer les benchmarks, le lab et les documents non requis par PIE.

**Step 4: Run the check**

Run: chmod +x scripts/check.sh && ./scripts/check.sh

Expected: PASS.

**Step 5: Commit**

    git add Cargo.toml Cargo.lock composer.json .gitattributes rust-toolchain.toml .gitignore crates scripts/check.sh
    git commit -m "build: bootstrap native RabbitMQ workspace"

### Task 2: Modéliser et valider la configuration native

**Files:**
- Create: crates/rabbit-rs-core/src/config.rs
- Create: crates/rabbit-rs-core/src/error.rs
- Modify: crates/rabbit-rs-core/src/lib.rs
- Test: crates/rabbit-rs-core/src/config.rs

**Step 1: Write failing tests**

Ajouter des tests pour :

- rejeter un broker sans hôte ;
- rejeter prefetch = 0 ;
- rejeter `scheduler.max_in_flight` inférieur à un prefetch ;
- rejeter une durée `starvation_after` nulle et appliquer 30 s par défaut ;
- rejeter un mode de topologie inconnu ;
- normaliser l'ordre des hôtes ;
- masquer les secrets dans Debug ;
- produire la même empreinte pour deux configurations équivalentes.

Structure publique minimale :

    pub struct BrokerConfig {
        pub name: String,
        pub hosts: Vec<Endpoint>,
        pub vhost: String,
        pub tls: TlsConfig,
        pub heartbeat: Duration,
    }

    pub struct WorkerProfile {
        pub subscriptions: Vec<SubscriptionConfig>,
        pub scheduler: SchedulerConfig,
    }

    pub struct SchedulerConfig {
        pub strategy: SchedulerStrategy,
        pub max_in_flight: u16,
    }

    pub struct SubscriptionConfig {
        pub starvation_after: Duration,
    }

    pub enum TopologyMode {
        Declare,
        Verify,
        External,
    }

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core config::tests

Expected: FAIL avec types ou fonctions absents.

**Step 3: Implement minimal validated types**

Utiliser serde pour l'entrée, secrecy pour les secrets et une représentation canonique sans secret pour l'empreinte. Retourner ConfigError avec un chemin de champ exploitable par Laravel.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core config::tests

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add validated connection and worker configuration"

### Task 3: Implémenter le scheduler multi-queue déterministe

**Files:**
- Create: crates/rabbit-rs-core/src/consumer/mod.rs
- Create: crates/rabbit-rs-core/src/consumer/scheduler.rs
- Create: crates/rabbit-rs-core/tests/scheduler_fairness.rs
- Modify: crates/rabbit-rs-core/src/lib.rs

**Step 1: Write failing scheduler tests**

Tester :

- une seule subscription ;
- deux subscriptions de poids 8 et 2 sur 10 000 choix ;
- une queue vide qui ne consomme pas son crédit ;
- le retour d'une queue précédemment vide ;
- priorité haute sans famine de la priorité basse ;
- résultat identique avec une horloge et une séquence identiques.

Interface :

    pub trait Scheduler {
        fn register(&mut self, id: SubscriptionId, policy: SubscriptionPolicy);
        fn mark_ready(&mut self, id: SubscriptionId);
        fn mark_empty(&mut self, id: SubscriptionId);
        fn next(&mut self, now: Instant) -> Option<SubscriptionId>;
    }

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test scheduler_fairness

Expected: FAIL.

**Step 3: Implement deficit weighted round-robin**

Séparer priority_class et weight. Ajouter un aging borné pour qu'une classe basse prête finisse par être choisie. Ne pas ajouter de prefetch adaptatif.

**Step 4: Verify distribution**

Run: cargo test -p rabbit-rs-core --test scheduler_fairness

Expected: PASS avec erreur de distribution sous la tolérance définie dans le test.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add starvation-safe weighted scheduler"

### Task 4: Rendre le runtime sûr après fork

**Files:**
- Create: crates/rabbit-rs-core/src/runtime.rs
- Create: crates/rabbit-rs-core/src/pool/mod.rs
- Create: crates/rabbit-rs-core/src/pool/key.rs
- Modify: crates/rabbit-rs-core/src/lib.rs
- Test: crates/rabbit-rs-core/src/runtime.rs

**Step 1: Write failing lifecycle tests**

Injecter un PidProvider de test et vérifier :

- création paresseuse ;
- réutilisation dans le même PID ;
- invalidation de tous les handles après changement de PID ;
- une configuration différente ne partage pas le pool ;
- close est idempotent.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core runtime::tests pool::tests

Expected: FAIL.

**Step 3: Implement RuntimeRegistry**

    pub struct RuntimeRegistry {
        pid: u32,
        runtime: tokio::runtime::Runtime,
        pools: HashMap<ConnectionKey, Arc<ConnectionHandle>>,
    }

Le runtime ne doit être créé ni dans une statique globale initialisée au chargement, ni avant la première acquisition après fork. Utiliser OnceLock uniquement pour le verrou du registre, pas pour une socket ou un runtime hérité.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core runtime::tests pool::tests

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add fork-safe per-process runtime registry"

### Task 5: Isoler Lapin derrière un transport testable

**Files:**
- Create: crates/rabbit-rs-core/src/transport.rs
- Create: crates/rabbit-rs-core/src/transport/lapin.rs
- Create: crates/rabbit-rs-core/src/transport/mock.rs
- Modify: crates/rabbit-rs-core/Cargo.toml
- Modify: crates/rabbit-rs-core/src/lib.rs

**Step 1: Write a compile-failing contract test**

Définir les capacités minimales :

    #[async_trait]
    pub trait Transport: Send + Sync {
        async fn connect(&self, config: &BrokerConfig) -> Result<Box<dyn TransportConnection>>;
    }

    #[async_trait]
    pub trait TransportConnection: Send + Sync {
        async fn open_publisher(&self) -> Result<Box<dyn PublisherChannel>>;
        async fn open_consumer(&self) -> Result<Box<dyn ConsumerChannel>>;
        async fn close(&self) -> Result<()>;
    }

Les traits de channels doivent couvrir declare, passive verify, bind, publish, confirm, return, qos, consume, ack et reject.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core transport

Expected: FAIL.

**Step 3: Implement MockTransport then LapinTransport**

Commencer par le mock scriptable. Adapter ensuite Lapin sans exposer ses types hors du module transport/lapin.rs.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core transport

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): abstract AMQP transport behind testable traits"

### Task 6: Construire la machine de connexion et de recovery

**Files:**
- Create: crates/rabbit-rs-core/src/recovery.rs
- Create: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Create: crates/rabbit-rs-core/tests/recovery_state_machine.rs
- Modify: crates/rabbit-rs-core/src/pool/mod.rs

**Step 1: Write failing state-machine tests**

Avec le temps Tokio suspendu, vérifier :

- Disconnected vers Connecting puis Ready ;
- backoff 100 ms, 200 ms, 400 ms avec jitter injecté ;
- plafond 30 s ;
- erreur d'authentification permanente ;
- perte de connexion Ready vers Recovering ;
- fermeture pendant le backoff ;
- génération incrémentée après recovery.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test recovery_state_machine

Expected: FAIL.

**Step 3: Implement ConnectionActor**

Toutes les opérations passent par un canal mpsc borné. Les états et raisons sont publiés via watch. Le générateur de jitter et l'horloge sont injectables.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test recovery_state_machine

Expected: PASS sans attente réelle.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add deterministic connection recovery actor"

### Task 7: Déclarer ou vérifier la topologie

**Files:**
- Create: crates/rabbit-rs-core/src/topology/mod.rs
- Create: crates/rabbit-rs-core/src/topology/plan.rs
- Create: crates/rabbit-rs-core/src/topology/reconciler.rs
- Create: crates/rabbit-rs-core/tests/topology_recovery.rs

**Step 1: Write failing topology tests**

Vérifier :

- ordre exchange, queue, binding ;
- quorum durable par défaut ;
- classic explicite ;
- declare idempotent ;
- verify passif sans création ;
- external sans commande de déclaration ;
- aucune DLQ applicative dans la configuration par défaut ;
- DLX, DLQ et bindings déclarés seulement après activation explicite ;
- incompatibilité remontée comme erreur permanente ;
- replay complet après nouvelle génération.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test topology_recovery

Expected: FAIL.

**Step 3: Implement TopologyPlan and Reconciler**

Compiler la configuration en plan immuable avant toute I/O. Refuser les combinaisons quorum exclusive ou auto_delete. Ne pas tenter de créer des policies RabbitMQ.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test topology_recovery

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add declarative and externally managed topology modes"

### Task 8: Implémenter batching, confirms et mandatory returns

**Files:**
- Create: crates/rabbit-rs-core/src/publisher/mod.rs
- Create: crates/rabbit-rs-core/src/publisher/batcher.rs
- Create: crates/rabbit-rs-core/src/publisher/confirms.rs
- Create: crates/rabbit-rs-core/src/publisher/actor.rs
- Create: crates/rabbit-rs-core/tests/publisher_safety.rs

**Step 1: Write failing publisher tests**

Tester :

- flush à max_messages ;
- flush à max_bytes ;
- flush au timer ;
- ACK de plusieurs séquences ;
- NACK ciblé ;
- basic.return avant ACK ;
- timeout ;
- buffer plein retourne Backpressure ;
- coupure avant confirm classe la séquence Ambiguous dans le ledger interne ;
- message_id conservé lors d'une republication.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test publisher_safety

Expected: FAIL.

**Step 3: Implement the bounded publisher actor**

    pub struct PublishRequest {
        pub destination: Destination,
        pub payload: Bytes,
        pub properties: MessageProperties,
        pub deadline: Instant,
    }

    pub enum PublishOutcome {
        Confirmed { message_id: String },
        Returned { message_id: String, reply: ReturnInfo },
        Ambiguous { message_id: String },
    }

L'acteur possède le ledger de séquences. Il ne résout pas un ACK routé avant d'avoir traité le flux basic.return correspondant.

À ce stade, Ambiguous est un état interne. La tâche 9 bis remplace sa résolution immédiate par une rétention bornée et un replay automatique après recovery.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test publisher_safety

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add bounded batched publisher confirms"

### Task 9: Ajouter les délais plugin et TTL

**Files:**
- Create: crates/rabbit-rs-core/src/topology/delay.rs
- Create: crates/rabbit-rs-core/src/publisher/delay.rs
- Create: crates/rabbit-rs-core/tests/delay_routing.rs
- Modify: crates/rabbit-rs-core/src/config.rs

**Step 1: Write failing delay tests**

Tester :

- auto choisit x-delayed-message si disponible ;
- auto retombe sur TTL si le plugin est absent ;
- plugin obligatoire échoue sans plugin ;
- TTL arrondit au bucket supérieur ;
- nombre maximal de buckets ;
- nom stable de queue TTL ;
- x-expires supérieur au TTL ;
- délai négatif rejeté.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test delay_routing

Expected: FAIL.

**Step 3: Implement DelayStrategy**

    pub enum DelayStrategy {
        Plugin,
        TtlBuckets(TtlBucketPlan),
    }

La détection du plugin doit être bornée dans le temps et mise en cache par génération de connexion.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test delay_routing

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add delayed exchange and TTL fallback"

### Task 9 bis: Rejouer les publications après reconnexion

**Files:**
- Modify: crates/rabbit-rs-core/src/publisher/mod.rs
- Modify: crates/rabbit-rs-core/src/publisher/actor.rs
- Modify: crates/rabbit-rs-core/src/publisher/confirms.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Create: crates/rabbit-rs-core/tests/publisher_recovery.rs

**Step 1: Write failing publisher recovery tests**

Vérifier :

- une publication acceptée pendant Recovering reste suspendue et part après Ready ;
- un message encore dans le batch au moment de la coupure est conservé ;
- une publication envoyée sans confirm est classée Ambiguous, replacée dans le buffer et automatiquement republiée ;
- la republication conserve exactement le message_id, la destination, les propriétés, le payload en Bytes et la deadline originale ;
- le nouveau channel active publisher confirms avant tout replay ;
- le replay ne commence qu'après la restauration de topologie pour la nouvelle génération ;
- un confirm tardif provenant de l'ancienne génération est ignoré et l'attente n'est résolue qu'une fois ;
- plusieurs coupures successives ne dupliquent pas une entrée dans le ledger de replay ;
- ACK, NACK et basic.return restent terminaux après replay ;
- l'expiration de la deadline pendant la coupure retourne Timeout sans publier après Ready ;
- une erreur permanente de reconnexion termine toutes les attentes concernées sans boucle de retry ;
- la capacité globale couvre commandes, batches, replay et confirms en vol ; lorsqu'elle est atteinte, try_publish retourne Backpressure même si l'acteur continue de drainer son canal mpsc ;
- la fermeture explicite réveille toutes les attentes avec une erreur typée ;
- aucun test ne promet un replay après crash du processus, le buffer étant volontairement mémoire-only.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test publisher_recovery

Expected: FAIL parce que connection_lost résout actuellement les séquences Ambiguous et détruit le batch.

**Step 3: Implement the suspended publisher lifecycle**

Ajouter au publisher les phases Ready et Suspended. Le coordinateur de connexion transmet uniquement des événements bornés et ordonnés :

    enum PublisherConnectionEvent {
        Recovering { generation: u64 },
        Ready {
            generation: u64,
            channel: Arc<dyn PublisherChannel>,
            topology_restored: bool,
        },
        FailedPermanent { generation: u64, error: TransportError },
    }

Au passage vers Recovering, annuler les futures de confirm de l'ancienne génération, retirer leurs entrées du ledger actif et replacer les PublishRequest complets dans une deque de replay. Ne pas résoudre les waiters Ambiguous. Les payloads restent des Bytes afin que cette transition ne copie pas leur contenu.

Le ledger doit conserver pour chaque publication la requête originale, son waiter, sa deadline absolue, sa génération d'envoi et un identifiant interne unique. Une entrée ne peut exister qu'une fois entre batch, replay et confirms en vol. Une nouvelle séquence AMQP est attribuée à chaque republication, sans modifier message_id.

PublisherHandle acquiert par try_acquire_owned un permit d'un Semaphore dimensionné à la capacité globale avant d'accepter la commande. Le permit suit l'entrée jusqu'à son état terminal ; le simple drainage du mpsc ne libère donc pas de capacité pendant une coupure.

Lors de Ready, rejeter les générations anciennes ou identiques, vérifier topology_restored, activer confirm_select sur le nouveau channel, puis rejouer d'abord la deque existante avant les nouvelles commandes. La deadline originale est vérifiée avant chaque tentative et utilisée pour le timeout de confirm. Une erreur recoverable replace l'entrée une seule fois en replay ; NACK, return, timeout, erreur permanente et fermeture sont terminaux.

Le ConnectionActor reste seul responsable du backoff et de l'ouverture réseau. Il ne republie rien lui-même ; après la réconciliation de topologie, le coordinateur lui fournit le nouveau PublisherChannel et la génération au PublisherActor.

**Step 4: Verify targeted behavior**

Run: cargo test -p rabbit-rs-core --test publisher_recovery

Expected: PASS sans attente réelle grâce au temps Tokio suspendu.

**Step 5: Verify publisher regressions**

Run: cargo test -p rabbit-rs-core --test publisher_safety --test publisher_recovery

Expected: PASS. Adapter publisher_safety pour considérer Ambiguous comme un état interne rejoué, et non comme un résultat utilisateur immédiat.

**Step 6: Commit**

    git add crates/rabbit-rs-core docs/plans
    git commit -m "feat(core): replay publishes after connection recovery"

### Task 10: Implémenter ConsumerSet et les jetons de delivery

**Files:**
- Create: crates/rabbit-rs-core/src/consumer/set.rs
- Create: crates/rabbit-rs-core/src/consumer/delivery.rs
- Create: crates/rabbit-rs-core/src/consumer/actor.rs
- Create: crates/rabbit-rs-core/tests/consumer_semantics.rs

**Step 1: Write failing consumer tests**

Tester :

- plusieurs subscriptions sur deux connexions ;
- scheduler choisissant le prochain buffer prêt ;
- budget global max_in_flight ;
- prefetch par subscription ;
- ACK sur bonne génération ;
- rejet d'un ACK ancien ;
- release(0) appelle basic.reject avec requeue=true ;
- release différé publie, confirme, puis ACK ;
- échec de publication différée n'ACK pas ;
- fermeture réveille next avec une erreur typée.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test consumer_semantics

Expected: FAIL.

**Step 3: Implement ConsumerSet**

    pub struct Delivery {
        pub id: MessageId,
        pub subscription: SubscriptionId,
        pub payload: Bytes,
        pub headers: Headers,
        pub attempts: u32,
        token: DeliveryToken,
    }

Le token contient connection key, génération, channel id et delivery tag. Ses transitions Pending, Acked, Rejected et Lost sont atomiques et terminales.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test consumer_semantics

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add multiplexed consumers and safe delivery tokens"

### Task 11: Ajouter les compteurs attempts et poison-message

**Files:**
- Create: crates/rabbit-rs-core/src/consumer/attempts.rs
- Create: crates/rabbit-rs-core/tests/delivery_attempts.rs
- Modify: crates/rabbit-rs-core/src/consumer/delivery.rs

**Step 1: Write failing attempts tests**

Cas :

- première acquisition = 1 ;
- x-acquired-count prioritaire sur redelivered bool ;
- x-delivery-count lu pour les échecs quorum ;
- release différé incrémente le compteur applicatif ;
- limite atteinte produit MaxAttempts ;
- classic sans compteur utilise le fallback documenté.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test delivery_attempts

Expected: FAIL.

**Step 3: Implement AttemptsResolver**

Centraliser toute interprétation des headers. Ne pas disperser les règles RabbitMQ dans la couche PHP.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test delivery_attempts

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): preserve Laravel-compatible delivery attempts"

### Task 12: Exposer un snapshot de métriques sans backend

**Files:**
- Create: crates/rabbit-rs-core/src/metrics.rs
- Create: crates/rabbit-rs-core/tests/metrics_snapshot.rs
- Modify: crates/rabbit-rs-core/src/publisher/actor.rs
- Modify: crates/rabbit-rs-core/src/consumer/actor.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs

**Step 1: Write failing metric tests**

Vérifier que publish, confirm, return, delivery, ACK, reject, reconnect et backpressure mettent à jour les bons compteurs. Vérifier qu'un snapshot ne bloque pas les acteurs et ne contient aucun secret.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test metrics_snapshot

Expected: FAIL.

**Step 3: Implement atomics and histograms**

Garder une API de snapshot sérialisable. Ne dépendre ni de Prometheus ni d'OpenTelemetry dans rabbit-rs-core.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test metrics_snapshot

Expected: PASS.

**Step 5: Run Milestone A gate**

Run: ./scripts/check.sh

Expected: PASS.

**Step 6: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): expose transport metrics snapshots"

## Milestone B — Extension PHP

### Task 13: Définir l'API et les stubs PHP

**Files:**
- Create: crates/rabbit-rs-php/src/classes/mod.rs
- Create: crates/rabbit-rs-php/src/classes/pool.rs
- Create: crates/rabbit-rs-php/src/classes/consumer.rs
- Create: crates/rabbit-rs-php/src/classes/delivery.rs
- Create: crates/rabbit-rs-php/src/classes/exception.rs
- Create: crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
- Modify: crates/rabbit-rs-php/src/lib.rs
- Create: scripts/test-extension.sh

**Step 1: Write failing reflection tests**

Créer PHPT vérifiant l'existence de :

    Goopil\RabbitRs\Pool
    Goopil\RabbitRs\Consumer
    Goopil\RabbitRs\Delivery
    Goopil\RabbitRs\Exception
    Goopil\RabbitRs\BackpressureException
    Goopil\RabbitRs\ConnectionException

Vérifier aussi que extension_loaded('rabbit_rs') est vrai et que phpversion('rabbit_rs') correspond à la version Cargo et au tag de release.

API minimale :

    final class Pool {
        public function __construct(array $config);
        public function publish(array $message): string;
        public function publishBatch(array $messages): array;
        public function consumer(string $profile): Consumer;
        public function stats(): array;
        public function close(): void;
    }

    final class Consumer {
        public function next(int $timeoutMs): ?Delivery;
        public function close(): void;
    }

    final class Delivery {
        public function payload(): string;
        public function metadata(): array;
        public function ack(): void;
        public function release(int $delayMs = 0): void;
        public function reject(bool $requeue = false): void;
    }

**Step 2: Verify failure**

Run: cargo build -p rabbit-rs-php --release && ./scripts/test-extension.sh reflection

Expected: FAIL.

**Step 3: Implement thin ext-php-rs classes**

À ce checkpoint, les trois classes opérationnelles sont volontairement sans état et toutes leurs opérations échouent avec l'exception de base stable. La Task 14 introduira les handles natifs validés. Ne pas exposer Lapin.

`ext-php-rs` 0.15.15 conserve tels quels les identifiants de paramètres Rust dans les arguments nommés PHP. Les méthodes frontières gardent donc les noms contractuels PHP, y compris `timeoutMs` et `delayMs`, puis consomment explicitement leurs paramètres inutilisés avant l'initialisation des handles natifs.

**Step 4: Verify**

Run: cargo build -p rabbit-rs-php --release && ./scripts/test-extension.sh reflection

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-php scripts/test-extension.sh
    git commit -m "feat(extension): expose native pool publisher and consumer API"

### Task 14: Tester conversions, erreurs et transitions PHP

**Files:**
- Create: crates/rabbit-rs-php/tests/phpt/config_validation.phpt
- Create: crates/rabbit-rs-php/tests/phpt/binary_payload.phpt
- Create: crates/rabbit-rs-php/tests/phpt/delivery_terminal_state.phpt
- Create: crates/rabbit-rs-php/tests/phpt/secrets.phpt
- Create: crates/rabbit-rs-php/tests/phpt/backpressure.phpt

**Step 1: Add failing PHPT cases**

Inclure payload binaire avec octets nuls, headers imbriqués autorisés, taille maximale, configuration invalide, double ACK, opération après close et message d'erreur expurgé.

**Step 2: Verify failure**

Run: ./scripts/test-extension.sh

Expected: FAIL sur les cas non implémentés.

**Step 3: Implement converters and guards**

Définir une liste exacte des types AMQP supportés. Refuser les ressources, objets arbitraires et structures récursives.

**Step 4: Verify**

Run: ./scripts/test-extension.sh

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-php
    git commit -m "test(extension): harden PHP value conversion and handle states"

### Task 15: Certifier le cycle de vie CLI, fork et FPM

**Files:**
- Create: crates/rabbit-rs-php/tests/phpt/pid_registry.phpt
- Create: crates/rabbit-rs-php/tests/phpt/fork_invalidation.phpt
- Create: crates/rabbit-rs-php/tests/fixtures/fpm/index.php
- Create: crates/rabbit-rs-php/tests/fixtures/fpm/php-fpm.conf
- Create: scripts/test-fpm.sh
- Modify: crates/rabbit-rs-php/src/classes/pool.rs

**Step 1: Write failing process lifecycle tests**

Vérifier :

- deux Pool équivalents dans un PID partagent le connection key ;
- pcntl_fork invalide le handle hérité dans l'enfant ;
- l'enfant crée un nouveau registre ;
- deux requêtes FPM du même worker réutilisent le pool ;
- deux workers FPM n'annoncent pas le même PID ou handle.

**Step 2: Verify failure**

Run: ./scripts/test-fpm.sh

Expected: FAIL avant instrumentation et garde PID.

**Step 3: Implement lifecycle hooks**

Ajouter uniquement les hooks nécessaires à l'arrêt de module/processus. Ne jamais ouvrir de connexion dans MINIT.

**Step 4: Verify**

Run: ./scripts/test-fpm.sh

Expected: PASS.

**Step 5: Run Milestone B gate**

Run: ./scripts/check.sh && ./scripts/test-extension.sh && ./scripts/test-fpm.sh

Expected: PASS.

**Step 6: Commit**

    git add crates/rabbit-rs-php scripts
    git commit -m "feat(extension): make native pools safe across PHP process lifecycles"

## Milestone C — Package Laravel

### Task 16: Initialiser le package et sa configuration

**Files:**
- Create: packages/laravel-queue/composer.json
- Create: packages/laravel-queue/phpunit.xml
- Create: packages/laravel-queue/src/RabbitMqServiceProvider.php
- Create: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Create: packages/laravel-queue/config/rabbit-rs.php
- Create: packages/laravel-queue/tests/TestCase.php
- Create: packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php

**Step 1: Write failing package tests**

Tester la publication de configuration, la validation des brokers/routes/workers, les defaults quorum/confirm/mandatory, l'absence de DLQ applicative par défaut, le masquage des secrets et l'erreur si ext-rabbit_rs manque.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && composer install && vendor/bin/phpunit --filter ConfigNormalizerTest

Expected: FAIL.

**Step 3: Implement package skeleton**

Utiliser le namespace Goopil\RabbitRs\Laravel, illuminate/queue et Orchestra Testbench avec une matrice Composer Laravel 12/13. Le package porte exactement le nom goopil/rabbit-rs-laravel et exige PHP ^8.4, ext-rabbit_rs avec la même version majeure, et illuminate/queue ^12.0 || ^13.0.

Le composer.json du package contient au minimum :

    {
        "name": "goopil/rabbit-rs-laravel",
        "type": "library",
        "require": {
            "php": "^8.4",
            "ext-rabbit_rs": "^1.0",
            "illuminate/queue": "^12.0 || ^13.0"
        },
        "autoload": {
            "psr-4": {
                "Goopil\\RabbitRs\\Laravel\\": "src/"
            }
        }
    }

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter ConfigNormalizerTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): bootstrap native RabbitMQ queue package"

### Task 17: Enregistrer le connecteur et le pool partagé

**Files:**
- Create: packages/laravel-queue/src/Connectors/RabbitMqConnector.php
- Create: packages/laravel-queue/src/Support/NativePoolFactory.php
- Create: packages/laravel-queue/src/RabbitMqQueue.php
- Create: packages/laravel-queue/tests/Unit/RabbitMqConnectorTest.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing connector tests**

Vérifier Queue::connection retourne le driver, deux résolutions équivalentes partagent le handle de pool, une empreinte différente crée un autre handle et aucune Request n'est retenue.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqConnectorTest

Expected: FAIL.

**Step 3: Implement connector and factory**

Enregistrer le nom rabbit-rs. Le factory transmet une configuration normalisée immuable à Goopil\RabbitRs\Pool. Créer le squelette contractuel de RabbitMqQueue afin que Laravel puisse appliquer setConnectionName et setContainer ; laisser ses opérations non implémentées jusqu'à la Task 18.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqConnectorTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): register native RabbitMQ queue connector"

### Task 18: Implémenter push, later et bulk

**Files:**
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Create: packages/laravel-queue/src/Support/MessageMapper.php
- Create: packages/laravel-queue/src/Exceptions/QueueException.php
- Create: packages/laravel-queue/tests/Unit/RabbitMqQueuePublishTest.php
- Create: packages/laravel-queue/tests/bootstrap.php
- Modify: packages/laravel-queue/src/Connectors/RabbitMqConnector.php
- Modify: packages/laravel-queue/tests/Unit/RabbitMqConnectorTest.php
- Modify: packages/laravel-queue/phpunit.xml

**Step 1: Write failing Queue publish tests**

Tester :

- push sérialise le payload Laravel ;
- pushRaw conserve le payload ;
- message_id UUID stable ;
- onQueue alimente le routing key ;
- later passe le délai en millisecondes ;
- bulk appelle publishBatch une seule fois ;
- basic.return devient QueueException ;
- backpressure devient une exception dédiée ;
- dispatch_after_commit reste géré par la classe Queue de Laravel.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueuePublishTest

Expected: FAIL.

**Step 3: Implement minimal publishing adapter**

Étendre Illuminate\Queue\Queue et implémenter Illuminate\Contracts\Queue\Queue. Ne pas dupliquer createPayload. Résoudre la route nommée avec fallback `default`, réutiliser l'UUID du payload Laravel comme `message_id` et conserver les événements, délais et callbacks transactionnels de Laravel autour des appels natifs simples ou batchés.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueuePublishTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): publish immediate delayed and bulk jobs"

### Task 19: Implémenter RabbitMqJob

**Files:**
- Create: packages/laravel-queue/src/Jobs/RabbitMqJob.php
- Create: packages/laravel-queue/tests/Unit/RabbitMqJobTest.php
- Modify: packages/laravel-queue/src/RabbitMqQueue.php

**Step 1: Write failing job tests**

Tester :

- getRawBody ;
- getJobId ;
- attempts ;
- delete appelle ACK une fois ;
- release(0) appelle basic.reject avec requeue=true via le handle natif ;
- release(10) appelle release(10000) ;
- double delete sans effet dangereux ;
- exception d'ACK remonte comme connexion perdue ;
- failed job suit la séquence Laravel.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqJobTest

Expected: FAIL.

**Step 3: Implement Job adapter**

Étendre Illuminate\Queue\Jobs\Job. Garder Delivery privé et libérer son handle après transition terminale.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqJobTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): map native deliveries to Laravel jobs"

### Task 20: Brancher pop sur un profil multi-vhost

**Files:**
- Create: packages/laravel-queue/src/Support/WorkerProfileResolver.php
- Create: packages/laravel-queue/tests/Feature/MultiVhostWorkerTest.php
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Modify: packages/laravel-queue/config/rabbit-rs.php

**Step 1: Write failing feature test**

Configurer deux brokers/vhosts et trois subscriptions. Vérifier qu'un seul appel pop sur le profil main peut rendre des jobs des trois sources, avec bons connectionName, queue et attempts.

Tester aussi un profil inconnu, une subscription désactivée et un timeout sans job.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter MultiVhostWorkerTest

Expected: FAIL.

**Step 3: Implement aggregate pop**

La valeur queue de la connexion Laravel référence par défaut le nom du profil worker. Documenter que la sélection fine de plusieurs aliases par option --queue arrive avec rabbit-rs:work ; ne pas simuler une boucle bloquante queue par queue.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter MultiVhostWorkerTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): consume multi-vhost worker profiles"

### Task 21: Implémenter size, clear et monitoring

**Files:**
- Create: packages/laravel-queue/tests/Unit/RabbitMqQueueAdminTest.php
- Modify: packages/laravel-queue/src/RabbitMqQueue.php

**Step 1: Write failing admin tests**

Vérifier size agrégé et par route, clear explicite, refus de clear sans permission de configuration, et métriques Monitor.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueueAdminTest

Expected: FAIL.

**Step 3: Implement bounded admin operations**

Ne pas utiliser l'API HTTP management pour le chemin critique. Les commandes AMQP passives suffisent pour size lorsque disponibles.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueueAdminTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): add queue administration and monitoring"

### Task 22: Ajouter événements natifs et commande de diagnostic

**Files:**
- Create: packages/laravel-queue/src/Events/ConnectionStateChanged.php
- Create: packages/laravel-queue/src/Events/BackpressureDetected.php
- Create: packages/laravel-queue/src/Console/RabbitMqStatusCommand.php
- Create: packages/laravel-queue/tests/Feature/RabbitMqStatusCommandTest.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing command tests**

Vérifier sortie humaine et JSON, absence de secrets, états par broker/vhost, buffers, confirms et génération.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqStatusCommandTest

Expected: FAIL.

**Step 3: Implement status adapter**

La commande rabbit-rs:status lit seulement Pool::stats. Elle ne doit ni reconnecter ni modifier la topologie sauf option explicite future.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqStatusCommandTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): expose native connection diagnostics"

### Task 23: Ajouter la commande multiprocessus progressive

**Files:**
- Create: packages/laravel-queue/src/Console/RabbitMqWorkCommand.php
- Create: packages/laravel-queue/src/Console/WorkerSupervisor.php
- Create: packages/laravel-queue/tests/Unit/WorkerSupervisorTest.php
- Create: packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing supervisor tests**

Tester construction de la commande enfant, workers = 1 et workers > 1, propagation SIGTERM/SIGINT, redémarrage avec backoff, arrêt propre, max restarts et codes de sortie.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter 'WorkerSupervisorTest|RabbitMqWorkCommandTest'

Expected: FAIL.

**Step 3: Implement orchestration only**

Chaque enfant exécute queue:work avec une connexion/profil déterminé. Utiliser Symfony Process. Ne pas appeler des handlers de job depuis le superviseur.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter 'WorkerSupervisorTest|RabbitMqWorkCommandTest'

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): supervise multiple standard queue workers"

### Task 24: Certifier Octane

**Files:**
- Create: packages/laravel-queue/src/Octane/OctaneLifecycle.php
- Create: packages/laravel-queue/tests/Feature/OctaneLifecycleTest.php
- Create: scripts/test-octane.sh
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing lifecycle tests**

Vérifier :

- aucune Request conservée ;
- deux requêtes réutilisent le même pool dans un worker ;
- reload ferme le pool ;
- worker stop draine dans la deadline ;
- requête annulée ne laisse pas une attente PHP orpheline ;
- pool indépendant par worker.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter OctaneLifecycleTest

Expected: FAIL.

**Step 3: Implement Octane hooks**

Détecter Octane de manière optionnelle. Ne pas rendre laravel/octane obligatoire pour les utilisateurs FPM.

**Step 4: Verify package tests**

Run: cd packages/laravel-queue && vendor/bin/phpunit

Expected: PASS.

**Step 5: Run Milestone C gate**

Run: ./scripts/test-octane.sh --server=frankenphp && ./scripts/test-octane.sh --server=roadrunner && ./scripts/test-octane.sh --server=swoole && ./scripts/test-octane.sh --server=openswoole

Expected: PASS pour chaque runtime certifié.

**Step 6: Commit**

    git add packages/laravel-queue scripts/test-octane.sh
    git commit -m "feat(laravel): support persistent Octane worker lifecycles"

## Milestone D — Cluster, intégration et chaos

### Task 25: Créer le cluster RabbitMQ de test

**Files:**
- Create: lab/rabbitmq/compose.yaml
- Create: lab/rabbitmq/rabbitmq/Dockerfile
- Create: lab/rabbitmq/rabbitmq/enabled_plugins
- Create: lab/rabbitmq/rabbitmq/rabbitmq.conf
- Create: lab/rabbitmq/rabbitmq/definitions.json
- Create: lab/rabbitmq/toxiproxy/config.json
- Create: lab/rabbitmq/prometheus/prometheus.yml
- Create: scripts/lab-up.sh
- Create: scripts/lab-down.sh
- Create: scripts/lab-ready.sh

**Step 1: Write failing readiness check**

Le script doit vérifier trois nœuds RabbitMQ 4.3, cluster sain, quorum leader, deux vhosts, utilisateurs limités, Prometheus et Toxiproxy.

**Step 2: Verify failure**

Run: ./scripts/lab-up.sh && ./scripts/lab-ready.sh

Expected: FAIL avant les services.

**Step 3: Implement the lab**

Épingler les images par version et digest lors de l'implémentation. Le Dockerfile télécharge une version épinglée de rabbitmq_delayed_message_exchange et vérifie son SHA-256. Fournir un profile Compose avec le plugin et un sans plugin.

**Step 4: Verify**

Run: ./scripts/lab-up.sh && ./scripts/lab-ready.sh

Expected: PASS.

**Step 5: Commit**

    git add lab scripts
    git commit -m "test: add reproducible three-node RabbitMQ lab"

### Task 26: Écrire les tests d'intégration end-to-end

**Files:**
- Create: crates/rabbit-rs-core/tests/integration/publish_consume.rs
- Create: crates/rabbit-rs-core/tests/integration/topology_modes.rs
- Create: packages/laravel-queue/tests/Integration/QueueWorkerTest.php
- Create: packages/laravel-queue/tests/Integration/DelayedJobTest.php
- Create: scripts/test-integration.sh

**Step 1: Write failing scenarios**

Inclure :

- publish confirmé puis consume/ACK ;
- mandatory return ;
- quorum et classic ;
- declare, verify et external ;
- deux vhosts dans un ConsumerSet ;
- release(0) par reject/requeue ;
- release différé plugin ;
- release différé TTL ;
- failed job Laravel ;
- bulk ;
- TLS.

**Step 2: Verify failure**

Run: ./scripts/test-integration.sh

Expected: FAIL avant branchement réel.

**Step 3: Complete real transport integration**

Remplacer uniquement les chemins encore mockés. Garder les assertions at-least-once explicites.

**Step 4: Verify**

Run: ./scripts/test-integration.sh

Expected: PASS.

**Step 5: Commit**

    git add crates packages scripts
    git commit -m "test: validate native Laravel queue flows against RabbitMQ"

### Task 27: Écrire les scénarios de panne

**Files:**
- Create: lab/rabbitmq/scenarios/
- Create: crates/rabbit-rs-core/tests/chaos/reconnect.rs
- Create: packages/laravel-queue/tests/Integration/AtLeastOnceChaosTest.php
- Create: scripts/test-chaos.sh

**Step 1: Write failing chaos assertions**

Scénarios :

- reset TCP avant confirm ;
- reset TCP après confirm avant ACK ;
- arrêt du leader quorum ;
- redémarrage d'un nœud ;
- partition du consumer ;
- channel fermé pour erreur de topologie ;
- plugin delay indisponible ;
- credentials refusés ;
- SIGTERM du worker avec jobs non acquittés.

Pour chaque scénario, compter messages attendus, uniques, doublons et manquants.

**Step 2: Verify failure**

Run: ./scripts/test-chaos.sh

Expected: FAIL jusqu'à implémentation complète du recovery.

**Step 3: Fix one scenario at a time**

Appliquer systematic-debugging. Ne jamais accepter un message manquant. Accepter les doublons uniquement dans les fenêtres ambiguës documentées.

**Step 4: Verify**

Run: ./scripts/test-chaos.sh

Expected: PASS, missing = 0.

**Step 5: Commit**

    git add lab crates packages scripts
    git commit -m "test: prove at-least-once behavior under RabbitMQ failures"

## Milestone E — Performance

### Task 38: Créer bench-native

**Files:**
- Create: benchmarks/native/Cargo.toml
- Create: benchmarks/native/benches/batching.rs
- Create: benchmarks/native/benches/ffi_conversion.rs
- Create: benchmarks/native/benches/scheduler.rs
- Create: benchmarks/native/benches/transport.rs
- Create: benchmarks/native/php/ffi_conversion.php
- Create: benchmarks/native/README.md
- Modify: Cargo.toml

**Step 1: Add benchmark smoke tests**

Les benchmarks doivent couvrir tailles 256 o, 1 Kio, 10 Kio, 100 Kio et 1 Mio, batch 1/16/64/256, confirms, coût scheduler et allocation. La baseline issue de l'audit mesure séparément le coût d'un appel à la frontière PHP/Rust, la conversion et la copie des payloads et headers, ainsi que la soumission des batches, sans broker lorsque celui-ci n'est pas nécessaire.

**Step 2: Verify command**

Run: cargo bench -p rabbit-rs-native-bench --no-run

Expected: FAIL avant le crate benchmark.

**Step 3: Implement Criterion suites**

Séparer microbench sans broker et bench transport avec le lab. Le harness PHP exerce l'extension compilée et distingue le coût fixe de la frontière FFI du coût de conversion selon le volume et la taille du batch. Enregistrer version, CPU, noyau, PHP, mode NTS/ZTS, RabbitMQ, payload et configuration dans chaque résultat.

**Step 4: Verify**

Run: cargo bench -p rabbit-rs-native-bench --no-run

Expected: PASS.

**Step 5: Commit**

    git add Cargo.toml benchmarks/native
    git commit -m "perf: add native batching and transport benchmarks"

### Task 39: Créer l'application bench-laravel

**Files:**
- Create: benchmarks/laravel/composer.json
- Create: benchmarks/laravel/artisan
- Create: benchmarks/laravel/app/Jobs/BenchmarkJob.php
- Create: benchmarks/laravel/app/Console/Commands/PublishBenchmark.php
- Create: benchmarks/laravel/app/Console/Commands/ConsumeBenchmark.php
- Create: benchmarks/laravel/config/benchmark.php
- Create: benchmarks/laravel/drivers/
- Create: benchmarks/laravel/scripts/run-matrix.sh
- Create: benchmarks/laravel/README.md

**Step 1: Write failing benchmark contract test**

Chaque driver doit exposer setup, publish, consume, reset et metrics avec les mêmes payloads et garanties configurables.

Drivers :

- rabbit-rs ;
- php-amqplib direct ;
- vyuldashev/laravel-queue-rabbitmq comme driver Laravel RabbitMQ de référence ;
- Redis Laravel ;
- database témoin.

**Step 2: Verify failure**

Run: cd benchmarks/laravel && composer install && vendor/bin/phpunit

Expected: FAIL.

**Step 3: Implement the harness**

Mesurer throughput, p50/p95/p99 end-to-end, CPU, RSS, connexions, channels, doublons et pertes. Fournir modes CLI, FPM et Octane. Ne pas inclure SQS.

**Step 4: Verify a short matrix**

Run: benchmarks/laravel/scripts/run-matrix.sh --smoke

Expected: PASS et fichier JSON de résultats.

**Step 5: Commit**

    git add benchmarks/laravel
    git commit -m "perf: add reproducible Laravel queue comparison lab"

### Task 40: Calibrer les defaults et figer les budgets

**Files:**
- Create: benchmarks/baselines/reference-machine.json
- Create: benchmarks/baselines/v1-budget.json
- Create: docs/performance.md
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: crates/rabbit-rs-core/src/config.rs

**Step 1: Capture the reference environment**

Run: benchmarks/laravel/scripts/run-matrix.sh --full

Expected: résultats complets et métadonnées de machine.

**Step 2: Analyze batch and prefetch sweeps**

Comparer batch_messages, batch_bytes, flush interval, publisher channels, prefetch et max_in_flight. Examiner latence sous 50 %, 70 % et 90 % de saturation.

**Step 3: Set absolute and comparative gates**

Écrire les objectifs mesurés de débit et p99 par profil de payload, ainsi que le gain minimal attendu face au driver PHP RabbitMQ. Ne pas inventer un seuil non mesuré.

**Step 4: Update healthy defaults**

Changer les defaults uniquement si les tests de fairness, mémoire et latence restent verts.

**Step 5: Verify anti-regression check**

Run: benchmarks/laravel/scripts/run-matrix.sh --verify-budget benchmarks/baselines/v1-budget.json

Expected: PASS.

**Step 6: Commit**

    git add benchmarks/baselines docs/performance.md packages crates
    git commit -m "perf: calibrate safe queue and publisher defaults"

## Milestone F — Distribution et documentation

### Task 41: Préparer les packages Rabbit RS et la matrice PIE

**Files:**
- Modify: composer.json
- Modify: .gitattributes
- Modify: packages/laravel-queue/composer.json
- Create: release/pie-matrix.json
- Create: scripts/validate-distribution.sh
- Create: scripts/package-pie-binary.sh
- Create: scripts/split-laravel-package.sh

**Step 1: Write the failing distribution metadata check**

Le script vérifie :

- le package racine s'appelle goopil/rabbit-rs-native et son type est php-ext ;
- extension-name vaut rabbit_rs ;
- download-url-method contient seulement pre-packaged-binary ;
- NTS et ZTS sont annoncés ;
- Linux est la seule famille d'OS annoncée en V1 ;
- le package Laravel s'appelle goopil/rabbit-rs-laravel ;
- son namespace est Goopil\RabbitRs\Laravel ;
- il exige ext-rabbit_rs avec la même version majeure ;
- versions Cargo, extension PHP et tag de release sont cohérentes.

**Step 2: Verify failure**

Run: ./scripts/validate-distribution.sh

Expected: FAIL avant le manifeste et les contrôles.

**Step 3: Add the exact PIE matrix**

release/pie-matrix.json contient exactement 16 combinaisons :

    PHP: 8.4, 8.5
    architecture: x86_64, arm64
    libc: glibc, musl
    thread safety: nts, zts

Ne pas distribuer de build debug. Documenter la version minimale de glibc utilisée pour les artefacts glibc.

**Step 4: Implement deterministic PIE packaging**

scripts/package-pie-binary.sh reçoit version, PHP, architecture, libc, mode thread-safe et chemin du shared object. Il produit une archive ZIP conforme à PIE, par exemple :

    php_rabbit_rs-1.2.0_php8.5-x86_64-linux-glibc-nts.zip

L'archive contient rabbit_rs.so et aucune bibliothèque dynamique non documentée. Le script produit aussi le SHA-256 et refuse un nom, une ABI ou une architecture incohérents.

**Step 5: Implement the Laravel split dry-run**

scripts/split-laravel-package.sh extrait packages/laravel-queue, conserve son composer.json à la racine du résultat et refuse de publier si sa version majeure n'est pas compatible avec ext-rabbit_rs.

**Step 6: Verify**

Run: ./scripts/validate-distribution.sh && ./scripts/package-pie-binary.sh --self-test && ./scripts/split-laravel-package.sh --dry-run

Expected: PASS et matrice de 16 artefacts uniques.

**Step 7: Commit**

    git add composer.json .gitattributes packages/laravel-queue/composer.json release scripts
    git commit -m "build: define Rabbit RS PIE and Packagist packages"

### Task 42: Ajouter la CI et la publication synchronisée

**Files:**
- Create: .github/workflows/rust.yml
- Create: .github/workflows/php.yml
- Create: .github/workflows/integration.yml
- Create: .github/workflows/octane.yml
- Create: .github/workflows/bench-smoke.yml
- Create: .github/workflows/split-laravel.yml
- Create: .github/workflows/release.yml
- Create: scripts/verify-release-assets.sh

**Step 1: Write failing workflow and release checks**

scripts/verify-release-assets.sh vérifie les 16 archives attendues, leurs SHA-256, SBOM, attestations, noms PIE et versions synchronisées. Il échoue si un artefact debug ou une plateforme non supportée est publiée.

**Step 2: Validate before adding workflows**

Run: actionlint && ./scripts/verify-release-assets.sh --fixtures release/pie-matrix.json

Expected: FAIL avant workflows et fixtures.

**Step 3: Add build and test jobs**

Séparer tests Rust, PHPT, Laravel 12/13, intégration cluster, Octane et chaos programmé. Mettre en cache Cargo et Composer sans partager une extension construite entre deux ABI PHP.

**Step 4: Build and smoke-test all PIE binaries**

Pour chaque ligne de release/pie-matrix.json :

1. construire rabbit_rs.so avec la bonne ABI ;
2. inspecter architecture et dépendances dynamiques ;
3. charger l'extension avec php --ri rabbit_rs ;
4. exécuter un smoke test publication/consommation ;
5. créer l'archive PIE, le SHA-256 et la SBOM ;
6. produire une attestation GitHub.

Les dépendances Rust, Lapin et rustls sont liées statiquement autant que possible.

**Step 5: Publish a draft native release**

Créer une GitHub Release en brouillon et immuable après publication. Attacher les 16 archives et leurs preuves. Ne pas publier le brouillon si une combinaison manque.

**Step 6: Split and tag the Laravel package**

Le workflow split-laravel publie packages/laravel-queue dans le dépôt miroir goopil/rabbit-rs-laravel, en lecture seule, puis pousse exactement le même tag. Déclencher les mises à jour Packagist de goopil/rabbit-rs-native et goopil/rabbit-rs-laravel.

**Step 7: Verify installation as a user**

Dans des conteneurs propres représentatifs :

    pie install goopil/rabbit-rs-native
    composer require goopil/rabbit-rs-laravel
    php --ri rabbit_rs
    php artisan rabbit-rs:status --json

Expected: PIE sélectionne le bon binaire et Composer accepte la version de plateforme.

**Step 8: Publish only after synchronized verification**

Publier la GitHub Release uniquement après validation des artefacts, du dépôt miroir, des deux métadonnées Packagist et du test d'installation utilisateur.

**Step 9: Commit**

    git add .github scripts/verify-release-assets.sh
    git commit -m "ci: publish synchronized Rabbit RS releases"

### Task 43: Documenter installation, configuration et exploitation

**Files:**
- Create: README.md
- Create: docs/installation.md
- Create: docs/distribution.md
- Create: docs/configuration.md
- Create: docs/laravel.md
- Create: docs/topology.md
- Create: docs/reliability.md
- Create: docs/operations.md
- Create: docs/octane.md
- Create: docs/troubleshooting.md
- Create: examples/laravel/
- Create: scripts/test-docs.sh

**Step 1: Write documentation acceptance checklist**

Le lecteur doit pouvoir :

- installer l'extension avec pie install goopil/rabbit-rs-native ;
- installer le bridge avec composer require goopil/rabbit-rs-laravel ;
- utiliser PIE dans un Dockerfile sans image Rabbit RS dédiée ;
- compiler localement avec Cargo pour contribuer ;
- comprendre pourquoi Composer ne modifie pas le système PHP ;
- déclarer deux vhosts ;
- publier et lancer queue:work ;
- choisir declare/verify/external ;
- configurer quorum/classic ;
- activer explicitement une DLQ applicative si elle est souhaitée ;
- comprendre les doublons ;
- activer plugin delay ou TTL ;
- diagnostiquer une reconnexion ;
- configurer Supervisor/Kubernetes ;
- utiliser Octane sans retenir Request.

Indiquer explicitement que PECL, les paquets Debian/RPM/APK, un plugin Composer installant des binaires et les images PHP complètes ne sont pas des canaux V1.

**Step 2: Add copy-paste examples**

Tous les exemples sont exécutés en CI. Le README commence par les deux commandes d'installation et un exemple Laravel minimal.

**Step 3: Verify links and examples**

Run: ./scripts/test-docs.sh

Expected: PASS.

**Step 4: Commit**

    git add README.md docs examples scripts/test-docs.sh
    git commit -m "docs: document Rabbit RS installation and operations"

### Task 44: Effectuer la vérification de release

**Files:**
- Create: docs/release-checklist.md
- Modify: CHANGELOG.md

**Step 1: Run all fast checks**

Run: ./scripts/check.sh && ./scripts/test-extension.sh && ./scripts/validate-distribution.sh

Expected: PASS.

**Step 2: Run all PHP environments**

Run: ./scripts/test-fpm.sh && ./scripts/test-octane.sh --all

Expected: PASS.

**Step 3: Run integration and chaos**

Run: ./scripts/test-integration.sh && ./scripts/test-chaos.sh

Expected: PASS avec missing = 0.

**Step 4: Run performance gate**

Run: benchmarks/laravel/scripts/run-matrix.sh --verify-budget benchmarks/baselines/v1-budget.json

Expected: PASS.

**Step 5: Verify all release assets**

Run: ./scripts/verify-release-assets.sh --release-tag VERSION

Expected: 16 archives valides, 16 checksums valides, SBOM et attestation présentes, aucun build debug.

**Step 6: Verify fresh user installation**

Exécuter dans la matrice de conteneurs propres :

    pie install goopil/rabbit-rs-native:VERSION
    composer require goopil/rabbit-rs-laravel:^MAJOR
    php --ri rabbit_rs

Expected: PASS sur les 16 combinaisons annoncées.

**Step 7: Record evidence**

Ajouter versions, checksums, résultats, doublons observés, temps de recovery, URLs Packagist et tag du dépôt miroir dans docs/release-checklist.md.

**Step 8: Commit**

    git add CHANGELOG.md docs/release-checklist.md
    git commit -m "chore: record Rabbit RS release verification"

## Critères de fin

- Tous les tests Rust, PHPT, PHPUnit et matrices Composer passent.
- Les 16 artefacts PIE PHP 8.4/8.5, NTS/ZTS, glibc/musl et x86_64/ARM64 se chargent.
- pie install goopil/rabbit-rs-native sélectionne et active le bon artefact.
- composer require goopil/rabbit-rs-laravel valide ext-rabbit_rs sans modifier le système.
- Les tags et versions goopil/rabbit-rs-native, goopil/rabbit-rs-laravel et ext-rabbit_rs sont synchronisés.
- CLI, FPM, FrankenPHP, RoadRunner, Swoole et Open Swoole sont certifiés.
- Un queue:work standard consomme un profil contenant plusieurs vhosts.
- rabbit-rs:work supervise plusieurs queue:work sans réimplémenter Worker.
- Le lab chaos ne constate aucune perte silencieuse sans recréation manuelle de pool.
- Le recovery coordinator rétablit automatiquement connections, topology, publishers et consumers après une panne.
- Les doublons des fenêtres ambiguës sont mesurés et documentés.
- Le delay routing côté éditeur fonctionne en mode plugin et TTL fallback.
- La DLQ applicative est configurable depuis la config Laravel et déclarée par le topology reconciler.
- Les arguments de queue génériques (`x-delivery-limit`, etc.) sont câblés depuis la config Laravel.
- Le TLS est configurable (SNI, CA, cert client) et testé end-to-end.
- Les consumers sont proprement fermés (pas de fuite de channels en process long).
- Les events Laravel (connection state, backpressure) sont dispatchés depuis l'extension native.
- Les métriques consumer et les latences sont exposées dans le status command.
- La config publisher (confirms, mandatory, timeout) est câblée depuis Laravel.
- Le lifecycle Octane (reload, stop, consumer cleanup) est entièrement branché.
- Le WorkCommand supervisor est testé end-to-end (crash, restart, signaux).
- Les defaults de batch, prefetch et buffers proviennent des benchmarks.
- Les budgets absolus et comparatifs sont versionnés.
- Les logs et diagnostics ne révèlent aucun secret.
- Le comportement at-least-once et l'obligation d'idempotence sont clairement documentés.
