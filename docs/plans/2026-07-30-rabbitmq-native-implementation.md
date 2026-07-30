# RabbitMQ Native PHP Extension and Laravel Queue Driver Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Livrer une extension PHP Rust et un driver Laravel Queue performants, at-least-once, capables de mutualiser publication et consommation sur plusieurs vhosts avec reconnexion automatique.

**Architecture:** Un workspace Rust contient un noyau indépendant et une couche ext-php-rs. Un package Composer adapte cette API aux contrats Laravel Queue sans remplacer Illuminate\Queue\Worker. Les connexions et channels sont pilotés par des acteurs Tokio par processus PHP, tandis qu'un laboratoire RabbitMQ reproductible valide performances et scénarios de panne.

**Tech Stack:** Rust stable, Tokio, Lapin, ext-php-rs, PHP 8.4/8.5, Laravel 12/13, PHPUnit, Orchestra Testbench, RabbitMQ 4.3, Docker Compose, Prometheus, Toxiproxy, Criterion.

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

## Arborescence cible

    Cargo.toml
    rust-toolchain.toml
    crates/
      rabbitmq-core/
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
      rabbitmq-php/
        Cargo.toml
        src/
          lib.rs
          classes/
        stubs/rabbitmq_native.stub.php
        tests/phpt/
    packages/
      laravel-rabbitmq/
        composer.json
        config/rabbitmq.php
        src/
          RabbitMqServiceProvider.php
          Config/
          Connectors/
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
- Create: rust-toolchain.toml
- Create: .gitignore
- Create: crates/rabbitmq-core/Cargo.toml
- Create: crates/rabbitmq-core/src/lib.rs
- Create: crates/rabbitmq-php/Cargo.toml
- Create: crates/rabbitmq-php/src/lib.rs
- Create: scripts/check.sh

**Step 1: Write the failing workspace smoke check**

Créer scripts/check.sh avec :

    #!/usr/bin/env bash
    set -euo pipefail
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace --all-targets

**Step 2: Run it to verify it fails**

Run: ./scripts/check.sh

Expected: FAIL parce que le workspace et les crates ne sont pas encore déclarés.

**Step 3: Add the minimal workspace**

Déclarer resolver = "2", les deux members et les dépendances partagées. Épingler une toolchain Rust stable connue dans rust-toolchain.toml. Le crate rabbitmq-core doit compiler sans dépendance PHP. Le crate rabbitmq-php doit être un cdylib dépendant du core.

**Step 4: Run the check**

Run: chmod +x scripts/check.sh && ./scripts/check.sh

Expected: PASS.

**Step 5: Commit**

    git add Cargo.toml rust-toolchain.toml .gitignore crates scripts/check.sh
    git commit -m "build: bootstrap native RabbitMQ workspace"

### Task 2: Modéliser et valider la configuration native

**Files:**
- Create: crates/rabbitmq-core/src/config.rs
- Create: crates/rabbitmq-core/src/error.rs
- Modify: crates/rabbitmq-core/src/lib.rs
- Test: crates/rabbitmq-core/src/config.rs

**Step 1: Write failing tests**

Ajouter des tests pour :

- rejeter un broker sans hôte ;
- rejeter prefetch = 0 ;
- rejeter max_in_flight inférieur à un prefetch ;
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
        pub max_in_flight: u16,
        pub scheduler: SchedulerConfig,
    }

    pub enum TopologyMode {
        Declare,
        Verify,
        External,
    }

**Step 2: Verify failure**

Run: cargo test -p rabbitmq-core config::tests

Expected: FAIL avec types ou fonctions absents.

**Step 3: Implement minimal validated types**

Utiliser serde pour l'entrée, secrecy pour les secrets et une représentation canonique sans secret pour l'empreinte. Retourner ConfigError avec un chemin de champ exploitable par Laravel.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core config::tests

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add validated connection and worker configuration"

### Task 3: Implémenter le scheduler multi-queue déterministe

**Files:**
- Create: crates/rabbitmq-core/src/consumer/mod.rs
- Create: crates/rabbitmq-core/src/consumer/scheduler.rs
- Create: crates/rabbitmq-core/tests/scheduler_fairness.rs
- Modify: crates/rabbitmq-core/src/lib.rs

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

Run: cargo test -p rabbitmq-core --test scheduler_fairness

Expected: FAIL.

**Step 3: Implement deficit weighted round-robin**

