# Round 2 — Fiabilité consumer : stall ack-pipeline, livraisons pre-fill, Pool::clear × consumer

Date: 2026-08-30
Base: main (b3294c1, PR #35 mergée)
Précédent: `2026-08-29-post-pump-simplification.md` (exécuté, mergé)
Roadmap: `docs/plans/ROADMAP.md` — round 2 = priorité ; round C (harnais integration
local) et round D (gap dispatch) parkés.

## Contexte

La campagne perf-gap (pump v2 + post-pump) a établi les fenêtres de performance :

- publish fire-and-forget : goopil **leader** (2,46× vladimir au niveau framework,
  ~2,8× au transport) — rien à optimiser ;
- publish safe (contrat at-least-once) : 0,31× framework / 0,28× transport — la
  frontière, cible du round D ;
- consume : **leader ~5,9×** au transport ; ~4× au niveau framework MALGRÉ la taxe
  stall (worker goopil 10 030/s médian vs 27 073/s sur rounds sains, batterie E2).

Trois défauts consumer, observés au niveau driver par le harnais Phase E
(`benchmarks/driver-bench/README.md` § Known ext-rabbit_rs consumer quirks, avec
reproductions documentées), plombent le chemin worker. Aucun n'est une perte
at-least-once (les messages restent `ready` côté broker) mais tous dégradent la
latence et le débit réels d'un worker Laravel.

## Bugs (par priorité)

### P1 — Stall de la pipeline d'ack en pop+ack soutenu (taxe 2,70×)

Le consumer cesse de recevoir des livraisons pendant un drain pop+ack unitaire
soutenu alors que les messages restent `ready` (indépendant du prefetch : observé
avec 1 et 64 ; indépendant de la purge). Le runner le détecte (400 pops null
consécutifs) et reconstruit la connexion — ~0,6-0,8 s par stall, facturé dans le
temps mesuré, `stall_recoveries` reporté par round. 9 rounds sur 10 affectés dans
la batterie E2.

Investigation root-cause D'ABORD : l'exploration préalable de code
(`consumer/actor.rs`, `composite.rs`, `set.rs`, `delivery.rs`) n'a pas identifié le
mécanisme. Le harnais driver-bench reproduit à volonté (~stall en pop+ack soutenu)
— debug favorable. Zones suspectes : ingestion des `basic.deliver` asynchrones vs
état du buffer `next()`, ré-armement du crédit/prefetch, interaction ack token →
`try_settle` → CAS (delivery.rs).

Livrable : root-cause documentée + fix + test de non-régression au niveau core
(pause de temps Tokio + transport mock scriptable, pas de sleep réel).

### P2 — Consumer créé avant l'ingestion du fill : livraisons manquées (~2 %)

Si le consumer natif est créé avant que le fill ait été ingéré (premier `pop()`
pendant que le fill est en vol, ou consumer laissé en place entre deux rounds), une
fraction des messages ne remonte jamais sur cette connexion (reproductible ~2 %).
Vérifié : consumer créé avant le fill → ~2 % manqués ; créé après → clean. Les 3
drivers concurrents (amqplib, amqp-ext, bunny) ne présentent pas le comportement.
Les messages restent `ready` (pas une perte), mais un worker réel qui démarre
pendant une publication en cours peut mourir de faim.

Livrable : root-cause + fix + test core de la course création-consumer × backlog
en vol + levée des workarounds du runner (reconnect par round, garde round-0) si le
fix le permet.

### P3 — `Pool::clear()` × consumer préexistant : pops dégradés ~25×

Combinaison observée une fois (première attribution au pattern P2, combinaison
méritant un test dédié côté core). `Pool::clear()` (fork recovery, invalidation) en
présence d'un consumer préexistant dégrade les pops ~25×.

Livrable : test core dédié de la combinaison ; fix si confirmé ; sinon documenter
la séquence correcte (clear → consumer) et refermer.

## Scope secondaire (items parkés roulés dans le round)

1. **Test contrat closed-pump batch fail** (`client.rs:143-147`) : batch doit
   échouer immédiatement et re-buffer (sémantique superset) — ~10 lignes, parké
   depuis la Phase B.
2. **`scripts/lib-extension.sh` rebuild-on-change** : l'artifact stalé est toujours
   utilisé s'il existe (le fix D2 couvre build-on-miss + warning uniquement) —
   rebuild quand Cargo.toml/lock changent.
3. **Test symétrique flush_blind** (`blind_pump.rs`) : le frère blind du test flush
   non-vacue D2.
4. **shellcheck `scripts/test-integration.sh`** (bash -n seul aujourd'hui).
5. **Validation unicité des noms de subscriptions** (préexistant,
   `update_generation` sans appelant production).

## Re-bench (critère de sortie)

Protocole Phase E complet (100 runs, 4 conditions × 2 modes × 10 rounds,
interleavé, release, JSON archivés) après les fixes. Attendus :

- `stall_recoveries = 0` sur tous les rounds worker ;
- worker goopil proche du round clean (27 073/s) — gap vs vladimir à re-mesurer ;
- 0 pertes / 0 late partout (invariant intangible) ;
- dispatch inchangé dans la variance (les fixes ne touchent que le consumer).

Comparaison systématique vs archives E2 (`runs/phase-e/` du workspace SDD — copier
la référence avant suppression du workspace).

## Protocole

- TDD : test rouge par bug → fix minimal → vert → invariants existants verts.
- Gates : `rtk ./scripts/check.sh` ; `./scripts/test-extension.sh` (rebuild
  `--features extension-tests` AVANT, sinon faux échecs testing_pool) ;
  `./scripts/test-laravel.sh`. Benchs release uniquement, runs interleavés.
- Contrat intangible : at-least-once (Safe/Unsafe) ; Blind = fire-and-forget
  documenté ; pas d'unsafe Rust ; acks connection-generation-aware.
- SDD : brief fichier → implémenteur → reviewer → ledger
  `.superpowers/sdd/2026-08-30-consumer-stall-and-reliability/`. Jamais de fix hors
  review.

## Ordre d'exécution

1. Investigation root-cause P1/P2 (lectures parallèles possibles, fixes séquentiels
   par risque — P1 d'abord : plus grosse taxe mesurée).
2. P3 (test dédié, fix si confirmé).
3. Scope secondaire (items 1-5, indépendants).
4. Re-bench + comparaison E2.
5. Review finale + PR.

Chaque étape validée explicitement avant la suivante.
