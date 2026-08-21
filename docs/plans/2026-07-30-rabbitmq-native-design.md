# Rabbit RS — Extension PHP RabbitMQ native et driver Laravel

**Statut :** validé le 30 juillet 2026

## Objectif

Construire une extension PHP écrite en Rust et un package Laravel capables de publier et consommer des jobs RabbitMQ avec un coût minimal, tout en conservant les comportements attendus de Laravel Queue. Un même worker doit pouvoir agréger plusieurs connexions, vhosts, queues et channels. Les connexions doivent être réutilisées au maximum à l'intérieur de chaque processus PHP et restaurées automatiquement après une coupure.

## Périmètre de la V1

- PHP 8.4 et 8.5.
- Laravel 12 et 13.
- RabbitMQ 4.3.x.
- Linux x86_64 et ARM64.
- Distributions glibc et musl.
- SAPIs CLI, PHP-FPM et Octane.
- Serveurs Octane : FrankenPHP, RoadRunner, Open Swoole et Swoole.
- AMQP 0-9-1.
- Quorum queues par défaut, classic queues configurables.
- Livraison at-least-once.
- Plusieurs vhosts et abonnements depuis un worker Laravel.
- Topologie gérée par la bibliothèque ou provisionnée extérieurement.
- Jobs immédiats, différés, libérés, échoués et envoyés en masse.
- Reconnexion, publisher confirms, mandatory routing, backpressure et métriques.

## Hors périmètre initial

- PHP 8.3 et versions antérieures.
- Windows et macOS comme plateformes de production distribuées.
- AMQP 1.0 et RabbitMQ Streams.
- Exactly-once.
- Partage d'une connexion TCP entre plusieurs processus OS.
- Contrôleur adaptatif de prefetch activé par défaut.
- Dashboard équivalent à Horizon.
- Benchmark SQS.

## Décisions principales

### Nomenclature

Le nom public de l'écosystème est Rabbit RS. Sa tagline est : High-performance RabbitMQ transport for PHP and Laravel, powered by Rust.

Les noms techniques sont :

- dépôt principal : rabbit-rs/rabbit-rs ;
- package PIE de l'extension : goopil/rabbit-rs-native ;
- nom interne de l'extension PHP : rabbit_rs ;
- dépendance de plateforme Composer : ext-rabbit_rs ;
- package Laravel : goopil/rabbit-rs-laravel ;
- namespace PHP natif : Goopil\RabbitRs ;
- namespace du package Laravel : Goopil\RabbitRs\Laravel ;
- crates Rust : rabbit-rs-core et rabbit-rs-php ;
- driver Laravel : rabbit-rs ;
- commandes Artisan : rabbit-rs:work et rabbit-rs:status ;
- fichier de configuration : rabbit-rs.php.

L'extension et le package Laravel utilisent une version synchronisée. Une release 1.2.0 produit donc goopil/rabbit-rs-native 1.2.0 et goopil/rabbit-rs-laravel 1.2.0. Le package Laravel exige une version compatible de ext-rabbit_rs.

### Architecture hybride Laravel

La première couche est un driver Laravel standard. La commande queue:work reste responsable de la boucle de traitement, des signaux, des événements, des limites mémoire, des timeouts et des failed jobs.

Une connexion Laravel peut référencer un profil de worker agrégé. Ce profil contient plusieurs abonnements répartis sur différents brokers et vhosts. La méthode pop du driver demande au noyau Rust le prochain message disponible dans l'ensemble du profil.

Une commande rabbit-rs:work sera ajoutée dans un second jalon. Elle pourra superviser plusieurs processus queue:work standards et leur transmettre les signaux. Elle ne réimplémentera pas la boucle Illuminate\Queue\Worker.

### Trois couches

1. rabbit-rs-core : crate Rust indépendant de PHP, contenant configuration, runtime, pool, acteurs AMQP, topologie, publication, consommation, scheduling, reconnexion et métriques.
2. rabbit-rs-php : extension ext-php-rs exposant une API PHP réduite et transportant uniquement des valeurs possédées entre PHP et Rust.
3. goopil/rabbit-rs-laravel : package Composer contenant connecteur, driver Queue, Job, configuration, commandes et intégration Octane.

