# Publish Pump v2 — vrai fire-and-forget

Date: 2026-08-28
Base: `perf/publish-optimizations` (2b8a9b5) → merge PR #29 → nouvelle branche `perf/publish-pump`

## Contexte

L'ancienne extension (Goopil/php-ext-rabbit-rs, archivée) atteignait de bien meilleurs débits
publish fire-and-forget grâce à : pump dédié avec `FuturesUnordered` (2048 in-flight),
`tokio::select! biased` qui chevauche intake et drain, flume 4096, runtime multi-thread,
et un chemin PHP qui **retourne après enfilage** (jamais d'attente d'outcome).

Le chemin blind actuel de rabbit-rs passe par l'actor (`try_publish_hot` = `try_publish`) :
semaphore + oneshot + Box + HashMap + métriques **par message**, puis attente séquentielle
des outcomes dans le même `block_on` (client.rs:173-183). Le pump existant (pump.rs) est
séquentiel (1 publish lapin à la fois) et saturait (flume 1024).

Objectif utilisateur : ≥ 80-100k msg/s publish en fire-and-forget. Au-delà, inutile.

## Décisions tranchées

- A (pump v2 + routage blind) : GO sans réserve.
- B (découplage PHP/outcomes) : inhérent au routage blind vers le pump ; décidé après mesure.
  Le point de décision post-bench porte sur le diet du chemin actor (Safe/Unsafe), pas sur blind.