Séparer priority_class et weight. Ajouter un aging borné pour qu'une classe basse prête finisse par être choisie. Ne pas ajouter de prefetch adaptatif.

**Step 4: Verify distribution**

Run: cargo test -p rabbitmq-core --test scheduler_fairness

Expected: PASS avec erreur de distribution sous la tolérance définie dans le test.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add starvation-safe weighted scheduler"

### Task 4: Rendre le runtime sûr après fork

**Files:**
- Create: crates/rabbitmq-core/src/runtime.rs
- Create: crates/rabbitmq-core/src/pool/mod.rs
- Create: crates/rabbitmq-core/src/pool/key.rs
- Modify: crates/rabbitmq-core/src/lib.rs
- Test: crates/rabbitmq-core/src/runtime.rs

**Step 1: Write failing lifecycle tests**

Injecter un PidProvider de test et vérifier :

- création paresseuse ;
- réutilisation dans le même PID ;
- invalidation de tous les handles après changement de PID ;
- une configuration différente ne partage pas le pool ;
- close est idempotent.

**Step 2: Verify failure**

Run: cargo test -p rabbitmq-core runtime::tests pool::tests

Expected: FAIL.

**Step 3: Implement RuntimeRegistry**

    pub struct RuntimeRegistry {
        pid: u32,
        runtime: tokio::runtime::Runtime,
        pools: HashMap<ConnectionKey, Arc<ConnectionHandle>>,
    }

Le runtime ne doit être créé ni dans une statique globale initialisée au chargement, ni avant la première acquisition après fork. Utiliser OnceLock uniquement pour le verrou du registre, pas pour une socket ou un runtime hérité.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core runtime::tests pool::tests

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add fork-safe per-process runtime registry"

### Task 5: Isoler Lapin derrière un transport testable

**Files:**
- Create: crates/rabbitmq-core/src/transport.rs
- Create: crates/rabbitmq-core/src/transport/lapin.rs
- Create: crates/rabbitmq-core/src/transport/mock.rs
- Modify: crates/rabbitmq-core/Cargo.toml
- Modify: crates/rabbitmq-core/src/lib.rs

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

Run: cargo test -p rabbitmq-core transport

Expected: FAIL.

**Step 3: Implement MockTransport then LapinTransport**

Commencer par le mock scriptable. Adapter ensuite Lapin sans exposer ses types hors du module transport/lapin.rs.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core transport

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): abstract AMQP transport behind testable traits"

### Task 6: Construire la machine de connexion et de recovery

**Files:**
- Create: crates/rabbitmq-core/src/recovery.rs
- Create: crates/rabbitmq-core/src/pool/connection_actor.rs
- Create: crates/rabbitmq-core/tests/recovery_state_machine.rs
- Modify: crates/rabbitmq-core/src/pool/mod.rs

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

Run: cargo test -p rabbitmq-core --test recovery_state_machine

Expected: FAIL.

**Step 3: Implement ConnectionActor**

Toutes les opérations passent par un canal mpsc borné. Les états et raisons sont publiés via watch. Le générateur de jitter et l'horloge sont injectables.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core --test recovery_state_machine

Expected: PASS sans attente réelle.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add deterministic connection recovery actor"

### Task 7: Déclarer ou vérifier la topologie

**Files:**
- Create: crates/rabbitmq-core/src/topology/mod.rs
- Create: crates/rabbitmq-core/src/topology/plan.rs
- Create: crates/rabbitmq-core/src/topology/reconciler.rs
- Create: crates/rabbitmq-core/tests/topology_recovery.rs

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

Run: cargo test -p rabbitmq-core --test topology_recovery

Expected: FAIL.

**Step 3: Implement TopologyPlan and Reconciler**

Compiler la configuration en plan immuable avant toute I/O. Refuser les combinaisons quorum exclusive ou auto_delete. Ne pas tenter de créer des policies RabbitMQ.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core --test topology_recovery

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add declarative and externally managed topology modes"

### Task 8: Implémenter batching, confirms et mandatory returns

**Files:**
- Create: crates/rabbitmq-core/src/publisher/mod.rs
- Create: crates/rabbitmq-core/src/publisher/batcher.rs
- Create: crates/rabbitmq-core/src/publisher/confirms.rs
- Create: crates/rabbitmq-core/src/publisher/actor.rs
- Create: crates/rabbitmq-core/tests/publisher_safety.rs

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
- coupure avant confirm retourne Ambiguous ;
- message_id conservé lors d'une republication.