Lapin est le client AMQP initial. Il utilise Tokio, gère AMQP 0-9-1, les publisher confirms et la récupération automatique. Il reste caché derrière une abstraction de transport afin de permettre son remplacement après benchmark.

## Cycle de vie du runtime

Chaque processus PHP possède exactement un registre natif. Le runtime Tokio et les sockets sont créés paresseusement après le fork. Le registre mémorise le PID ; un changement de PID invalide toutes les ressources héritées.

Une clé de connexion normalisée contient :

- ensemble d'hôtes et stratégie de sélection ;
- port et paramètres TLS ;
- identité et mécanisme d'authentification ;
- vhost ;
- heartbeat, timeouts et paramètres AMQP négociables ;
- empreinte de configuration.

Un vhost nécessite sa propre connexion AMQP. Les channels sont réutilisés à l'intérieur de cette connexion. Les channels de consommateurs restent dédiés pendant la durée de leur consumer. Les channels de publication proviennent d'un pool borné.

FPM réutilise le registre entre les requêtes d'un même worker. Octane le conserve pendant la vie du worker persistant. Deux processus ne partagent jamais le registre.

Les threads Rust ne conservent aucun zval, objet Zend, callback PHP, conteneur Laravel ou objet Request. Ils ne manipulent que des chaînes, octets, nombres et structures Rust possédées.

## Publication

Le package Laravel sérialise le job selon le format Laravel, assigne un message_id stable, puis appelle l'extension.

Le publisher natif :

1. valide et copie le payload et les propriétés ;
2. place la commande dans une file bornée ;
3. publie avec delivery_mode persistant et mandatory=true ;
4. associe les numéros de séquence aux attentes de confirmation ;
5. traite basic.return avant les confirmations ;
6. termine chaque attente seulement après ACK, NACK, retour ou timeout.

Un appel publish fiable attend sa confirmation avant de rendre la main à PHP. La méthode publishBatch transmet un tableau complet en une seule traversée FFI et constitue le chemin rapide pour Laravel bulk.

Une coupure avant confirmation rend l'état ambigu. Par défaut, la politique at-least-once conserve en mémoire du processus les publications non envoyées et ambiguës, puis les republie automatiquement avec le même message_id lorsque la connexion, la topologie et un channel avec confirms sont de nouveau prêts. La deadline originale continue de s'appliquer pendant la coupure : elle n'est jamais réinitialisée par une reconnexion.

Le publisher passe en état suspendu pendant la recovery mais continue d'accepter des commandes tant que sa capacité globale bornée n'est pas atteinte. Cette capacité couvre les commandes en attente et les confirms en vol afin qu'un acteur qui draine son canal pendant une longue coupure ne puisse pas accumuler une mémoire non bornée. Une fois la capacité atteinte, les nouvelles publications reçoivent Backpressure.

Une publication jamais écrite peut être rejouée sans ambiguïté. Une publication écrite mais non confirmée est rejouée automatiquement pour éviter toute perte silencieuse ; cela peut créer un doublon et impose donc des jobs idempotents. ACK, NACK, basic.return, erreur permanente ou expiration de deadline sont terminaux et résolvent l'attente une seule fois. Cette garantie est locale au processus : un crash du processus PHP perd le buffer mémoire ; une garantie au-delà du crash nécessiterait un outbox persistant, hors périmètre de la V1.

## Consommation multi-vhost

Un ConsumerSet possède plusieurs subscriptions. Chaque subscription référence :

- un broker et son vhost ;
- une queue ;
- un alias stable ;
- un poids de fairness ;
- une classe de priorité inter-queues ;
- une configuration de prefetch ;
- les options de topologie et, lorsqu'il est explicitement activé, de dead-lettering applicatif.

Les deliveries arrivent dans des buffers bornés. Un scheduler de type deficit weighted round-robin choisit le prochain message en respectant les poids et une politique d'aging empêchant la famine.

