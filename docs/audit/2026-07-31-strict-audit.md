# Audit strict — Plans Rabbit RS (design + implementation) vs code implémenté

**Date :** 31 juillet 2026
**Auditeur :** euria-code
**Périmètre :** `docs/plans/2026-07-30-rabbitmq-native-design.md`, `docs/plans/2026-07-30-rabbitmq-native-implementation.md`, code Tasks 1–12 (Milestone A terminé).

## Résumé exécutif

L'architecture générale est saine : séparation core/extension/package, abstraction `Transport`, acteurs Tokio, tests déterministes. Le Milestone A passe son gate (100 tests, clippy, fmt). Cependant, l'audit révèle **5 bugs HIGH** (perte de données, deadlock, fuite mémoire), **7 problèmes MEDIUM** et **5 LOW**, plus **6 divergences** entre les plans et le code. La plupart des HIGH sont des bugs silencieux qui ne se manifesteront qu'en intégration réelle (Milestone D) ou en production.

| Sévérité | Count | Bloquant ? |
|----------|-------|------------|
| 🔴 HIGH   | 5     | Oui — avant Milestone B |
| 🟡 MEDIUM | 7     | Recommandé — avant Milestone C |
| 🔵 LOW    | 5     | Non — backlog |
| 🏗️ Architecture/dérive | 6 | Recommandé — avant Milestone B |

---

## Bugs critiques (HIGH)

### #1 — AMQP `message_id` perdu à la consommation

**Fichier :** `crates/rabbit-rs-core/src/transport/lapin.rs:251`

`map_headers` n'extrait que `properties.headers()`. Les basic properties (`message_id`, `correlation_id`, `timestamp`, `delivery_mode`) sont jetées. Le `Delivery` Rust construit un ID synthétique `"generation:channel:delivery_tag"` qui **change à chaque redelivery**.

**Impact :** Laravel `getJobId()` doit retourner l'UUID stable posé par le publisher. Sans cela, les retries Laravel, le dédoublonnage et les failed jobs sont cassés.

**Fix :** Étendre `Delivery` (transport) avec `message_id: Option<String>` et `correlation_id: Option<String>`, les extraire dans `map_delivery` depuis `delivery.properties`, et propager jusqu'au `Delivery` public du consumer.

### #2 — Bug de rejet de génération dans le publisher

**Fichier :** `crates/rabbit-rs-core/src/publisher/actor.rs:408`

```rust
if generation <= state.generation {  // BUG: devrait être <
```

Si le coordinateur envoie `Recovering { generation: N }` puis `Ready { generation: N }` (même génération, cas normal), `suspend()` avance `self.generation` à `N`. Ensuite `Ready { generation: N }` est rejeté car `N <= N`. Le publisher ne reprend jamais.

`connection_lost()` masque le bug en envoyant `generation: 0`, mais le vrai coordinateur (Task 6 `ConnectionActor`) enverra la génération réelle.

**Impact :** Après une recovery, le publisher reste suspendu indéfiniment. Toutes les publications en attente restent en replay sans jamais partir.

**Fix :** Changer `<=` en `<`. Ajouter un test : `Recovering { generation: 3 }` puis `Ready { generation: 3 }` doit réussir.

### #3 — `SubscriptionId` n'a pas d'accesseur → consumer tag corrompu

**Fichier :** `crates/rabbit-rs-core/src/consumer/set.rs:133`

```rust
format!("rabbit-rs.{:?}", subscription.id)
```

`SubscriptionId(String)` derive `Debug` → produit `rabbit-rs.SubscriptionId("orders_high")` avec le nom du struct et des quotes. Le tag AMQP contient des caractères invalides et un format unexpected.

**Impact :** Consumer tag illisible dans RabbitMQ Management, debugging difficile, potentiellement rejeté par certains brokers stricts.

**Fix :** Ajouter `pub fn as_str(&self) -> &str { &self.0 }` à `SubscriptionId` et remplacer `{:?}` par `{}` via `as_str()`.