**Step 2: Verify failure**

Run: cargo test -p rabbitmq-core --test publisher_safety

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

**Step 4: Verify**

Run: cargo test -p rabbitmq-core --test publisher_safety

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add bounded batched publisher confirms"

### Task 9: Ajouter les délais plugin et TTL

**Files:**
- Create: crates/rabbitmq-core/src/topology/delay.rs
- Create: crates/rabbitmq-core/src/publisher/delay.rs
- Create: crates/rabbitmq-core/tests/delay_routing.rs
- Modify: crates/rabbitmq-core/src/config.rs

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

Run: cargo test -p rabbitmq-core --test delay_routing

Expected: FAIL.

**Step 3: Implement DelayStrategy**

    pub enum DelayStrategy {
        Plugin,
        TtlBuckets(TtlBucketPlan),
    }

La détection du plugin doit être bornée dans le temps et mise en cache par génération de connexion.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core --test delay_routing

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add delayed exchange and TTL fallback"

### Task 10: Implémenter ConsumerSet et les jetons de delivery

**Files:**
- Create: crates/rabbitmq-core/src/consumer/set.rs
- Create: crates/rabbitmq-core/src/consumer/delivery.rs
- Create: crates/rabbitmq-core/src/consumer/actor.rs
- Create: crates/rabbitmq-core/tests/consumer_semantics.rs

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

Run: cargo test -p rabbitmq-core --test consumer_semantics

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

Run: cargo test -p rabbitmq-core --test consumer_semantics

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): add multiplexed consumers and safe delivery tokens"

### Task 11: Ajouter les compteurs attempts et poison-message

**Files:**
- Create: crates/rabbitmq-core/src/consumer/attempts.rs
- Create: crates/rabbitmq-core/tests/delivery_attempts.rs
- Modify: crates/rabbitmq-core/src/consumer/delivery.rs

**Step 1: Write failing attempts tests**

Cas :

- première acquisition = 1 ;
- x-acquired-count prioritaire sur redelivered bool ;
- x-delivery-count lu pour les échecs quorum ;
- release différé incrémente le compteur applicatif ;
- limite atteinte produit MaxAttempts ;
- classic sans compteur utilise le fallback documenté.

**Step 2: Verify failure**

Run: cargo test -p rabbitmq-core --test delivery_attempts

Expected: FAIL.

**Step 3: Implement AttemptsResolver**

Centraliser toute interprétation des headers. Ne pas disperser les règles RabbitMQ dans la couche PHP.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core --test delivery_attempts

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): preserve Laravel-compatible delivery attempts"

### Task 12: Exposer un snapshot de métriques sans backend

**Files:**
- Create: crates/rabbitmq-core/src/metrics.rs
- Create: crates/rabbitmq-core/tests/metrics_snapshot.rs
- Modify: crates/rabbitmq-core/src/publisher/actor.rs
- Modify: crates/rabbitmq-core/src/consumer/actor.rs
- Modify: crates/rabbitmq-core/src/pool/connection_actor.rs

**Step 1: Write failing metric tests**

Vérifier que publish, confirm, return, delivery, ACK, reject, reconnect et backpressure mettent à jour les bons compteurs. Vérifier qu'un snapshot ne bloque pas les acteurs et ne contient aucun secret.

**Step 2: Verify failure**

Run: cargo test -p rabbitmq-core --test metrics_snapshot

Expected: FAIL.

**Step 3: Implement atomics and histograms**

Garder une API de snapshot sérialisable. Ne dépendre ni de Prometheus ni d'OpenTelemetry dans rabbitmq-core.

**Step 4: Verify**

Run: cargo test -p rabbitmq-core --test metrics_snapshot

Expected: PASS.

**Step 5: Run Milestone A gate**

Run: ./scripts/check.sh

Expected: PASS.

**Step 6: Commit**

    git add crates/rabbitmq-core
    git commit -m "feat(core): expose transport metrics snapshots"

## Milestone B — Extension PHP

### Task 13: Définir l'API et les stubs PHP

**Files:**
- Create: crates/rabbitmq-php/src/classes/mod.rs
- Create: crates/rabbitmq-php/src/classes/pool.rs
- Create: crates/rabbitmq-php/src/classes/consumer.rs
- Create: crates/rabbitmq-php/src/classes/delivery.rs
- Create: crates/rabbitmq-php/src/classes/exception.rs
- Create: crates/rabbitmq-php/stubs/rabbitmq_native.stub.php
- Modify: crates/rabbitmq-php/src/lib.rs
- Create: scripts/test-extension.sh