La priorité AMQP d'un message dans une queue est distincte de la priorité d'une subscription entre plusieurs queues.

La V1 utilise un prefetch fixe par subscription et un budget global max_in_flight par worker. Les métriques nécessaires au futur contrôleur adaptatif sont collectées dès la V1 : durée du job, temps réservé, profondeur du buffer, latence d'ACK et pression mémoire.

## ACK, retry et attempts

Chaque message rendu à PHP contient un jeton natif opaque avec l'identité de connexion, de channel, de consumer, le delivery tag et la génération de connexion.

- delete envoie basic.ack.
- release(0) envoie basic.reject avec requeue=true.
- release(delay > 0) republie vers le mécanisme de délai, attend le publisher confirm, puis ACK le message original.
- une publication différée échouée laisse l'original non acquitté.
- une fermeture de connexion réinsère automatiquement les messages non acquittés.

basic.reject est préféré à basic.nack pour une livraison unique : les quorum queues peuvent ainsi incrémenter leurs compteurs de livraison. Les headers x-acquired-count et x-delivery-count de RabbitMQ 4.3 sont utilisés avec le compteur applicatif pour implémenter attempts.

Après une coupure, un ACK portant une ancienne génération est refusé par l'extension. Le broker redélivre le message. Si le job avait terminé côté PHP avant l'échec d'ACK, son traitement peut donc être répété.

## Délais

Le driver delay est auto par défaut :

1. utiliser rabbitmq_delayed_message_exchange lorsqu'il est disponible et autorisé ;
2. sinon utiliser des files TTL avec dead-letter exchange.

Le fallback TTL utilise des buckets bornés et configurables. Les queues de délai sont déclarées paresseusement, durables lorsque nécessaire et munies d'une expiration de queue afin d'éviter une croissance illimitée de la topologie. Les délais sont arrondis au bucket supérieur afin qu'un job ne soit jamais livré avant son échéance.

## Reconnexion

La machine d'états de connexion est :

    Disconnected -> Connecting -> Ready -> Recovering -> Ready
                                   |
                                   +-> Draining -> Closed

Les retries utilisent un backoff exponentiel avec jitter et plafond. Les erreurs d'authentification ou de topologie incompatibles sont classées comme permanentes et remontées sans boucle infinie dans les contextes de publication. Un worker de consommation peut continuer à réessayer selon sa politique.

La restauration suit un ordre déterministe :

1. connexion et négociation ;
2. channels ;
3. exchanges ;
4. queues ;
5. bindings ;
6. QoS ;
7. consumers.

Les publisher confirms interrompus sont classés comme ambigus sans résoudre immédiatement l'appel, puis replacés dans le buffer borné de replay. Après une nouvelle génération, la topologie et le mode confirm sont restaurés avant leur republication. Les messages consommés mais non acquittés sont redélivrés par RabbitMQ.

## Topologie

Trois modes sont disponibles :

- declare : déclarer idempotemment la topologie et échouer en cas d'incompatibilité ;
- verify : effectuer des déclarations passives et vérifier les propriétés attendues ;
- external : utiliser la topologie sans la modifier.

Les queues créées automatiquement sont des quorum queues durables, non exclusives et non auto-delete. Classic reste configurable. Aucune DLQ applicative n'est créée par défaut : exchange, queue et bindings de dead-lettering doivent être activés explicitement ou provisionnés par l'infrastructure. Cette règle ne concerne pas le dead-letter exchange interne nécessaire au fallback des messages différés par TTL. Les policies de cluster restent de préférence gérées par l'infrastructure.

## Configuration Laravel

La configuration est séparée en quatre concepts :

- brokers : endpoints, vhosts, TLS et authentification ;
- routes : destinations utilisées pour publier ;
- topologies : exchanges, queues, bindings et délais ;
- workers : ensembles de subscriptions et politique de scheduling.

