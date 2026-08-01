# Audit de la Milestone B

**Date :** 2026-08-01  
**Périmètre :** performance, stabilité et couverture de tests de l’extension PHP native à la fin de la Milestone B.

## Verdict

La Milestone B est fonctionnellement avancée, mais elle ne doit pas encore être considérée comme stable pour la production.

- **Performance : 6/10** — architecture prometteuse, mais aucune mesure réelle ne valide encore la promesse de haute performance.
- **Stabilité : 5/10** — deux défauts concurrentiels importants peuvent affecter les garanties de livraison et le cycle de vie des ressources.
- **Couverture : 7/10** — bonne couverture déterministe, avec plusieurs angles morts critiques à traiter.

## Corrections prioritaires

### 1. Critique — une delivery peut être absorbée par un waiter expiré

`Consumer::next()` annule son futur après expiration, mais le waiter annulé peut rester dans la file. La delivery suivante peut alors être retirée du buffer puis envoyée vers ce waiter fermé, ce qui la rend invisible à PHP tout en la laissant non acquittée côté broker.

Conséquences possibles :

- blocage progressif du consumer jusqu’à une fermeture ou une redelivery ;
- saturation du prefetch après plusieurs timeouts ;
- violation pratique de l’objectif « aucune perte silencieuse ».

Fichiers concernés :

- `crates/rabbit-rs-core/src/consumer.rs` ;
- `crates/rabbit-rs-core/src/consumer/set.rs` ;
- `crates/rabbit-rs-core/src/consumer/actor.rs`.

Correction attendue : rendre les waiters annulables et garantir qu’une delivery n’est retirée du buffer que lorsqu’un destinataire actif peut la recevoir. Ajouter un test déterministe « timeout, puis arrivée d’une delivery ».

### 2. Élevé — course entre fermeture et création d’opérations

`ClientPool::publish()` et `ClientPool::consumer()` vérifient l’état de fermeture avant de créer ou récupérer leurs ressources. Une fermeture concurrente peut vider les registres entre cette vérification et l’insertion, permettant la création d’une connexion, d’un publisher ou d’un consumer après `close()`.

Conséquences possibles :

- ressources actives après la fermeture déclarée du pool ;
- comportement imprévisible sous Octane ou PHP ZTS ;
- arrêt incomplet et fuites de ressources réseau.

Fichier principal : `crates/rabbit-rs-core/src/client.rs`.

Correction attendue : rendre atomique la transition de fermeture vis-à-vis des créations de ressources. Ajouter des tests contrôlant les courses `close/publish`, `close/consumer` et la fermeture pendant une confirmation.

### 3. Élevé — mémoire non bornée à la frontière PHP

Le payload individuel est limité à 1 Mio, mais le nombre de messages, la taille cumulée d’un `publishBatch()` et le volume des headers ne sont pas bornés. Le batch est entièrement converti et copié avant que la backpressure du publisher puisse s’appliquer.

Conséquences possibles :

- pic de mémoire important ou épuisement mémoire ;
- coût CPU élevé pendant la conversion ;
- backpressure trop tardive pour protéger le processus PHP.

Fichier principal : `crates/rabbit-rs-php/src/conversion.rs`.

Correction attendue : définir et appliquer des limites explicites pour le nombre de messages, la taille totale du batch, le nombre de headers, leur profondeur et leur taille cumulée. Les erreurs doivent identifier précisément le chemin d’entrée fautif.

### 4. Moyen — durées extrêmes et arrêt potentiellement non borné

`timeout_ms` accepte des valeurs allant jusqu’à `PHP_INT_MAX`. Leur conversion ou leur addition à un `Instant` peut dépasser les limites de la plateforme. Par ailleurs, `RuntimeRegistry::close()` attend les fermetures réseau sans deadline.

Conséquences possibles :

- panique sur une durée invalide ou excessive ;
- blocage d’un arrêt ou d’un reload FPM ;
- latence opérationnelle non maîtrisée.

Fichiers concernés :

- `crates/rabbit-rs-php/src/conversion.rs` ;
- `crates/rabbit-rs-core/src/runtime.rs`.

Correction attendue : plafonner et valider les durées avant conversion, puis borner le temps d’arrêt avec une politique explicite pour les ressources qui ne se ferment pas dans le délai imparti.

## Performance

### Points solides

- runtime et connexions créés paresseusement ;
- handles réutilisés par PID et configuration normalisée ;
- files publisher et replay explicitement bornées ;
- métriques atomiques ;
- `publishBatch()` traverse une seule fois la frontière PHP et soumet les messages avant d’attendre les confirmations.

### Limites actuelles

- aucun benchmark de débit, latence p50/p95/p99, CPU ou RSS ;
- copies des payloads et headers à la frontière PHP ;
- revalidation et hash complet de la configuration à chaque construction de `Pool` ;
- mutex de publishers, consumers et connexions conservés pendant certaines opérations réseau, ce qui peut sérialiser l’initialisation de profils indépendants.

La promesse « high-performance » reste donc plausible, mais non démontrée.

## Couverture de tests

### Couverture existante

Le dernier gate complet était vert avec :

- 112 tests Rust ;
- 9 tests PHPT ;
- Clippy et formatage ;
- validation Composer ;
- laboratoire FPM à deux workers.

Les scénarios couverts incluent notamment l’API et la réflexion, les payloads binaires, les configurations invalides, les secrets, le double ACK, la backpressure, la fermeture, le fork et la réutilisation des handles sous FPM. Les fixtures mock ne sont pas exposées dans le binaire de production.

### Tests encore nécessaires

- timeout consumer suivi de l’arrivée d’une delivery ;
- courses `close/publish` et `close/consumer` ;
- fermeture pendant une confirmation publisher ;
- `release()` et `reject()` à travers l’API PHP ;
- publication réussie, retour mandatory, timeout de confirmation et `ConnectionException` depuis PHP ;
- batches et headers aux limites et au-delà des limites ;
- valeurs de durée extrêmes ;
- arrêt avec connexions et acteurs actifs ;
- validation sur PHP 8.5, Linux glibc/musl, ARM64 et ZTS ;
- cluster RabbitMQ réel, scénarios de chaos et benchmarks.

## Ordre de traitement recommandé

1. Corriger le waiter consumer annulé qui peut absorber une delivery.
2. Rendre la fermeture atomique face aux opérations concurrentes.
3. Borner les batches, headers et durées.
4. Ajouter les tests de concurrence et de shutdown.
5. Établir une baseline de microbenchmarks FFI, conversion et batch.

La Milestone B pourra être considérée comme stabilisée lorsque les quatre premiers points seront corrigés et couverts par des tests déterministes. Les benchmarks et validations multiplateformes peuvent ensuite servir de critères d’entrée pour une qualification de production.