**Step 1: Write failing reflection tests**

Créer PHPT vérifiant l'existence de :

    RabbitMQ\Native\Pool
    RabbitMQ\Native\Consumer
    RabbitMQ\Native\Delivery
    RabbitMQ\Native\Exception
    RabbitMQ\Native\BackpressureException
    RabbitMQ\Native\ConnectionException

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

Run: cargo build -p rabbitmq-php --release && ./scripts/test-extension.sh reflection

Expected: FAIL.

**Step 3: Implement thin ext-php-rs classes**

Chaque objet PHP contient seulement un handle Arc ou identifiant natif. Convertir les erreurs Rust en exceptions PHP stables. Ne pas exposer Lapin.

**Step 4: Verify**

Run: cargo build -p rabbitmq-php --release && ./scripts/test-extension.sh reflection

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbitmq-php scripts/test-extension.sh
    git commit -m "feat(extension): expose native pool publisher and consumer API"

### Task 14: Tester conversions, erreurs et transitions PHP

**Files:**
- Create: crates/rabbitmq-php/tests/phpt/config_validation.phpt
- Create: crates/rabbitmq-php/tests/phpt/binary_payload.phpt
- Create: crates/rabbitmq-php/tests/phpt/delivery_terminal_state.phpt
- Create: crates/rabbitmq-php/tests/phpt/secrets.phpt
- Create: crates/rabbitmq-php/tests/phpt/backpressure.phpt

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

    git add crates/rabbitmq-php
    git commit -m "test(extension): harden PHP value conversion and handle states"

### Task 15: Certifier le cycle de vie CLI, fork et FPM

**Files:**
- Create: crates/rabbitmq-php/tests/phpt/pid_registry.phpt
- Create: crates/rabbitmq-php/tests/phpt/fork_invalidation.phpt
- Create: crates/rabbitmq-php/tests/fixtures/fpm/index.php
- Create: crates/rabbitmq-php/tests/fixtures/fpm/php-fpm.conf
- Create: scripts/test-fpm.sh
- Modify: crates/rabbitmq-php/src/classes/pool.rs

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

    git add crates/rabbitmq-php scripts
    git commit -m "feat(extension): make native pools safe across PHP process lifecycles"

## Milestone C — Package Laravel

### Task 16: Initialiser le package et sa configuration

**Files:**
- Create: packages/laravel-rabbitmq/composer.json
- Create: packages/laravel-rabbitmq/phpunit.xml
- Create: packages/laravel-rabbitmq/src/RabbitMqServiceProvider.php
- Create: packages/laravel-rabbitmq/src/Config/ConfigNormalizer.php
- Create: packages/laravel-rabbitmq/config/rabbitmq.php
- Create: packages/laravel-rabbitmq/tests/TestCase.php
- Create: packages/laravel-rabbitmq/tests/Unit/ConfigNormalizerTest.php

**Step 1: Write failing package tests**

Tester la publication de configuration, la validation des brokers/routes/workers, les defaults quorum/confirm/mandatory, l'absence de DLQ applicative par défaut, le masquage des secrets et l'erreur si ext-rabbitmq-native manque.

**Step 2: Verify failure**

Run: cd packages/laravel-rabbitmq && composer install && vendor/bin/phpunit --filter ConfigNormalizerTest

Expected: FAIL.

**Step 3: Implement package skeleton**

Utiliser illuminate/queue et Orchestra Testbench avec une matrice Composer Laravel 12/13. Le package Composer exige PHP ^8.4 et ext-rabbitmq-native.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter ConfigNormalizerTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): bootstrap native RabbitMQ queue package"

### Task 17: Enregistrer le connecteur et le pool partagé

**Files:**
- Create: packages/laravel-rabbitmq/src/Connectors/RabbitMqConnector.php
- Create: packages/laravel-rabbitmq/src/Support/NativePoolFactory.php
- Create: packages/laravel-rabbitmq/tests/Unit/RabbitMqConnectorTest.php
- Modify: packages/laravel-rabbitmq/src/RabbitMqServiceProvider.php

**Step 1: Write failing connector tests**

Vérifier Queue::connection retourne le driver, deux résolutions équivalentes partagent le handle de pool, une empreinte différente crée un autre handle et aucune Request n'est retenue.