Exemple conceptuel :

    return [
        'brokers' => [
            'orders_eu' => [
                'hosts' => ['rabbit-1:5672', 'rabbit-2:5672'],
                'vhost' => '/orders-eu',
            ],
        ],

        'routes' => [
            'orders' => [
                'broker' => 'orders_eu',
                'exchange' => 'laravel.jobs',
                'routing_key' => '{queue}',
            ],
        ],

        'workers' => [
            'main' => [
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 64,
                ],
                'subscriptions' => [
                    'orders_high' => [
                        'broker' => 'orders_eu',
                        'queue' => 'orders.high',
                        'weight' => 8,
                        'prefetch' => ['mode' => 'fixed', 'value' => 8],
                    ],
                ],
            ],
        ],
    ];

Les valeurs initiales saines sont :

- confirms et mandatory activés ;
- queue quorum durable ;
- delivery limit à 20 sauf policy externe ;
- aucune DLQ applicative sans configuration explicite ;
- buffer publisher borné à 8192 commandes ;
- prefetch initial à 16, borné par max_in_flight ;
- reconnexion de 100 ms à 30 s, multiplicateur 2 et jitter 20 %.

Les valeurs de prefetch doivent être calibrées par benchmark avant la V1 stable.

## Compatibilité Laravel

Le package enregistre un driver rabbit-rs via Queue::extend. Il implémente les contrats Queue, ClearableQueue et Monitor lorsque pertinents.

RabbitMqQueue implémente push, pushRaw, later, bulk, pop, size et clear. RabbitMqJob implémente delete, release, attempts, getJobId et getRawBody.

Pour conserver queue:work sans remplacer Worker, la valeur queue de la connexion représente normalement un profil agrégé. Une sélection avancée de subscriptions et le mode multiprocessus seront fournis par rabbit-rs:work dans le second jalon.

Les événements Laravel natifs JobQueued, JobProcessing, JobProcessed, JobFailed et JobExceptionOccurred restent émis par le framework.

## Octane et FPM

Le package ne garde aucune référence à Application, Request ou Config dans des singletons persistants. Il normalise la configuration en valeurs immuables avant de créer le handle natif.

Des hooks Octane ferment proprement les ressources lors de l'arrêt ou du reload d'un worker. Une requête terminée ne détruit pas le pool natif. Les confirmations déjà attendues conservent une deadline bornée.

Le registre détecte les forks, y compris ceux qui surviennent après l'initialisation accidentelle d'un handle.

## Observabilité

Le noyau expose un snapshot sans imposer de backend :

- état des connexions et génération ;
- channels ouverts, empruntés et invalidés ;
- commandes publisher en buffer ;
- confirmations ACK/NACK/timeout ;
- messages retournés comme non routables ;
- deliveries prêtes et non acquittées ;
- ACK, reject, release et redelivery ;
- tentatives et durée de reconnexion ;
- latences de publication, confirmation, attente et traitement ;
- poids théorique et distribution effective par subscription.

Le package Laravel transforme ces données en événements et peut fournir des adaptateurs Prometheus ou OpenTelemetry ultérieurs. Les logs sont structurés et ne contiennent jamais de mot de passe, URI complète ou certificat privé.

## Validation

Quatre niveaux de tests sont requis :

1. tests unitaires Rust déterministes ;
2. tests PHPT et package Laravel ;
3. tests d'intégration sur cluster RabbitMQ ;
4. tests de chaos et benchmarks.

La propriété principale est : aucune perte silencieuse dans les scénarios at-least-once ; les doublons sont autorisés, identifiés et mesurés.

Le dépôt contient trois laboratoires :

- benchmarks/native : coût Rust, Lapin, batching, confirms et FFI ;
- benchmarks/laravel : application Laravel avec extension native, php-amqplib, driver Laravel RabbitMQ existant, Redis et database témoin ;
- lab/rabbitmq : cluster RabbitMQ 4.3 à trois nœuds, métriques et injection de fautes.

Les payloads de référence sont 256 o, 1 Kio, 10 Kio, 100 Kio et 1 Mio. Les métriques sont débit, p50/p95/p99, CPU par message, RSS, connexions, channels, temps de récupération, pertes, doublons et erreur de fairness.