### #4 — `source_errors` non borné — fuite mémoire

**Fichier :** `crates/rabbit-rs-core/src/consumer/actor.rs:56`

```rust
source_errors: VecDeque<ConsumerError>,  // pas de limite
```

Si un stream produit des erreurs en continu (connexion flappy) sans waiters pour les consommer, la mémoire croît indéfiniment. Le design exige « buffers bornés » partout.

**Impact :** OOM en production sur une connexion instable.

**Fix :** Borner à `max_in_flight` ou une constante (ex. 64). Quand la limite est atteinte, drop les anciennes erreurs ou mettre le stream en pause.

### #5 — `RuntimeRegistry::acquire` peut bloquer indéfiniment

**Fichier :** `crates/rabbit-rs-core/src/runtime.rs:111`

`close_state` → `state.take()` drop le `Runtime` Tokio sous le `Mutex<Option<ProcessState>>`. Si des tâches (ConnectionActor, ConsumerActor) tournent encore, le drop du runtime bloque. Pas de `shutdown_timeout()` explicite.

**Impact :** Après un fork, le processus enfant peut se bloquer au premier `acquire()` si des tâches du parent tournent encore. Deadlock possible.

**Fix :** Utiliser `runtime.shutdown_timeout(Duration::from_secs(1))` avant de drop, ou séparer le drop hors du Mutex via `std::mem::take` + spawn un thread pour join.

---

## Bugs modérés (MEDIUM)

### #6 — Deadline hardcoded 30 s pour delayed release

**Fichier :** `crates/rabbit-rs-core/src/consumer/actor.rs:363`

```rust
tokio::time::Instant::now() + Duration::from_secs(30)
```

Non configurable. Si le broker est lent pendant recovery, le delayed release échoue par timeout sans retry.

**Fix :** Remplacer par `state.config.publish_deadline` ou dériver du `confirm_timeout` du publisher.

### #7 — Delivery perdue si waiter droppé

**Fichier :** `crates/rabbit-rs-core/src/consumer/actor.rs:170`

```rust
if waiter.send(Ok(item)).is_ok() {
    self.in_flight = self.in_flight.saturating_add(1);
}
```

Si `send` échoue (waiter annulé côté Laravel), le message sort du buffer sans ack. Il reste unacked côté broker jusqu'à la prochaine déconnexion.

**Fix :** Si `send` échoue, re-pousser le `TransportDelivery` dans le buffer de la subscription et `mark_ready`.

### #8 — `ConsumerSet::spawn` sans rollback

**Fichier :** `crates/rabbit-rs-core/src/consumer/set.rs:121`

Si `set_qos` réussit pour la subscription 1 mais `consume` échoue pour la 2, les canaux déjà configurés restent ouverts. Pas de nettoyage.

**Fix :** En cas d'erreur, fermer tous les canaux déjà ouverts avant de propager l'erreur.

### #9 — URI avec credentials passée à Lapin

**Fichier :** `crates/rabbit-rs-core/src/transport/lapin.rs:39`

`Connection::connect(uri.as_str(), ...)` où l'URI contient `user:password@host`. Si Lapin log cette URI (error path, debug), le mot de passe fuite. Le design interdit « URI complète » dans les logs.

**Fix :** Construire l'URI sans credentials et passer `Credentials` séparément via les `ConnectionProperties` de Lapin, ou utiliser une URI opaque et logger uniquement `host:port/vhost`.

### #10 — `PublishRequest::new` default `mandatory: false`

**Fichier :** `crates/rabbit-rs-core/src/transport.rs:186`

L'acteur override à `true`, mais l'API transport permet de publier sans mandatory. Un appel direct au transport (bypass acteur) perd le routage mandatory.

**Fix :** Soit `mandatory: true` par défaut, soit marquer le constructeur `pub(crate)` et exiger un builder.

### #11 — Pas de `Reject` sur `Delivery` Rust

