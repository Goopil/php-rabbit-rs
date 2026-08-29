# Design — Prefetch adaptatif par subscription

Date : 2026-08-29
Statut : approuvé (brainstorming)
Branche : `feat-auto-prefetch`
Périmètre : crate core Rust + package Laravel + docs. Pas de benchmark dédié dans ce jalon.

## Motivation

Le prefetch fixe par subscription ne peut pas convenir simultanément à des queues de
profils opposés :

- jobs rapides (5-20 ms) : un prefetch bas (ex. 8) laisse le pipeline se vider entre
  deux acks, le RTT réseau devient visible et le débit plafonne à `prefetch / durée_job` ;
- jobs lents (30 s) : un prefetch haut (ex. 64, défaut Laravel) maintient 64 messages
  en vol pour rien — mémoire gaspillée et rayon de crash amplifié (tout message
  non acquitté au crash doit être redelivré, donc rejoué).

Le document de design initial (`docs/plans/2026-07-30-rabbitmq-native-design.md`,
§ « Évolutions prévues ») anticipait cette évolution : *« prefetch adaptatif basé sur
EWMA, target buffer time, hystérésis et pression mémoire »*, avec le format de config
union (`'prefetch' => ['mode' => ..., ...]`) déjà présent dans la couche Laravel.
Les métriques nécessaires (latence de settlement incluant la durée du job PHP via
`reserved_at`) sont collectées depuis la V1.

## Décisions validées

1. **Signal de pilotage** : EWMA de la latence de settlement (réservation → ack,
   incluant la durée du job PHP). Prefetch cible = `target_buffer_seconds / durée_ewma`,
   borné `[min, max]`, application par hystérésis.
2. **Surface de config** : complète — `mode`, `initial`, `min`, `max`,
   `target_buffer_seconds`.
