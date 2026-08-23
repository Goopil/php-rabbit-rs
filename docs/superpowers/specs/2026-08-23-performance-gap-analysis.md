# Analyse des écarts de performance — rabbit-rs vs cibles

**Date :** 2026-08-23  
**Branche :** `perf/improve-perf`  
**Contexte :** Post-implémentation du plan de correction de performance (11 tâches, PR #13)

## Cibles vs réalité

| Métrique | Cible | Actuel (avant fix) | Écart |
|----------|-------|---------------------|------|
| Publish msg/s (safe) | 40-60k | ~19k | 2-3x |
| Consume msg/s (manual ack) | 25-30k | ~16k | 1.5-2x |
| Consume p99 | <300ms | ~646ms | 2x |

L'implémentation a optimisé l'acteur Rust (settlement lanes, pipeline, event dispatch), mais le bottleneck est ailleurs.

---

## 1. Limitations d'implémentation (code qu'on a écrit)

### 1.1 Publish — Double clonage par message

`PublishRequest` est cloné dans `publish_batch()` (`client.rs:150`), puis re-cloné dans
`into_transport_request()` (`actor.rs:702-748`). Deux rounds d'allocations string (exchange,
routing_key, message_id, correlation_id…) × 256 par batch.

`PublishRequest` (`publisher/mod.rs:146-179`) contient :
- `Destination` avec deux `String` (heap allocs)
- `Bytes` payload (cheap, Arc'd)
- `MessageProperties` avec `message_id: String`, optional `content_type`, `correlation_id`, et `PublishHeaders`

Puis `into_transport_request()` (`actor.rs:702-748`) clone tout ça **encore** dans un `TransportRequest`.

### 1.2 Publish — `async_trait` = BoxFuture par publish

`Transport::publish()` est `#[async_trait]` (`transport/lapin.rs:144`), donc chaque appel à
`now_or_never()` alloue un `Box::pin` + vtable dispatch (`actor.rs:623`). Le fast path de
`now_or_never()` évite le runtime scheduling, mais paie quand même :
- Une heap allocation pour le `BoxFuture` (`actor.rs:623`)
- Une vtable indirection через `dyn PublisherChannel`
- Un `Arc::clone(&channel)` par publish (`actor.rs:622`)
- Un `into_transport_request()` qui clone exchange, routing_key, payload, et toutes les
  properties (`actor.rs:702-748`)

### 1.3 Publish — Waiter polling séquentiel

`publish_batch` (`client.rs:159-169`) await chaque `waiter.wait()` un par un dans une boucle
for. Les confirmations arrivent en ordre (le temps total ≈ temps pour tous les confirms, pas
256× le temps individuel), mais il y a 256 cycles await/wake par batch.

amqplib utilise un seul appel `wait_for_pending_acks(5)` qui bloque une fois pour tous les
confirms en attente — beaucoup moins d'overhead.

### 1.4 Publish — Un seul publisher actor, pas de parallélisme de channels

Le publisher actor tourne sur un seul `tokio::spawn` (`actor.rs:80`). Il traite
`Command::Publish` un à la fois via `select!`. Chaque commande déclenche `accept_publish` →
`publish_queue` avec une queue de 1 élément (`actor.rs:488-493`). L'acteur peut pipeline les
frames via `now_or_never()`, mais ne peut pas paralléliser across channels — l'architecture
utilise un channel par publisher actor.

### 1.5 Consume — `ackThrough` + `block_on` = stop-and-wait

`ackThrough` (`consumer.rs:155-157`) appelle `self.runtime.block_on(self.handle.ack_through(...))`.
Ceci parque le thread PHP jusqu'à ce que Lapin écrive le frame `basic_ack` sur le socket.

Pattern résultant : récup ~16 → ack → attend → récup ~16 → ack → attend.

amqplib pipeline en continu : `$msg->ack()` est fire-and-forget (écrit le frame, retourne
immédiatement), et `wait()` lit la prochaine delivery. Les messages flottent en permanence
pendant que les acks sont en vol.

### 1.6 Consume — Headers deep-clone par delivery

`actor.rs:232` : `let headers = Arc::new(delivery.headers.clone());` — `delivery.headers` est
un `HashMap<String, HeaderValue>`. C'est un **deep clone** du HashMap suivi d'une allocation
Arc, par delivery. C'est le clone le plus cher du hot path.

`delivery.payload.clone()` (`actor.rs:286`, `actor.rs:301`) — `Bytes` clone (Arc bump, cheap).

Le plan (Task 3, Step 15) disait d'utiliser `Arc::clone(&delivery.headers)` mais
`delivery.headers` n'est pas un `Arc` — c'est `Headers` (un HashMap). Le clone est
inévitable sans changer `TransportDelivery` pour持有 `Arc<Headers>`.

### 1.7 Consume — `nextBatch` ne retourne jamais 256

Le buffer flume est calculé à `consumer/set.rs:175-179` :

```rust
let total_prefetch: u64 = subscriptions.iter().map(|s| u64::from(s.prefetch)).sum();
let buffer_size = usize::try_from(total_prefetch).unwrap_or(usize::MAX) * BUFFER_CAPACITY_FACTOR / 2;
```

Avec prefetch=16 et `BUFFER_CAPACITY_FACTOR=3` : buffer_size = **24**.
`nextBatch(256)` ne peut jamais retourner plus que ce qu'il y a dans le buffer (~16-24).
La taille effective du batch est limitée par le prefetch, pas par le paramètre de l'API.

---

## 2. Divergences de conception vs amqplib

| Aspect | rabbit-rs | amqplib |
|--------|-----------|---------|
| Ack model | `block_on(ack_through)` — synchrone, parque le thread PHP | `$msg->ack()` — fire-and-forget, écrit le frame et retourne |
| Consume model | Polling (`nextBatch`/`tryNext`) — pas de callbacks PHP depuis Rust | Callback (`$channel->consume(callback)`) — messages push au callback |
| `no_ack` mode | **Hardcodé `false`** (`lapin.rs:246`) — non implémenté | `no_ack=true` — zéro frames d'ack, RabbitMQ auto-ack en interne |
| early_ack | 1 `tokio::spawn` par delivery (`actor.rs:238`) pour ack individuel | N/A (utilise `no_ack`) |
| Confirms | `now_or_never()` + `FuturesUnordered` — polling par future | `wait_for_pending_acks(5)` — un seul appel bloque pour tous |
| Channel parallelism | 1 actor, 1 channel par publisher | Multi-channel dans le même process |

Le modèle polling de rabbit-rs (par contrainte d'architecture : pas de callbacks PHP depuis
les threads Rust) introduit fondamentalement plus de latence que le modèle callback d'amqplib.
C'est un choix d'architecture, pas un bug.

---

## 3. Méthodologie de benchmark

### 3.1 AUTO_ACK injustement comparé

- `RabbitRsDriver.php:60` : AUTO_ACK sets `confirms=true`
- `AmqplibDriver.php:89-98` : AUTO_ACK utilise le fire-and-forget path (pas de
  `confirm_select()`, pas de `wait_for_pending_acks`)

rabbit-rs fait publisher confirms + early_ack (spawn de tasks, tracking de confirms) tandis
qu'amqplib fait fire-and-forget publish + `no_ack=true` consume. amqplib a zéro overhead de
confirmation et zéro overhead d'ack.

### 3.2 Prefetch=16 limite les batches

`Config.php:30` : `PREFETCH_COUNT = 16` (equalisé avec amqplib pour fairness).

Mais ceci limite `nextBatch(256)` à ~16-24 messages par batch. Le benchmark ne peut pas
tester le cas où `nextBatch` retournerait de grands batches — il faudrait un prefetch plus
élevé pour rabbit-rs (qui supporterait un buffer plus large) ou un scénario dédié.

### 3.3 p99 est un artefact du stop-and-wait

Avec prefetch=16, le pattern est :
1. `nextBatch` retourne ~16 messages (rapide)
2. `ackThrough` → `block_on` → écrit le frame d'ack → unparke PHP (sync point)
3. RabbitMQ libère le prefetch → envoie les 16 suivants
4. Source task lit → actor dispatch → flume → PHP
5. `nextBatch` retourne les ~16 suivants

Si une étape de cette chaîne a de la latence (réseau, runtime scheduling, overhead de
`block_on`), les messages à la fin de la queue attendent le round-trip complet. Avec 10,000
messages et ~625 round-trips (10000/16), même un petit overhead par round-trip se cumule
en p99 élevé.

amqplib maintient un pipeline continu — les messages sont toujours en vol, donc le p99 est
borné par le temps de processing, pas par l'attente de round-trip.

---

## 4. Ce qui n'a pas été implémenté du plan

### 4.1 `no_ack` true mode — non implémenté

Explicitement différé par le plan (merge order step 10 : "if benchmarks prove necessity").
`transport/lapin.rs:246` hardcode toujours `no_ack: false`.

C'était pourtant le levier principal pour rattraper amqplib en auto-ack : `no_ack=true`
supprime complètement les frames d'ack (RabbitMQ auto-ack en interne), tandis qu'`early_ack`
spawn encore un task par delivery pour envoyer un ack individuel.

### 4.2 Byte budgets — soft limit uniquement

Ajouté en fix wave mais c'est un soft limit : skip `dispatch()` dans le handler `Incoming`
seulement (`actor.rs:407-409`), pas un hard gate sur tous les paths de `dispatch()`. Les autres
paths (settlement completion, `dispatch_notify`) ne vérifient pas le byte budget.

### 4.3 `now_or_never()` testé uniquement avec mock synchrone

Le mock transport (`mock.rs`) retourne toujours `Ok(receipt)` synchronously sans yield,
donc `now_or_never()` retourne toujours `Some` au premier poll. Le fast path de `now_or_never()`
n'est pas validé avec un vrai Lapin — on ne sait pas si `basic_publish` complète vraiment
synchronously ou yield dans la pratique.

Le test `pipeline_publishes_before_confirmation` (`publisher.rs:598-627`) passe de façon
identique avec l'ancien code (`.await` séquentiel) et le nouveau (`now_or_never()`) — il ne
peut pas distinguer les deux implémentations.

---

## 5. FFI et runtime

### 5.1 `block_on` par appel FFI

Chaque `block_on` parque le thread PHP, schedule le future sur le Tokio multi-thread runtime,
poll, et unparke. Overhead estimé : ~5-15 μs par appel (thread park/unpark + runtime
scheduling).

- `nextBatch` fast path (flume `try_recv`) : **pas de `block_on`** — lock-free. ✓
- `nextBatch` slow path (buffer vide) : `block_on` wrap `time::timeout(dur, handle.next())`.
- `ackThrough` : `block_on` — parque jusqu'à l'ack complet.
- `tryNext` : pas de `block_on` (lock-free `try_recv`). ✓
- `next(timeoutMs)` : `block_on` sur `time::timeout`.
- `ack()` : `block_on` — parque jusqu'à l'ack complet.

### 5.2 Runtime unique partagé

`runtime.rs:44-49` : Un seul runtime `multi_thread` par process, partagé par tous les pool
handles. Tous les actors, source tasks, settlement futures, et appels `block_on` partagent
ce runtime. Pas de création de runtime par appel. ✓

### 5.3 Publisher handle cache — utilisé

`client.rs:485-538` : `publisher()` vérifie le cache en premier. Pour le benchmark, tous
les messages vont au broker 'default', donc le handle est acquis une fois et réutilisé.
`publish_batch` (`client.rs:147`) appelle `self.publisher(broker)` une fois par groupe de
broker. ✓

---

## 6. Résumé des causes racines

| Cible | Cause principale | Localisation |
|--------|-------------------|-------------|
| Publish 40-60k | Double clonage par message (PublishRequest + TransportRequest) | `client.rs:150`, `actor.rs:702-748` |
| Publish 40-60k | `async_trait` BoxFuture allocation par publish | `transport/lapin.rs:144`, `actor.rs:623` |
| Publish 40-60k | Waiter polling séquentiel (256 await cycles par batch) | `client.rs:159-169` |
| Publish 40-60k | Un seul publisher actor, pas de parallélisme de channels | `actor.rs:80` |
| Consume 25-30k | prefetch=16 limite les batches à ~16, pas 256 | `Config.php:30`, `set.rs:175-179` |
| Consume 25-30k | `ackThrough` + `block_on` crée un point de sync stop-and-wait | `consumer.rs:155-157` |
| Consume 25-30k | amqllib pipeline ack+read, rabbit-rs les sérialise | `AmqplibDriver.php:143-164` vs `RabbitRsDriver.php:135-152` |
| Consume p99 <300ms | Stop-and-wait : ~625 round-trips pour 10k messages, la latence se cumule | `consumer.rs:155-157` + prefetch=16 |
| Consume p99 <300ms | Deep HashMap clone par delivery dans dispatch | `actor.rs:232` |
| AUTO_ACK injuste | rabbit-rs : confirms=true + early_ack (spawned acks) ; amqplib : fire-and-forget + no_ack=true | `RabbitRsDriver.php:60` vs `AmqplibDriver.php:89-98` |
| early_ack overhead | `no_ack` hardcodé false, 1 tokio::spawn par delivery pour ack | `lapin.rs:246`, `actor.rs:238` |
| `no_ack` non implémenté | Explicitement différé (step 10) — c'était le levier principal pour auto-ack | `lapin.rs:246` |

---

## 7. Pistes d'amélioration (non exhaustives)

### Court terme
1. **Implémenter `no_ack=true`** dans le transport (`lapin.rs:246`) — supprime tous les ack
   frames en auto-ack
2. **Éliminer le double clonage** — faire que `into_transport_request()` prenne ownership de
   `PublishRequest` au lieu de cloner
3. **Remplacer `async_trait`** par une impl concrète ou `impl Trait` pour éviter le BoxFuture
4. **Élever le prefetch** pour rabbit-rs dans le benchmark (il peut supporter un buffer plus
   grand grâce au byte budget)
5. **Corriger l'injustice AUTO_ACK** — rabbit-rs devrait faire fire-and-forget + no_ack comme
   amqplib dans ce scénario

### Moyen terme
6. **Async ack** — permettre à `ackThrough` de retourner avant que le frame soit écrit (fire-
   and-forget avec garantie d'ordre)
7. **Multi-channel publish** — paralléliser across channels dans le publisher actor
8. **`Arc<Headers>` dans `TransportDelivery`** — éliminer le deep clone du HashMap
9. **Batch wait** — un seul appel qui attend tous les confirms en attente (comme
   `wait_for_pending_acks`)
10. **Prefetch adaptatif** — ajuster dynamiquement le prefetch selon le taux de processing

### Long terme
11. **Réévaluer le modèle polling vs callback** — le polling introduit fondamentalement plus
    de latence. Une API async PHP (via Fiber ou Symfony Runtime) permettrait un pipeline
    continu.
12. **Zero-copy pipeline** — faire transiter les payloads en `Bytes` (Arc'd) de bout en bout
    sans clone, de l'input PHP jusqu'au socket