**Fichier :** `crates/rabbit-rs-core/src/consumer/delivery.rs`

`Settlement` n'a que `Ack` et `Release(Duration)`. `reject(requeue=false)` (discard) n'est pas implémenté. L'API PHP Task 13 (`reject(bool $requeue)`) le requiert.

**Fix :** Ajouter `Settlement::Reject { requeue: bool }` et implémenter dans `settle`.

### #12 — `DeliveryToken::settle` peut boucler sur erreur transport

**Fichier :** `crates/rabbit-rs-core/src/consumer/delivery.rs:173`

Une erreur transport (non-stale) remet l'état à `Pending`, permettant un retry immédiat. Si la génération n'est pas encore updatée, le retry hit la même connexion morte → boucle jusqu'à ce que `UpdateGeneration` arrive. Pas de backoff ni de limite.

**Fix :** Compter les retries et retourner `ConsumerErrorKind::Transport` après N tentatives, ou attendre un `UpdateGeneration` avant de permettre un nouveau `settle`.

---

## Bugs mineurs (LOW)

### #13 — `effective_priority` divise des nanos — `scheduler.rs:161`

`as_nanos()` retourne `u128`. La conversion `i64` est gérée, mais avec `starvation_after = 30s`, le premier step n'arrive qu'après 30 s. Acceptable mais pas réglable depuis la config.

### #14 — `ConnectionKey` contient un hash du password — `config.rs:341`

SHA-256 inclut le password. Le `ConnectionKey` dérive `Debug`. Pas une fuite directe, mais si la clé est exposée dans des métriques, un attaquant connaissant l'algo pourrait tenter un rainbow table. Risque faible.

### #15 — `publish_properties` encode tous les headers en `LongString` — `transport/lapin.rs:405`

Même les booléens et entiers sortent en `LongString` (via `to_string().into_bytes()`). La relecture via `map_header_value` les décode en string. Round-trip perd le type original.

---

## Divergences plan ↔ code (Architecture)

### A1 — `SchedulerConfig` vs design

Le design place `max_in_flight` dans la clé `scheduler` (`config/rabbit-rs.php`), mais le code Rust le met au niveau `WorkerProfile`. Le normalisateur Laravel devra traduire — risque de confusion.

### A2 — `Settlement` n'expose pas `Reject`

L'API PHP planifiée (Task 13) a `reject(bool $requeue)`, mais le Rust n'a que `Ack` et `Release(Duration)`. `reject(false)` (discard sans requeue) n'existe pas encore.

### A3 — `AttemptsResolver` default = aucune limite

`Default::default()` donne `max_attempts: None`. Le design dit « delivery limit à 20 sauf policy externe ». Le défaut devrait être `NonZeroU32::new(20)`.

### A4 — Jitter à 50 % au lieu de 20 %

Le design dit « jitter 20 % », mais `EqualJitter` retourne 50–100 % du délai. Dérive silencieuse.

### A5 — `starvation_after` absent de `SubscriptionConfig`

`SubscriptionPolicy::new()` panic si `starvation_after` est zéro, mais `SubscriptionConfig` ne fournit pas cette valeur. Le binding config→runtime devra combler ce gap ou panic.

### A6 — `Delivery` n'expose pas l'AMQP `message_id`

Voir bug #1.

---

## Plan d'implémentation

### Phase 1 — Fixes HIGH (avant tout travail sur le Milestone B)

