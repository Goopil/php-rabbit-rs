# Post-pump : benchs Laravel, sweep, mimalloc

Date: 2026-08-29
Base: main (d923fe1, PR #30 mergée) — branche Phase A `bench/laravel-realistic` déjà créée (vide, sur d923fe1)
Précédent: `2026-08-28-publish-pump-v2.md` (exécuté, mergé)

## Contexte

Le pump v2 est mergé : 268k msg/s publish fire-and-forget en release (×2.7 la cible
80-100k), AA publish +44 %, consume/p99 en parité, 0 pertes/duplicatas. Le fil devient
la consolidation :

1. Mesurer ce qui est **réellement utilisé côté Laravel** (les benchs actuels mesurent
   `publishBatch` + payload 256 B, alors que Laravel publie en unitaire bufferisé Safe
   avec des enveloppes ~1-2 KB et consomme en unitaire + ack par job).
2. Simplifier la codebase maintenant que la perf est au rendez-vous (sweep du code mort).
3. Évaluer un allocateur alternatif (mimalloc) sur des mesures honnêtes.

Les scénarios créés en Phase A servent ensuite de base de mesure pour B (confirmation
du sweep) et C (A/B mimalloc) — d'où l'ordre A → B → C.

## Décisions tranchées (archivées — ne pas rouvrir)

- **Pump-replay / collapse des modes** : rejeté. Unsafe-actor a des qualités uniques
  (waiters par message, métriques + `BackpressureDetected`, close propre, delay routing
  — le pump bypass `delay_ms > 0`). Blind-pump et Unsafe-actor = deux produits distincts.
- **Suppression du buffer PHP publish** (`publish_buffer` 64/1ms, pool.rs) : rejeté.
  Porteur en Safe (défaut Laravel) : amortit les RTT d'ack (64 msgs/RTT au lieu de
  ~1000 msg/s par message unitaire). Laravel : `push/pushRaw/later/Horizon` →
  `pool->publish()` bufferisé (RabbitMqQueue.php:436) ; `bulk` → `publishBatch` (:292).
- **parking_lot** : rejeté (<1-2 % ; locks chauds rares et uncontended ; métriques déjà
  lock-free atomiques, metrics.rs:25).
- **Poisoning** : clôturé, rien à faire. 2 styles cohérents (fail-fast pool.rs/
  callbacks.rs `.expect("... poisoned")` ; tolérant consumer/set.rs ×4 + runtime.rs:139
  `unwrap_or_else(PoisonError::into_inner)`) ; `tokio::sync::Mutex` sans poisoning ;
  sections critiques triviales, panic-free par design.

## Contraintes globales

- Gate complet avant de déclarer une tâche finie : `rtk ./scripts/check.sh` +
  `./scripts/test-extension.sh` + `./scripts/test-laravel.sh`.
- Benchs en **release uniquement** (debug masque ~4×) ; runs interleavés ;
  garder les JSON par run dans le workspace SDD.
- Extension rebuild avec `--features extension-tests` après tout changement Rust
  (sinon 38 échecs fantômes `testing_pool`) ; rebuild obligatoire après bump de version.
- Contrat at-least-once intangible pour Safe/Unsafe. Blind = fire-and-forget explicite
  (perte silencieuse sur erreur transport, documentée). Crash-safe = outbox externe,
  hors scope.
- SDD pour chaque phase (brief fichier → implementeur → reviewer → ledger dans
  `.superpowers/sdd/<date>-<slug>/`).
- MR séparées, chacune depuis main, validées explicitement par l'utilisateur avant exécution.

## Phase A — Benchs représentatifs de Laravel (MR 1)

Branche: `bench/laravel-realistic` (créée sur d923fe1).

### Task 1 — Framework + scénarios + driver rabbit-rs

Faits vérifiés : scénarios enregistrés dans run-benchmarks.php:60-64 ; drivers
auto-détectés (:47-58, amqp-ext skip si absent) ; drivers câblés via `match` sur
`$this->scenarioMode` (RabbitRsDriver.php:46-72) ; phases publish/consume chronométrées
séparément (fill-then-drain, AbstractBenchmark.php:51-92) ; budget global
smoke-budget.json (min 1000 pub / 500 conso, losses=0) ; prefetch défaut Laravel = 64
(packages/laravel-queue/config/rabbit-rs.php:208).

- `ScenarioMode.php` : + `LARAVEL_DISPATCH = 'laravel-dispatch'`,
  `LARAVEL_WORKER = 'laravel-worker'`.
- `Config.php` : + `MESSAGE_PAYLOAD_LARAVEL_BYTES = 1024` (enveloppe Laravel ~1-2 KB),
  + `PREFETCH_LARAVEL = 64` (défaut Laravel, vs 128 actuel).