3. **early_ack / no_ack** : rejetés en validation avec le mode adaptatif (le signal
   n'existe pas : l'ack part avant le job PHP, ou n'existe pas du tout).
4. **Observabilité** : incluse — `ConsumerHandle::prefetch_stats()` + méthode PHP
   `Consumer::getPrefetchStats()`.
5. **Approche** : contrôleur pur + tick dans l'actor (approche A). Les alternatives
   (tâche contrôleur séparée, pilotage depuis PHP) sont rejetées en fin de document.
6. **Défauts adaptatifs recommandés** : `initial = 64` (continuité avec le défaut
   Laravel actuel), `min = 1`, `max = 256` (plafond prudent, l'utilisateur peut
   monter), `target_buffer_seconds = 5`. AMQP `0` (= illimité) reste refusé partout.

## 1. Configuration core (`crates/rabbit-rs-core/src/config.rs`)

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrefetchConfig {
    Fixed(u16),
    Adaptive {
        initial: u16,
        min: u16,
        max: u16,
        target_buffer: Duration,
    },
}
```

- `SubscriptionConfig.prefetch` passe de `u16` à `PrefetchConfig`.
- **Formats wire acceptés** (désérialisation custom, rétrocompatible) :
  1. entier nu : `"prefetch": 16` → `Fixed(16)` (forme émise aujourd'hui par le
     normalisateur Laravel pour le mode fixed — comportement inchangé) ;
  2. union fixed : `{"mode": "fixed", "value": 16}` → `Fixed(16)` ;
  3. union adaptive : `{"mode": "adaptive", "initial": 64, "min": 1, "max": 256,
     "target_buffer_seconds": 5}` → `Adaptive { .. }` (`target_buffer_seconds`
     désérialisé via le helper existant `deserialize_duration_seconds`).
- **Validation** (chemins d'erreur typés, convention existante) :
  - forme entier nu : chemin `workers.X.subscriptions.Y.prefetch`, message inchangé
    (« prefetch must be greater than zero ») — compatibilité avec les tests actuels ;
  - forme union : chemin `...prefetch.value` / `...prefetch.min` / `...prefetch.max` /
    `...prefetch.initial` / `...prefetch.target_buffer_seconds` ;
  - règles : fixed `value ≥ 1` ; adaptive `min ≥ 1`, `min ≤ initial ≤ max`,
    `target_buffer_seconds > 0` (plancher 1 ms) ;
  - **refus croisé** : adaptive combiné à `early_ack = true` ou `no_ack = true` →
    erreur « adaptive prefetch requires consumer acknowledgements… » avec le chemin
    de la subscription ;
  - `max ≤ 65535` garanti par `u16`.
- **Fingerprint** (digest config, config.rs:759) : hash canonique de l'enum —
  discriminant + champs (`to_be_bytes` pour les `u16`, secondes pour la duration).
  Deux configs qui diffèrent uniquement par le mode adaptatif doivent produire des
  fingerprints distincts.

## 2. Contrôleur pur (`crates/rabbit-rs-core/src/consumer/prefetch.rs`, nouveau)

```rust
pub(crate) struct AdaptivePrefetch {
    bounds: PrefetchBounds,   // min, max, target_buffer
    current: u16,             // prefetch appliqué côté broker
    ewma_ns: f64,             // EWMA de la latence de settlement
    samples: u64,
}
```

API pure (aucune dépendance async, testable unitairement) :

- `observe(&mut self, latency: Duration)` : mise à jour EWMA, α = 0.25 (constante
  interne documentée).
- `tick(&mut self) -> Option<u16>` :
  - `samples < 3` → `None` (pas assez de données) ;
  - cible = `ceil(target_buffer / ewma)`, conversion `f64 → u16` saturante,
    puis clampée `[min, max]` ;
  - changement appliqué seulement si `|cible − current| ≥ max(1, current / 4)`
    (hystérésis relative 25 %) — sinon `None` ;
  - changement retenu → `current = cible`, retourne `Some(cible)`.

Constantes internes (non configurables — YAGNI) :

- `EWMA_ALPHA: f64 = 0.25`
- `PREFETCH_TICK: Duration = Duration::from_secs(1)` (intervalle d'application)
- `MIN_SAMPLES: u64 = 3`
- hystérésis : `25 %` relatif au courant

Sémantique de la cible : `target_buffer_seconds` = quantité de *travail* (temps de
traitement) qui doit être prête en buffer. Exemples : cible 5 s, job 250 ms →
prefetch 20 ; job 10 ms → 500 → clampé à `max` ; job 30 s → 1.

## 3. Actor consumer (`crates/rabbit-rs-core/src/consumer/actor.rs`)

- `ActorState` détient un `AdaptivePrefetch` par subscription adaptative
  (`HashMap<SubscriptionId, AdaptivePrefetch>`) ; les subscriptions fixed n'ont
  pas d'entrée.
- **Observation** : nourrie uniquement par les acks authentiques — dans les
  complétions de settlement réussies de type `Ack`, pour les commandes `Settle`
  et `SettleThrough`, aux endroits où l'actor enregistre déjà
  `record_ack(token.reserved_at.elapsed())`. Exclus explicitement : releases
  (délai — la latence inclurait le délai et corromprait l'EWMA), rejects, et le
  chemin early-ack (`record_ack(Duration::ZERO)`).
- **Tick d'application** : bras supplémentaire dans le `tokio::select!` de
  `run_actor`, armé par précondition `if has_adaptive` (aucun intervalle actif
  quand aucune subscription n'est adaptative — comportement actuel préservé à
  l'identique sinon). À chaque tick (1 s) : pour chaque subscription dont
  `tick()` renvoie `Some(v)` → appliquer `set_qos(v)`.
- **`set_qos` hors chemin critique** : c'est un aller-retour réseau ; l'exécuter
  directement dans le bras de l'actor bloquerait dispatch et settlements pendant
  le RTT. Chaque application est donc effectuée dans une tâche tokio détachée
  (`tokio::spawn`), qui pousse une erreur dans `error_tx` en cas d'échec transport
  (même canal d'erreur que les settlements, capacité bornée 256, drop du plus
  ancien). Le tick lui-même est pur et instantané.
- Les ajustements sur un même canal sont rares (hystérésis 25 % + tick 1 s) ;
  pas de garde supplémentaire contre des QoS concurrentes.

## 4. Spawn, buffers et recovery

- `Subscription` (`consumer/set.rs`) : le champ `prefetch: u16` devient
  `prefetch: PrefetchConfig` + helper `effective_prefetch() -> u16` (valeur initiale
  appliquée au spawn : `value` pour fixed, `initial` pour adaptive).
- **Dimensionnement des buffers** (`spawn_with_generation`, set.rs:170-217) : la
  capacité mpsc/flume est calculée sur la somme des **max** des subscriptions
  adaptatives (et des valeurs des subscriptions fixed) quand au moins une
  subscription est adaptative ; sinon comportement actuel inchangé. La croissance
  runtime du prefetch dispose donc toujours de la capacité interne requise —
  invariant « tout est borné explicitement » préservé.
- **Recovery** (`pool/recovery_coordinator.rs:422`) : reconstruction avec la config ;
  `.prefetch(...)` devient `.prefetch(config)` (clone de l'enum). L'état EWMA est
  perdu au re-spawn : le contrôleur repart de `initial` et réapprend. Accepté et
  documenté (la recovery suit déjà l'ordre déterministe connexion → … → QoS →
  consumers).

## 5. Couche Laravel (`packages/laravel-queue`)

- `ConfigNormalizer::prefetch()` (ConfigNormalizer.php:421) :
  - `['mode' => 'fixed', 'value' => N]` → émet toujours l'entier nu `N` vers le
    natif (**zéro changement de wire** pour la forme fixed) ;
  - `['mode' => 'adaptive', ...]` → valide `min ≥ 1`, `min ≤ initial ≤ max`,
    `target_buffer_seconds > 0` (messages avec chemin exact, convention
    `positiveInt` existante, plafond 65535) puis émet l'union native vers le core ;
  - **refus croisé** : adaptive + `early_ack`/`no_ack` → erreur explicite au chemin
    de la subscription ;
  - entier nu en entrée Laravel → traité comme fixed (rétrocompat config
    utilisateur).
- `config/rabbit-rs.php` (doc bloc lignes 175-190) : documentation du mode
  adaptatif + exemple commenté (désactivé par défaut ; défaut fixed 64 inchangé).
- `README.md` : section prefetch mise à jour (table env + exemple adaptive).

## 6. Observabilité

- Nouvelle commande `ConsumerCommand::GetPrefetchStats { completed }` (oneshot).
- `ConsumerHandle::prefetch_stats() -> Vec<PrefetchStat>` avec
  `PrefetchStat { subscription, queue, mode, current, ewma }` (`ewma: Duration`,
  zéro tant que `samples == 0`). Réponse instantanée (état de l'actor), commande
  ponctuelle — pas un tick.
- Extension PHP : méthode `Consumer::getPrefetchStats()` retournant un tableau
  associatif `subscription → { mode, prefetch, ewma_ms }` (valeur zéro pour fixed).
- Aucun changement des métriques globales (`Metrics`) : pas de labels disponibles,
  le state par subscription passe par la commande dédiée.

## 7. Tests (TDD — un test focal échouant avant chaque implémentation)

- **Rust unit** (`consumer/prefetch.rs`, `#[cfg(test)]`) :
  - EWMA : convergence, α appliqué, premier échantillon ;
  - tick : pas de changement sous le seuil d'hystérésis, changement au-delà,
    clampage `[min, max]`, saturation `ceil`, moins de 3 échantillons → `None` ;
  - cas aux limites : job très rapide (clamp à max), très lent (min), cible 0
    interdite en amont.
- **Rust config** (`config.rs::tests`) : parsing des 3 formes wire + invalides
  (entier 0, union champ manquant, `min > max`, `initial` hors bornes,
  `target_buffer_seconds = 0`) ; refus croisé early_ack/no_ack ; fingerprint
  différencié par mode.
- **Rust intégration** (`crates/rabbit-rs-core/tests/`, temps tokio pausé +
  mock transport scriptable) :
  - livraisons + acks à latences scriptées → après avance de temps au-delà du
    tick, assertion de la séquence `TransportOperation::Qos { prefetch: X }` sur
    le canal mocké ;
  - sous le seuil d'hystérésis → aucune opération `Qos` supplémentaire ;
  - échec `set_qos` mocké → erreur présente dans `drain_errors()`, l'actor
    continue ;
  - set fixed existant : aucun bras de tick actif (aucune opération Qos au-delà
    du spawn) — régression.
- **PHP Pest (Laravel)** : `ConfigNormalizerTest` — fixed inchangé (régression),
  adaptive pass-through avec valeurs validées, erreurs de validation (chemins
  exacts), refus croisé early_ack/no_ack ; test Feature provider avec config
  adaptative.
- **Extension** : PHPT/reflection pour `getPrefetchStats()` (présence, forme).
- **Gate final** : `rtk ./scripts/check.sh` complet.

## 8. Documentation

- `docs/plans/2026-07-30-rabbitmq-native-design.md` : item « prefetch adaptatif »
  de « Évolutions prévues » marqué implémenté (référence au présent spec).
- Commentaires de config Laravel + README (§5).

## Alternatives rejetées

- **Tâche contrôleur séparée par ConsumerSet** : la même logique pure, mais une
  tâche tokio dédiée qui enverrait `ConsumerCommand::SetPrefetch`. Rejeté : il
  faudrait exporter les latences par subscription hors de l'actor (canal
  supplémentaire), couplage temporel fragile, plus de pièces mobiles pour un
  résultat identique.
- **Adaptation pilotée depuis PHP** (`setPrefetch()` + boucle Laravel) : rejeté —
  réaction limitée par le polling PHP, ne colle pas au design « activé par
  configuration » de l'actor Rust, surface d'API de plus à maintenir.
- **Signal composite (buffer depth + mémoire)** : rejeté pour ce jalon — l'EWMA
  du temps de job couvre le besoin, moins de paramètres à régler. Le garde-fou
  mémoire existant (`max_buffered_bytes`) reste en place.

## Limites connues (documentées, acceptées)

- L'état EWMA est perdu à la recovery (re-spawn du ConsumerSet) : réapprentissage
  depuis `initial`.
- `early_ack`/`no_ack` n'ont pas de signal exploitable : combinaisons refusées.
- Le prefetch n'a pas d'effet côté broker pour les consumers `no_ack` (sémantique
  AMQP) — de toute façon refusé avec l'adaptatif.
- Quorum queues : prefetch élevé augmente la mémoire côté broker ; le défaut
  `max = 256` et l'hystérésis bornent l'exposition.