Les objectifs absolus sont calibrés sur une machine de référence après le prototype, puis enregistrés avec les gains comparatifs comme budgets anti-régression.

## Distribution

La distribution optimise la simplicité pour l'utilisateur et sépare clairement le binaire système du code Laravel.

### Extension native

Le dépôt principal est enregistré sur Packagist comme package goopil/rabbit-rs-native de type php-ext. Son composer.json racine déclare extension-name = rabbit_rs, Linux uniquement, support NTS et ZTS, et download-url-method = pre-packaged-binary.

L'installation publique est :

    pie install goopil/rabbit-rs-native

PIE remplace PECL comme canal principal. Il sélectionne le bon binaire selon la version PHP, l'architecture, la libc et le mode NTS/ZTS, installe le fichier partagé et active l'extension dans la bonne configuration PHP.

La CI produit 16 archives de release :

- PHP 8.4 et 8.5 ;
- x86_64 et ARM64 ;
- glibc et musl ;
- NTS et ZTS.

Les builds debug ne sont pas distribués. Chaque archive suit exactement la convention de nommage PIE, par exemple :

    php_rabbit_rs-1.2.0_php8.5-x86_64-linux-glibc-nts.zip

Les dépendances Rust et TLS sont liées statiquement autant que possible ; libc reste la seule dépendance système attendue. Les builds glibc utilisent une baseline documentée et suffisamment ancienne. Chaque archive est testée avec le PHP cible, accompagnée d'un SHA-256, d'une SBOM et d'une attestation de provenance GitHub.

La compilation depuis les sources reste documentée pour les contributeurs avec Cargo et cargo-php, mais elle n'est pas le fallback PIE de la V1. Aucun paquet PECL, installateur Composer privilégié, paquet Debian/RPM/APK ou image PHP complète n'est maintenu en V1. Les Dockerfiles utilisateurs installent l'extension avec PIE.

### Package Laravel

Le package packages/laravel-queue est publié sur Packagist sous le nom goopil/rabbit-rs-laravel. Son installation est :

    composer require goopil/rabbit-rs-laravel

Il exige PHP ^8.4, Laravel 12 ou 13 et ext-rabbit_rs avec la même version majeure. Composer vérifie la présence de l'extension mais ne tente jamais d'installer ou d'activer un binaire système.

Le monorepo reste la source de développement. Une CI de subtree split publie packages/laravel-queue dans un dépôt miroir en lecture seule, puis pousse le même tag que celui de l'extension. La release GitHub native n'est publiée qu'après production et validation de tous les binaires, publication du tag miroir Laravel et vérification des deux métadonnées Packagist.

La V1 stable n'est publiée qu'après certification CLI, FPM et des quatre serveurs Octane annoncés.

## Évolutions prévues

- prefetch adaptatif basé sur EWMA, target buffer time, hystérésis et pression mémoire ;
- commande rabbit-rs:work multiprocessus ;
- exporteurs Prometheus et OpenTelemetry ;
- stratégies de routing et de failover supplémentaires ;
- éventuel backend AMQP alternatif si les benchmarks le justifient ;
- support de RabbitMQ Streams dans un produit distinct si un besoin réel apparaît.

## Sources techniques

- PHP Supported Versions : https://www.php.net/supported-versions.php
- Laravel Queue Worker : https://github.com/laravel/framework/blob/13.x/src/Illuminate/Queue/Worker.php
- Laravel Octane : https://laravel.com/docs/13.x/octane
- RabbitMQ Consumer Acknowledgements and Publisher Confirms : https://www.rabbitmq.com/docs/confirms
- RabbitMQ Quorum Queues : https://www.rabbitmq.com/docs/quorum-queues
- RabbitMQ Release Information : https://www.rabbitmq.com/release-information
- Lapin : https://github.com/amqp-rs/lapin
- ext-php-rs : https://github.com/davidcole1340/ext-php-rs
- PIE : https://github.com/php/pie
- Composer Platform Packages : https://getcomposer.org/doc/01-basic-usage.md#platform-packages