**Step 2: Verify failure**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqConnectorTest

Expected: FAIL.

**Step 3: Implement connector and factory**

Enregistrer le nom rabbitmq-native. Le factory transmet une configuration normalisée immuable à RabbitMQ\Native\Pool.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqConnectorTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): register native RabbitMQ queue connector"

### Task 18: Implémenter push, later et bulk

**Files:**
- Create: packages/laravel-rabbitmq/src/RabbitMqQueue.php
- Create: packages/laravel-rabbitmq/src/Support/MessageMapper.php
- Create: packages/laravel-rabbitmq/tests/Unit/RabbitMqQueuePublishTest.php
- Modify: packages/laravel-rabbitmq/src/Connectors/RabbitMqConnector.php

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

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqQueuePublishTest

Expected: FAIL.

**Step 3: Implement minimal publishing adapter**

Étendre Illuminate\Queue\Queue et implémenter Illuminate\Contracts\Queue\Queue. Ne pas dupliquer createPayload.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqQueuePublishTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): publish immediate delayed and bulk jobs"

### Task 19: Implémenter RabbitMqJob

**Files:**
- Create: packages/laravel-rabbitmq/src/Jobs/RabbitMqJob.php
- Create: packages/laravel-rabbitmq/tests/Unit/RabbitMqJobTest.php
- Modify: packages/laravel-rabbitmq/src/RabbitMqQueue.php

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

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqJobTest

Expected: FAIL.

**Step 3: Implement Job adapter**

Étendre Illuminate\Queue\Jobs\Job. Garder Delivery privé et libérer son handle après transition terminale.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqJobTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): map native deliveries to Laravel jobs"

### Task 20: Brancher pop sur un profil multi-vhost

**Files:**
- Create: packages/laravel-rabbitmq/src/Support/WorkerProfileResolver.php
- Create: packages/laravel-rabbitmq/tests/Feature/MultiVhostWorkerTest.php
- Modify: packages/laravel-rabbitmq/src/RabbitMqQueue.php
- Modify: packages/laravel-rabbitmq/config/rabbitmq.php

**Step 1: Write failing feature test**

Configurer deux brokers/vhosts et trois subscriptions. Vérifier qu'un seul appel pop sur le profil main peut rendre des jobs des trois sources, avec bons connectionName, queue et attempts.

Tester aussi un profil inconnu, une subscription désactivée et un timeout sans job.

**Step 2: Verify failure**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter MultiVhostWorkerTest

Expected: FAIL.

**Step 3: Implement aggregate pop**

La valeur queue de la connexion Laravel référence par défaut le nom du profil worker. Documenter que la sélection fine de plusieurs aliases par option --queue arrive avec rabbitmq:work ; ne pas simuler une boucle bloquante queue par queue.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter MultiVhostWorkerTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): consume multi-vhost worker profiles"

### Task 21: Implémenter size, clear et monitoring

**Files:**
- Create: packages/laravel-rabbitmq/tests/Unit/RabbitMqQueueAdminTest.php
- Modify: packages/laravel-rabbitmq/src/RabbitMqQueue.php

**Step 1: Write failing admin tests**

Vérifier size agrégé et par route, clear explicite, refus de clear sans permission de configuration, et métriques Monitor.

**Step 2: Verify failure**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqQueueAdminTest

Expected: FAIL.

**Step 3: Implement bounded admin operations**

Ne pas utiliser l'API HTTP management pour le chemin critique. Les commandes AMQP passives suffisent pour size lorsque disponibles.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqQueueAdminTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): add queue administration and monitoring"

### Task 22: Ajouter événements natifs et commande de diagnostic

**Files:**
- Create: packages/laravel-rabbitmq/src/Events/ConnectionStateChanged.php
- Create: packages/laravel-rabbitmq/src/Events/BackpressureDetected.php
- Create: packages/laravel-rabbitmq/src/Console/RabbitMqStatusCommand.php
- Create: packages/laravel-rabbitmq/tests/Feature/RabbitMqStatusCommandTest.php
- Modify: packages/laravel-rabbitmq/src/RabbitMqServiceProvider.php

**Step 1: Write failing command tests**

Vérifier sortie humaine et JSON, absence de secrets, états par broker/vhost, buffers, confirms et génération.

**Step 2: Verify failure**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqStatusCommandTest

Expected: FAIL.

**Step 3: Implement status adapter**