| # | Tâche | Fichiers | Test requis | Effort |
|---|-------|----------|-------------|--------|
| 1 | Étendre `Delivery` transport avec `message_id` + `correlation_id`, extraire dans Lapin, propager au `Delivery` public | `transport.rs`, `transport/lapin.rs`, `consumer/delivery.rs`, `consumer/actor.rs` | `delivery_attempts.rs` : vérifier `message_id` stable après redelivery | 2 h |
| 2 | Changer `<=` en `<` dans `handle_connection_event` Ready | `publisher/actor.rs:408` | `publisher_recovery.rs` : `Recovering { gen: 3 }` puis `Ready { gen: 3 }` réussit | 15 min |
| 3 | Ajouter `SubscriptionId::as_str()`, remplacer `{:?}` par `as_str()` dans le consumer tag | `consumer/scheduler.rs`, `consumer/set.rs` | `consumer_semantics.rs` : tag ne contient pas `SubscriptionId(` | 15 min |
| 4 | Borner `source_errors` à `max_in_flight.max(64)` | `consumer/actor.rs` | `consumer_semantics.rs` : 100 erreurs consécutives n'augmentent pas la mémoire au-delà de la borne | 30 min |
| 5 | Appeler `shutdown_timeout` avant de drop le runtime, hors du Mutex | `runtime.rs` | `runtime.rs tests` : fork avec tâches en cours ne bloque pas | 1 h |

### Phase 2 — Fixes MEDIUM (pendant Milestone B)

| # | Tâche | Fichiers | Effort |
|---|-------|----------|--------|
| 6 | Configurer la deadline du delayed release | `consumer/actor.rs`, `consumer/set.rs` | 30 min |
| 7 | Re-pousser la delivery si waiter droppé | `consumer/actor.rs` | 30 min |
| 8 | Rollback des canaux ouverts en cas d'échec de `spawn` | `consumer/set.rs` | 45 min |
| 9 | Passer les credentials hors URI à Lapin | `transport/lapin.rs` | 1 h |
| 10 | `mandatory: true` par défaut sur `PublishRequest` | `transport.rs` | 15 min |
| 11 | Ajouter `Settlement::Reject { requeue: bool }` | `consumer/delivery.rs`, `consumer/actor.rs` | 1 h |
| 12 | Compteur de retries sur `DeliveryToken::settle` | `consumer/delivery.rs` | 45 min |

### Phase 3 — Fixes LOW + corrections de dérive (backlog)

| # | Tâche | Effort |
|---|-------|--------|
| 13 | Documenter le calcul aging dans le scheduler | 15 min |
| 14 | Ne pas hasher le password dans `ConnectionKey` (ou ne pas exposer en Debug) | 30 min |
| 15 | Encoder les headers selon leur type AMQP original | 1 h |
| A1 | Aligner `max_in_flight` entre config design et code Rust | 30 min |
| A3 | Défaut `AttemptsResolver` à 20 | 15 min |
| A4 | Corriger `EqualJitter` à 20 % | 15 min |
| A5 | Ajouter `starvation_after` à `SubscriptionConfig` | 30 min |

### Vérification finale

Après Phase 1 :
```sh
rtk cargo fmt --all
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
rtk cargo test -p rabbit-rs-core
rtk ./scripts/check.sh
```

Après Phase 2 :
```sh
rtk cargo test -p rabbit-rs-core --test consumer_semantics --test publisher_recovery --test delivery_attempts
rtk ./scripts/check.sh
```

### Ordre suggéré

1. **#2** (15 min, impact maximal, fix trivial)
2. **#3** (15 min, fix trivial)
3. **#1** (2 h, impact structurel mais nécessaire avant Milestone B)
4. **#5** (1 h, sécurité fork)
5. **#4** (30 min, fuite mémoire)
6. Phase 2 en parallèle avec le Milestone B
7. Phase 3 en backlog

---

## Risques non couverts par cet audit

- **Tests d'intégration réelle manquants** : le Milestone A utilise uniquement le mock transport. Les bugs #1, #2 et #9 ne se manifesteront qu'avec un vrai broker (Milestone D).
- **PHP extension non écrite** : l'API PHP (Task 13+) révélera d'autres gaps (types, conversion, lifecycle).
- **Pas de fuzzing** : le scheduler, le batcher et le ledger de confirms sont des cibles idéales pour des tests de propriétés (proptest/quickcheck).
- **Pas de benchmark** : les valeurs de batch/prefetch sont arbitraires. Le Milestone E doit les calibrer.