- `AbstractBenchmark.php` : propriété `protected int $payloadBytes` surchargeable,
  utilisée par `createMessage()` (remplace l'accès direct à la constante :30-44).
- Deux scénarios à headline orthogonal (la phase non-mesurée est un fill/drain le plus
  rapide possible pour ne pas polluer le signal) :
  - `Scenarios/LaravelDispatchBenchmark.php` — headline **publish** : publish unitaire
    (`pool->publish()`) ×10k en Safe (confirms+mandatory), payload 1024 B ; drain
    rapide par batch.
  - `Scenarios/LaravelWorkerBenchmark.php` — headline **consume** : fill rapide
    (publishBatch blind), consume `next()` unitaire + ack par message, payload 1024 B,
    prefetch 64.
- `run-benchmarks.php` : + 2 entrées dans la map `$scenarios`.
- `RabbitRsDriver.php` : branches `match` pour les 2 modes — dispatch : config
  confirms/mandatory/safe + publish unitaire ; worker : else-arm consume existante
  (tryNext/next + ack, RabbitRsDriver.php:159-183) avec prefetch 64 dans la config.
- Gate : `composer validate`, smoke ciblé `--scenario=laravel-dispatch --driver=rabbit-rs`.

### Task 2 — Wiring des 3 autres drivers

- amqplib : publish unitaire `basic_publish` + `wait_for_pending_acks_returns` tous
  les 64 (miroir du flush buffer rabbit-rs → RTT amorti équitablement) ; consume
  worker = basic_get + ack unitaire.
- bunny / amqp-ext : mêmes principes, selon le support confirms existant de leur
  chemin batch-confirm (batch 256 → flush 64, appels unitaires).
- Gate : smoke par driver (amqp-ext auto-skip si l'extension n'est pas installée).

### Task 3 — Run complet + PR

- Budget inchangé a priori (seuils globaux larges) ; ajustement seulement si justifié.
- Run release complet, JSON archivé dans le workspace SDD, vérif losses=0/dups=0
  (Safe), tableau comparatif 4 drivers × 2 scénarios.
- PR `bench/laravel-realistic` → main.

## Phase B — Sweep dead code publish (MR 2, ~230 lignes)

Branche depuis main. Items identifiés lors de l'évaluation simplification, vérifiés
morts (ni PHP, ni Rust, ni benchs) :

1. Alias `try_publish_hot` (mort depuis le routage blind vers le pump).
2. `PublishPump::try_publish` (mort, cf. note pump-v2 Task 4 : « conservé pour compat,
   plus utilisé par le chemin blind principal »).
3. Else-arm mort dans le routage.
4. Code de barrière `mandatory:true` devenu inutile.
5. Fallback `publish_blind` (mort).
6. API `publish_batch_detailed` (~150 lignes avec ses tests) — aucun appelant.

SDD : T1 suppression + grep appelants + tests verts → T2 bench de confirmation
(réutilise les scénarios Phase A : perfs identiques attendues) → PR.

**Garder explicitement** : dualité pump/actor, byte budget/semaphore/ledger,
3 modes distincts (Blind/Unsafe/Safe).

## Phase C — A/B mimalloc (MR 3, après A)

- Dépendance `mimalloc` + `#[global_allocator]` dans la cdylib
  (crates/rabbit-rs-php/src/lib.rs). Couvre tous les allocs Rust ; Zend MM séparé.
- A/B release main vs branche, interleavés : fire-and-forget, batch-confirm,
  auto-ack + les 2 scénarios Laravel (Phase A).
- Métrique supplémentaire : **max RSS** du process (long-lived FPM/Octane) via
  `/usr/bin/time -l` ou sonde dans le runner.
- Critère de conservation : batch-confirm ≥ 5 % ou RSS sensiblement réduit,
  **sans régression** ailleurs ; sinon rejet.

## Phase D — Backlog (au fil de l'eau, pas de MR planifiée)

1. Plomber `publisher.safety` dans ConfigNormalizer
   (packages/laravel-queue/src/Config/ConfigNormalizer.php:378-392).
2. Doc limitation `delay_ms` en blind (le pump bypass le routing delay).
3. Polish wording/stubs + minors parkés des reviews #29/#30.
4. Fix composition consumer multi-broker (client.rs:362, e2e
   `two_vhosts_in_one_consumer_set`).
5. Standardiser benchs release (protocole obligatoire, doc).

## Ordre d'exécution

A → B → C (les scénarios A servent de base de mesure pour B et C).
D au fil de l'eau. Chaque phase validée explicitement avant exécution.