La commande rabbitmq:status lit seulement Pool::stats. Elle ne doit ni reconnecter ni modifier la topologie sauf option explicite future.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter RabbitMqStatusCommandTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): expose native connection diagnostics"

### Task 23: Ajouter la commande multiprocessus progressive

**Files:**
- Create: packages/laravel-rabbitmq/src/Console/RabbitMqWorkCommand.php
- Create: packages/laravel-rabbitmq/src/Console/WorkerSupervisor.php
- Create: packages/laravel-rabbitmq/tests/Unit/WorkerSupervisorTest.php
- Create: packages/laravel-rabbitmq/tests/Feature/RabbitMqWorkCommandTest.php
- Modify: packages/laravel-rabbitmq/src/RabbitMqServiceProvider.php

**Step 1: Write failing supervisor tests**

Tester construction de la commande enfant, workers = 1 et workers > 1, propagation SIGTERM/SIGINT, redémarrage avec backoff, arrêt propre, max restarts et codes de sortie.

**Step 2: Verify failure**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter 'WorkerSupervisorTest|RabbitMqWorkCommandTest'

Expected: FAIL.

**Step 3: Implement orchestration only**

Chaque enfant exécute queue:work avec une connexion/profil déterminé. Utiliser Symfony Process. Ne pas appeler des handlers de job depuis le superviseur.

**Step 4: Verify**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter 'WorkerSupervisorTest|RabbitMqWorkCommandTest'

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-rabbitmq
    git commit -m "feat(laravel): supervise multiple standard queue workers"

### Task 24: Certifier Octane

**Files:**
- Create: packages/laravel-rabbitmq/src/Octane/OctaneLifecycle.php
- Create: packages/laravel-rabbitmq/tests/Feature/OctaneLifecycleTest.php
- Create: scripts/test-octane.sh
- Modify: packages/laravel-rabbitmq/src/RabbitMqServiceProvider.php

**Step 1: Write failing lifecycle tests**

Vérifier :

- aucune Request conservée ;
- deux requêtes réutilisent le même pool dans un worker ;
- reload ferme le pool ;
- worker stop draine dans la deadline ;
- requête annulée ne laisse pas une attente PHP orpheline ;
- pool indépendant par worker.

**Step 2: Verify failure**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit --filter OctaneLifecycleTest

Expected: FAIL.

**Step 3: Implement Octane hooks**

Détecter Octane de manière optionnelle. Ne pas rendre laravel/octane obligatoire pour les utilisateurs FPM.

**Step 4: Verify package tests**

Run: cd packages/laravel-rabbitmq && vendor/bin/phpunit

Expected: PASS.

**Step 5: Run Milestone C gate**

Run: ./scripts/test-octane.sh --server=frankenphp && ./scripts/test-octane.sh --server=roadrunner && ./scripts/test-octane.sh --server=swoole && ./scripts/test-octane.sh --server=openswoole

Expected: PASS pour chaque runtime certifié.

**Step 6: Commit**

    git add packages/laravel-rabbitmq scripts/test-octane.sh
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
- Create: crates/rabbitmq-core/tests/integration/publish_consume.rs
- Create: crates/rabbitmq-core/tests/integration/topology_modes.rs
- Create: packages/laravel-rabbitmq/tests/Integration/QueueWorkerTest.php
- Create: packages/laravel-rabbitmq/tests/Integration/DelayedJobTest.php
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
- Create: crates/rabbitmq-core/tests/chaos/reconnect.rs
- Create: packages/laravel-rabbitmq/tests/Integration/AtLeastOnceChaosTest.php
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

### Task 28: Créer bench-native

**Files:**
- Create: benchmarks/native/Cargo.toml
- Create: benchmarks/native/benches/batching.rs
- Create: benchmarks/native/benches/scheduler.rs
- Create: benchmarks/native/benches/transport.rs
- Create: benchmarks/native/README.md
- Modify: Cargo.toml

**Step 1: Add benchmark smoke tests**

Les benchmarks doivent couvrir tailles 256 o, 1 Kio, 10 Kio, 100 Kio et 1 Mio, batch 1/16/64/256, confirms, coût scheduler et allocation.

**Step 2: Verify command**

Run: cargo bench -p rabbitmq-native-bench --no-run

Expected: FAIL avant le crate benchmark.

**Step 3: Implement Criterion suites**