- Sémantique blind : backpressure = **blocage** (`send_async` sur flume bornée, comme l'ancien),
  pas erreur. Erreur transport après enfilage = perte silencieuse (vrai fire-and-forget,
  documenté). Modes Safe/Unsafe inchangés (actor, at-least-once, replay).
- Barrière de flush : port du pattern ancien (`flush_fire_and_forget`) — `Pool::flush()` en
  blind attend que tout ce qui a été enfilé soit remis à lapin.

## Contraintes globales

- `#![forbid(unsafe_code)]` intouchable ; pas d'affaiblissement des lints workspace.
- Rust 1.96.0, edition 2024. `rtk cargo fmt --all` après chaque edit Rust.
- Gate complet avant de déclarer une tâche finie : `rtk ./scripts/check.sh` (ou clippy + test
  workspace + composer validate pour les tâches non-Rust).
- At-least-once pour Safe/Unsafe inchangé. Blind = opt-in explicite, sémantique documentée.
- TDD : test focalisé en échec d'abord, implémentation minimale, re-run.
- Mock transport + temps tokio en pause pour les tests async déterministes. Pas de sleep réels.
- Pas de credentials/URIs complètes dans Debug/erreurs/métriques/logs.
- Préserver le travail non lié dans l'arbre. Commits logiques et scopés.

## Phase 0 — Housekeeping merge (sur `perf/publish-optimizations`)

### Task 1 — Restaurer le re-buffering Task 13 dans flush_publishes

Le commit d'optimisation publish (`7d3b20f`) a remplacé le re-buffering de `flush_publishes`
(pool.rs) par un appel `publish_batch` qui jette l'exception sans re-buffer. Les commentaires
pool.rs:289-290 et 309 prétendent le contraire — ils sont faux.

Référence : commit `7bc5c88` (« fix(ffi): re-buffer remaining messages on
PublishOutcome::Returned mid-flush ») — restaurer la sémantique adaptée au chemin batch :

- `Err(error)` de `publish_batch` : re-buffer **tous** les messages du flush dans
  `publish_buffer` (ordre préservé), puis lever l'exception. Exception : erreur `Closed`
  (pool en train de mourir) → pas de re-buffer, lever l'exception.
- `Ok(outcomes)` : zipper outcomes et requests par index. `Confirmed` → `publish_message_id`.
  `Returned` → re-buffer la requête concernée + lever l'exception sur la première `Returned`
  (les autres outcomes déjà résolus restent reportés). Erreur par message dans les outcomes
  (backpressure etc.) → re-buffer la requête concernée, première erreur lève.
- Les duplicatas sont permis et identifiables via `message_id` (contrat at-least-once).
- Tests : réadapter ceux du commit `7bc5c88` (voir `git show 7bc5c88`) au chemin batch.
  Les tests pool FFI tournent via `./scripts/test-extension.sh`.

### Task 2 — Cleanup docs `max_in_flight`

Champs supprimés mais docs restées (minors reportés de la review consumer-tuning) :

- `packages/laravel-queue/README.md` : sections `max_in_flight` / `BackpressureDetected`
- `packages/laravel-queue/config/rabbit-rs.php` : `RABBIT_RS_MAX_IN_FLIGHT` (ignoré par le
  normalizer)
- `docs/configuration.md`, `docs/troubleshooting.md` : références `max_in_flight`
- `benchmarks/src/Drivers/RabbitRsDriver.php` : `max_in_flight => 1024` (inoffensif, à retirer)

### Task 3 — Merge

`git merge main` dans `perf/publish-optimizations` (27 commits CI/docs), gates, push,
merge PR #29 via `gh`.

## Phase 1 — Pump v2 (branche `perf/publish-pump` depuis main post-merge)

### Task 4 — Pump v2 pipeliné (pump.rs)

Réécriture de `pump_loop` sur le modèle de l'ancien (`/tmp/php-ext-rabbit-rs/src/core/channels/
channel_publisher.rs`, `start_pump_if_needed`) :

- `flume::bounded(config.buffer_capacity)` (file d'attente, défaut 1024).
- Cap in-flight : `config.buffer_capacity.saturating_mul(2).max(128)` (défaut 2048).
- `tokio::select! { biased; }` :
  1. drain des complétions : `Some(_) = inflight.next(), if !inflight.is_empty()`
  2. intake : `maybe_job = rx.recv_async(), if inflight.len() < inflight_cap` — push du futur
     `channel.publish(request)` dans `FuturesUnordered`, puis drain non-bloquant
     (`while inflight.next().now_or_never().flatten().is_some() {}`)
  3. `else => break` (sender droppé ET in-flight vide)
- Job barrier : `PumpJob { barrier_tx: Option<oneshot::Sender<()>> }` — à la réception d'un
  barrier : drainer tout l'in-flight (`while inflight.next().await.is_some() {}`) puis répondre.
- `PublishPump::try_publish` (try_send, non-bloquant) conservé pour compat, plus utilisé par
  le chemin blind principal — voir Task 5.
- Nouveau : `PublishPump::send(request)` async — `rx.send_async(job).await` (backpressure par
  blocage) ; et `PublishPump::flush()` async — barrier + await.
- Recovery : vérifier/câbler `clear_channel()` sur événement Recovering et `update_channel()`
  sur Ready (la plomberie `ArcSwapOption` existe déjà). Sur channel `None` : jobs en queue
  ignorés (drop, sémantique blind) — pas d'erreur.
- Erreur publish en blind : log-métrique discret + drop (pas de replay, pas de waiter).

### Task 5 — Routage blind vers le pump

- `PublisherHandle` : exposer `publish_blind(request)` async → `into_transport_request` +
  `pump.send(request).await` (backpressure par blocage, pas d'erreur sauf pump fermé) +
  retourner `PublishWaiter::resolved(Confirmed)` (l'outcome n'est jamais lu en blind batch).
  Conserver `try_publish_blind` (try_send) pour les usages non-bloquants existants.
- `client.rs publish_batch` : branche blind → `publisher.publish_blind(request)` par message,
  **aucune attente d'outcome**, retour après enfilage complet. Erreurs : pump fermé uniquement.
- `client.rs publish` : branche blind → `publish_blind(...).await` (déjà sur le pump, mais
  via send bloquant au lieu de try_send + erreur).
- `pool.rs` : `flush()` en mode blind → `flush_blind()` (barrière) pour garantir « tout ce qui
  est enfilé avant flush est remis à lapin au retour » — port de `flush_fire_and_forget`.
- Modes Safe/Unsafe : **aucun changement** (actor, wait_all, replay, ledger).
- Doc : `SafetyMode::Blind` = fire-and-forget explicite ; erreur transport après enfilage =
  perte silencieuse ; backpressure = blocage du thread appelant (bounded). Mettre à jour
  `docs/configuration.md` + doc-comment de `SafetyMode`.

### Task 6 — Tests + gates

- Test pipelining : transport mock avec gate — M messages enfilés pendant que l'I/O est
  bloquée ; M ≤ cap in-flight acceptés sans blocage (le select biased draine pendant l'intake).
- Test backpressure : file pleine → `send` bloque (paused time / gate) et se débloque au drain.
- Test barrier : `flush()` retourne seulement après remise à lapin de tout l'enfilé.
- Test recovery : `clear_channel` → enfilage OK sans erreur (drop), `update_channel` → reprise.
- Test routage : blind publish_batch ne touche pas l'actor (mock : l'actor ne reçoit aucun
  command Publish en blind) ; Safe/Unsafe passent toujours par l'actor (tests existants verts).
- Gate complet + `./scripts/test-extension.sh` + `./scripts/test-laravel.sh`.

### Task 7 — Bench interleavé

- Build ext debug, RabbitMQ lab. 2 runs alternés main vs branche, queues purgées entre runs.
- Reporter F&F publish/consume, batch-confirm, auto-ack + p99 dans le ledger.
- Cible : ≥ 80k msg/s F&F publish. Sinon → point de décision (barrier par batch ? diet actor ?).

## Phase 2 — A/B optionnels (post-mesure)

- Task 8 : `worker_threads` 1 vs 2 (runtime.rs) — 2 runs, garder le meilleur documenté.
- Task 9 : ext release vs debug — 1 run chacun, décider si les benchs passent en release.
- Task 10 (contingent) : diet chemin actor (Command::PublishBatch, métriques batchées) si
  Safe/Unsafe appellent plus de débit.