Séparer microbench sans broker et bench transport avec le lab. Enregistrer version, CPU, noyau, RabbitMQ, payload et configuration dans chaque résultat.

**Step 4: Verify**

Run: cargo bench -p rabbitmq-native-bench --no-run

Expected: PASS.

**Step 5: Commit**

    git add Cargo.toml benchmarks/native
    git commit -m "perf: add native batching and transport benchmarks"

### Task 29: Créer l'application bench-laravel

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

- rabbitmq-native ;
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

### Task 30: Calibrer les defaults et figer les budgets

**Files:**
- Create: benchmarks/baselines/reference-machine.json
- Create: benchmarks/baselines/v1-budget.json
- Create: docs/performance.md
- Modify: packages/laravel-rabbitmq/config/rabbitmq.php
- Modify: crates/rabbitmq-core/src/config.rs

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

### Task 31: Ajouter les matrices CI

**Files:**
- Create: .github/workflows/rust.yml
- Create: .github/workflows/php.yml
- Create: .github/workflows/integration.yml
- Create: .github/workflows/octane.yml
- Create: .github/workflows/bench-smoke.yml
- Create: .github/workflows/release.yml

**Step 1: Write local matrix manifest**

Créer une matrice couvrant PHP 8.4/8.5, Laravel 12/13, x86_64/ARM64, glibc/musl et NTS/ZTS lorsque le SAPI le permet.

**Step 2: Validate workflow syntax**

Run: actionlint

Expected: FAIL avant workflows, puis PASS après ajout.

**Step 3: Add build and test jobs**

Séparer tests rapides, intégration cluster, Octane et chaos programmé. Mettre en cache Cargo et Composer sans mettre en cache l'extension construite entre ABI PHP différentes.

**Step 4: Add release artifacts**

Produire extension, checksum, SBOM et provenance. Vérifier le chargement réel de chaque artefact avec php --ri rabbitmq_native.

**Step 5: Commit**

    git add .github
    git commit -m "ci: test and package supported PHP RabbitMQ matrices"

### Task 32: Documenter installation, configuration et exploitation

**Files:**
- Create: README.md
- Create: docs/installation.md
- Create: docs/configuration.md
- Create: docs/laravel.md
- Create: docs/topology.md
- Create: docs/reliability.md
- Create: docs/operations.md
- Create: docs/octane.md
- Create: docs/troubleshooting.md
- Create: examples/laravel/

**Step 1: Write documentation acceptance checklist**

Le lecteur doit pouvoir :

- installer l'extension ;
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

**Step 2: Add copy-paste examples**

Tous les exemples doivent être exécutés dans la CI documentation ou par scripts de smoke test.

**Step 3: Verify links and examples**

Run: ./scripts/test-docs.sh

Expected: PASS.

**Step 4: Commit**

    git add README.md docs examples scripts/test-docs.sh
    git commit -m "docs: document native RabbitMQ Laravel operations"

### Task 33: Effectuer la vérification de release

**Files:**
- Create: docs/release-checklist.md
- Modify: CHANGELOG.md

**Step 1: Run all fast checks**

Run: ./scripts/check.sh && ./scripts/test-extension.sh

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

**Step 5: Verify artifacts**

Charger chaque extension produite avec la bonne ABI PHP et exécuter le smoke test de publication/consommation.

**Step 6: Record evidence**

Ajouter versions, checksums, résultats, doublons observés et temps de recovery dans docs/release-checklist.md.

**Step 7: Commit**

    git add CHANGELOG.md docs/release-checklist.md
    git commit -m "chore: record native RabbitMQ release verification"

## Critères de fin

- Tous les tests Rust, PHPT, PHPUnit et matrices Composer passent.
- Les artefacts PHP 8.4/8.5 glibc/musl se chargent sur x86_64 et ARM64.
- CLI, FPM, FrankenPHP, RoadRunner, Swoole et Open Swoole sont certifiés.
- Un queue:work standard consomme un profil contenant plusieurs vhosts.
- rabbitmq:work supervise plusieurs queue:work sans réimplémenter Worker.
- Le lab chaos ne constate aucune perte silencieuse.
- Les doublons des fenêtres ambiguës sont mesurés et documentés.
- Les defaults de batch, prefetch et buffers proviennent des benchmarks.
- Les budgets absolus et comparatifs sont versionnés.
- Les logs et diagnostics ne révèlent aucun secret.
- Le comportement at-least-once et l'obligation d'idempotence sont clairement documentés.
