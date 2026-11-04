# Domain Definition: Agent Sessions

## Résumé

Le domaine couvre la conduite d'un LLM comme agent de travail durable : une session conserve son historique, un espace
de travail programmable (scratchpad Rhai), des mémoires et une todo ; elle découvre à la demande les outils, skills et
sous-agents que sa définition d'agent lui accorde. Une librairie Rust pure, la crate `lymaille`, réalise ce domaine. Elle est destinée à
remplacer l'inférence actuelle de vanyline et à fournir à miryad-core des étapes de workflow pilotées par un LLM.

Statut du document : cadrage initial complet au 2026-10-24. Les points restants sont marqués `TBD`, dans les Open Issues
ou dans la Solution concernée.

Avancement de la réalisation au 2026-11-01 : les sept Solutions ont leurs specs `.sdd`, implémentées et validées (`session`, `context-window`, `scratchpad`, `agents`, `toolsets`, `modules`, `delegation`, plus la boucle de tour dans `src/runtime/` ; voir la section « Réalisation SpecDD » de chacune). Les pièces d'assemblage qui manquaient au 2026-10-31 sont livrées : la restauration de session de bout en bout (`src/runtime/restore.rs`, lot S1), l'adaptateur `rig` (`src/agents/rig.rs`, feature Cargo `rig`, lots R0 à R3), le manifeste de projet (`src/agents/manifest.rs`, lot M1), le prompt de résumé par défaut (`src/context/strategy.rs`, lot P3) et l'essai de bout en bout du rejeu du raisonnement, clos par la décision du 2026-11-01 dans `context-window`. Le module de création de toolsets (`toolsets` FR-8) reste à l'hôte (décision du 2026-10-31) ; la librairie en tient la règle d'accord et le nom réservé. La version 0.1.0 est publiée.

Lot 0.2.0 (2026-11-03) : cinq issues des hôtes (vanyline, miryad-core, `sebt3/lymaille#1` à `#5`) sont traduites en exigences : la réponse vide n'est plus un tour valide et le raisonnement d'une réponse écartée est conservé (`session` FR-16), l'erreur d'un appel de fonction MCP au mauvais type d'argument montre la syntaxe attendue (`modules` FR-12, `scratchpad` FR-14), les identifiants de ContextType acceptent `_` et les majuscules (`toolsets` FR-20), le niveau intermédiaire des fournisseurs, profils et ToolsetContext s'appelle `Shared` et non plus `Project` (rupture d'API, d'où la 0.2.0), et le contexte de travail, non l'agent, est le point de départ d'une session (BO-3, BR-9, `agents` FR-5). Les Solutions concernées portent le détail. Deux décisions du 2026-11-04 complètent le lot : le ContextType porte, en option, l'agent principal par défaut et la nature de session (`toolsets` FR-21), et la Memory suit le niveau `Shared` (`modules` FR-2, FR-4, sans alias).

Avancement au 2026-11-04 : le lot 0.2.0 est livré. Les cinq issues `sebt3/lymaille#1` à `#5` sont réalisées par des specs `.sdd` implémentées et validées : réponse vide et raisonnement conservé (`session`, `context-window`), ligne d'usage des fonctions MCP (`modules`, `scratchpad`), identifiants de ContextType (`toolsets` FR-20), niveau `Shared` des fournisseurs, profils, ToolsetContext et de la Memory (rupture d'API, sans alias), et ContextType qui porte l'agent par défaut et la nature de session avec l'aide `resolve_start` (`toolsets` FR-21). Reste, hors blocage : la version 0.2.0 du crate à publier, et une question ouverte non bloquante sur le nom d'agent à espace de bord (`toolsets`, Questions ouvertes).

## Objectif et problème

Les solutions d'agent usuelles chargent des dizaines de milliers de tokens de définitions d'outils au démarrage, perdent
leur état à la compaction et lient l'outillage à un environnement unique. Ce domaine vise des sessions dont le point de
départ est petit et constant, dont l'état de travail survit à la compaction et au redémarrage, et dont l'outillage se
réutilise d'un environnement cible à l'autre.

## Objectifs métier

- **BO-1:** Une seule librairie sert les sessions interactives (vanyline, agent Matrix) et les étapes LLM autonomes d'un
  workflow (miryad-core).
    - **Source:** user instruction, 2026-10-24

- **BO-2:** Le contexte initial du LLM est connu, petit et constant ; les capacités se découvrent à la demande.
    - **Source:** user instruction, 2026-10-24

- **BO-3:** Un même outillage (fonctions et skills) s'utilise sur plusieurs environnements cibles sans être
  réécrit. Une session démarre sur un *contexte de travail* (le projet, le bac à sable, l'écran d'où la conversation
  s'ouvre), pas sur le choix d'un agent : comme on choisit un répertoire dans un outil de développement, non une
  personnalité. L'agent principal découle de ce contexte, et les limites de la session sont posées avant son début par ce
  contexte, non par une approbation en cours de session.
    - **Source:** user instruction, 2026-10-24 ; reformulé, `sebt3/lymaille#4` et `#5`, 2026-11-03 (hôtes vanyline et
      miryad-core) : le texte d'origine disait que l'utilisateur choisit l'agent et l'environnement au démarrage

- **BO-4:** Une session longue reste exploitable : son état de travail survit à la compaction et à la reprise, et un
  humain peut suivre le travail en cours.
    - **Source:** user instruction, 2026-10-24

## Périmètre et non-objectifs

Le domaine couvre : le cycle de vie et la persistance de session, la gestion de la fenêtre de contexte, le scratchpad,
les modules de travail (todo, Memory, MCP), les toolsets et leurs skills, la délégation à des sous-agents, les watchers,
et la définition des agents et des projets.

Hors périmètre, par décision :

- le stockage (la librairie définit des traits, l'hôte les implémente) ;
- toute interface utilisateur ;
- le moteur de workflow (offert par miryad-core ; le module de gestion de workflows est fourni par l'hôte) ;
- la détention durable des secrets (la librairie ne les persiste ni ne les divulgue, voir CON-3) ;
- le module de connaissance (RAG) et la communication entre agents d'une même session (MessageBoard), idées réservées
  hors du premier jalon (voir `modules` et `delegation`) ; une Memory partagée entre agents a été écartée ;
- l'export de données d'entraînement (LoRA), extension réservée : seule la capture des données est exigée (voir `session`) ;
- les budgets de tokens et de temps appliqués de force (voir DEC-7) ;
- les petits modèles (SLM) : la cible est un modèle assez capable pour écrire du Rhai et assez fiable pour agir en
  autonomie ; un besoin SLM avéré rouvrira la question. Aucun modèle n'est désigné ici : les modèles disponibles changent
  plus vite que ce document, et le jeu de tâches de référence (Risques) dit si un modèle convient.

Source: user instruction, 2026-10-24

## Acteurs

- **Utilisateur humain :** ouvre une session depuis un contexte de travail (l'agent principal en découle, BR-9), converse en
  session interactive, suit la todo et les Memory.
- **Agent principal (LLM) :** conduit une session non déléguée ; deux types, l'**agent de travail** et l'**agent de configuration**.
- **Agent de travail (`Work`) :** agent productif, interactif ou batch, qui peut déléguer.
- **Agent de configuration (`Config`) :** agent interactif qui écrit des toolsets et des skills ; il ne délègue pas, n'appelle aucun serveur MCP, lit les sessions passées et inspecte un environnement de production sans pouvoir l'exécuter (BR-8).
- **Sous-agent (LLM) :** exécute une tâche déléguée de façon autonome.
- **Hôte :** application qui embarque la librairie (vanyline, miryad-core, application de réception d'alertes avec agent
  Matrix). Il implémente les traits de stockage, résout les références vers les connexions, alimente les événements.
- **Agent de développement :** un LLM local écrit tout le code de la librairie. Le harnais de qualité (cargo, clippy et
  leurs configurations) est sa garde mécanique ; un relecteur final contrôle les diffs sur les fichiers du harnais.
- **Auteur de toolset :** humain ou LLM (hors de sa propre session) qui écrit des toolsets et des skills.

## État actuel et état cible

- **Actuel :** vanyline dispose d'une solution d'inférence propre ; miryad-core, qui offre le moteur de workflow, n'a
  pas d'étape LLM. L'analyse d'impact sur vanyline se fera depuis son propre workspace, une fois la librairie
  existante ; elle n'est pas un objet de ce domaine.
- **Cible :** miryad-core, moteur de workflow, utilise cette librairie pour offrir des étapes LLM ; vanyline utilise
  miryad-core et la librairie, et offre à l'agent principal un module de gestion de workflows ; une application de
  réception d'événements alertmanager s'appuie sur miryad-core et expose un agent via Matrix.
    - **Source:** user instruction, 2026-10-24

## Glossaire

Le mot « contexte » n'apparaît jamais seul dans les documents de ce domaine : on écrit soit *ToolsetContext*, soit
*fenêtre de contexte*.

- **Hôte :** voir Acteurs.
- **Session :** unité de conversation et d'état : historique, scratchpad, todo, Memory de niveau session, référence à
  l'agent et au ToolsetContext utilisés.
- **Agent :** définition (prompt système templaté, modèle et paramètres, modules et toolsets accessibles, sous-agents
  délégables). Trois natures : *Work* (interactif ou batch, peut déléguer), *Config* (interactif, écrit des toolsets, ne délègue pas, voir BR-8), *Sub* (sous-agent : batch autonome, ne délègue pas). *Agent principal* désigne indifféremment `Work` ou `Config`.
- **Module :** module Rhai exposant des fonctions au LLM (todo, Memory, un MCP, un module fourni par l'hôte).
- **Toolset :** ensemble nommé formé d'un module Rhai écrit en Rhai, de skills et d'une déclaration des paramètres et
  rôles MCP qu'il exige. Il est indépendant de l'environnement cible.
- **Skill :** objet (métadonnées de frontmatter et corps markdown) rattaché à un toolset, compatible avec le format
  `SKILL.md`, dont le corps est lu à la demande par le LLM.
- **ToolsetContext :** jeu nommé de paramètres et de liaisons de rôles MCP, unique par session, qui alimente le
  templating des prompts et des skills et décide quel MCP joue quel rôle.
- **Contexte de travail :** ce depuis quoi l'utilisateur ouvre une session et qui la borne avant qu'elle ne commence : un
  ToolsetContext, dont le type (ContextType) dit de quel genre d'environnement il s'agit. La nature de la session et son agent principal par défaut en
  découlent : le ContextType peut les porter, en option (BR-9, `toolsets` FR-21).
- **Rôle MCP :** nom logique (par exemple `grafana`) qu'un ToolsetContext lie à un serveur MCP concret.
- **ContextType :** schéma nommé de paramètres et de rôles MCP (par exemple `cluster`). Un toolset exige un ou plusieurs
  ContextType ; un ToolsetContext leur est compatible s'il fournit les champs requis.
- **Niveau (Level) :** utilisateur, intermédiaire ou session. Les trois cohabitent. Le niveau intermédiaire se nomme
  *partagé* (`Shared`) pour les fournisseurs, les profils de modèle, les ContextType et les ToolsetContext : il est partagé
  par un ensemble d'utilisateurs de l'hôte, et le mot « projet » y prêtait à confusion avec la notion de projet de
  l'application hôte (`sebt3/lymaille#5`). Il en va de même pour la Memory (`MemoryLevel::Shared`, module `memory_shared`), sans alias de l'ancien nom :
  aucun hôte n'étant encore livré, la rupture ne coûte rien (décision du développeur principal, 2026-11-04).
- **Memory :** article rattaché à un niveau, avec un identifiant, un titre (ce que la liste affiche) et un corps. Clé :
  niveau et identifiant. Remplace les notions de « fait » et de « connaissance » évoquées au départ.
- **Projet :** niveau regroupant des Memory et un manifeste d'agents principaux, de sous-agents, de toolsets, de
  ContextType et de définitions de ToolsetContext prédéterminés. « Workspace » est synonyme.
- **Scratchpad :** scope Rhai lié à la session et définitions de fonctions associées, conservés d'un appel à l'autre.
- **Cible d'inspection :** ce que `rhai_prod_inspect` montre à l'agent de configuration : un agent avec un ToolsetContext, ou une session passée. Une seule à la fois.
- **Watcher :** souscription déclarative à un événement ; sa seule action est l'injection d'une notification.
- **Fenêtre de contexte, compaction :** contenu envoyé au LLM, et réduction de son volume par résumé ou autre stratégie.

## Règles du domaine

- **BR-1 - MUST:** Le même noyau exécute les sessions interactives et les runs batch autonomes.
    - **Source:** BO-1
    - **Verified by:** `AC-4`

- **BR-2 - MUST:** Les capacités du LLM sont exactement celles que la définition de son agent lui accorde par modules et
  toolsets ; rien de ce que le LLM écrit en session (fonctions du scratchpad comprises) ne peut étendre cet ensemble.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **BR-3 - MUST:** Les valeurs propres à un environnement cible viennent du ToolsetContext et jamais de la définition
  du toolset.
    - **Source:** BO-3
    - **Verified by:** `AC-2`

- **BR-4 - MUST:** Les niveaux utilisateur, projet et session cohabitent sans collision : une Memory est identifiée par
  son niveau et son identifiant.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-5`

- **BR-5 - MUST:** Les modifications de la todo et des Memory sont observables par l'hôte, pour être projetées à
  l'utilisateur.
    - **Source:** BO-4

- **BR-6 - MUST:** Un sous-agent s'exécute de façon autonome comme un batch, sans interaction humaine et sans pouvoir
  déléguer ; seul un agent principal délègue.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **BR-7 - SHOULD:** Un toolset qui accorde une capacité d'écriture l'expose par des fonctions étroitement cadrées, et
  les fonctions d'un toolset bornent le volume de données qu'elles ramènent.
    - **Source:** user instruction, 2026-10-24

- **BR-8 - MUST:** Un agent de configuration (`Config`) n'appelle aucun serveur MCP, ne délègue pas et ne peut pas activer un
  toolset (l'activation est une action de l'hôte). Il écrit des brouillons de toolsets et de skills, lit les sessions passées
  et inspecte un environnement de production (agent et ToolsetContext, ou session passée) en lecture seule, par un
  second pad d'inspection où rien ne s'exécute pour de bon. Une session de configuration est exclue des candidatures
  d'amélioration continue (DEC-13).
    - **Source:** user instruction, 2026-10-29
    - **Verified by:** `AC-6bis`

- **BR-9 - MUST:** Le point de départ d'une session est un contexte de travail (un ToolsetContext), non un agent. La nature
  de la session et son agent principal par défaut en découlent : le type de ce contexte peut les porter (`agent-sessions/toolsets#FR-21`) et
  l'hôte lit ces champs ou appelle la résolution ;
  l'utilisateur ne choisit pas d'agent, et les limites de la session (capacités, bac à sable) sont fixées avant son début
  par cette configuration, jamais par une approbation en cours de session (DEC-2). La librairie démarre une session avec
  l'agent et le ToolsetContext qu'on lui désigne et en vérifie la compatibilité (`agent-sessions/toolsets#FR-4`) ; la
  librairie résout le nom d'agent que porte le type contre le catalogue (`agent-sessions/toolsets#FR-21`), et l'hôte décide seul quand le
  type n'en porte pas.
    - **Source:** `sebt3/lymaille#4` et `#5`, 2026-11-03 (hôtes vanyline et miryad-core) ; BO-3 ; champs du ContextType, user
      instruction, 2026-11-04
    - **Verified by:** `agent-sessions/agents#AC-1`, `agent-sessions/toolsets#AC-16`

## Exigences fonctionnelles transverses

- **FR-1 - MUST:** Le LLM interagit avec la librairie par deux tools natifs : `rhai_pad`, qui exécute du Rhai, et `skill`,
  qui liste et lit les skills sans passer par Rhai ; le contexte initial ne contient aucune autre définition de tool. Seul un agent
  de configuration reçoit un troisième tool, `rhai_prod_inspect` (BR-8).
    - **Source:** user instruction, 2026-10-24 ; tool `skill`, user instruction, 2026-10-25 ; `rhai_prod_inspect`, user instruction, 2026-10-29
    - **Verified by:** `AC-6`, `AC-6bis`

- **FR-2 - MUST:** Le LLM découvre par introspection Rhai ce qui est disponible : variables du scope, modules, fonctions
  (signature et documentation), toolsets et skills, avec une recherche par mot-clé.
    - **Source:** user instruction, 2026-10-24

- **FR-3 - MUST:** La librairie émet des événements observables : usage de tokens, changements de todo et de Memory,
  scripts exécutés, appels MCP, dépassements de délai, réessais d'inférence, cycle de vie des tours.
    - **Source:** BR-5; user instruction, 2026-10-24 (audit des scripts et des appels MCP)

- **FR-4 - MUST:** Tout run, principal ou sous-agent, peut être annulé par l'hôte.
    - **Source:** user instruction, 2026-10-24

- **FR-5 - MUST:** Un dépassement de délai produit un événement ; la librairie n'interrompt jamais un run d'elle-même
  sur le temps écoulé ou le nombre de tokens.
    - **Source:** user instruction, 2026-10-24

- **FR-6 - MUST:** Une erreur transitoire du moteur d'inférence (réseau, délai, 429, 5xx) n'interrompt pas la session : la
  librairie réessaie avec un backoff croissant plafonné à cinq minutes, sans limite de nombre, et chaque réessai produit
  un événement ; seule l'annulation par l'hôte y met fin. Une erreur permanente (400, 401, 403, 404) fait échouer le tour
  avec une erreur explicite et un événement, la session restant intacte et reprenable. Un 400 de dépassement de contexte
  est classé comme tel dans l'erreur et l'événement : il signale une taille de contexte fausse dans le profil de modèle ou
  une estimation erronée, puisque la compaction automatique doit l'empêcher (`agent-sessions/context-window#FR-2`).
    - **Source:** user instruction, 2026-10-24 ; classement du dépassement de contexte, user instruction, 2026-10-25
    - **Verified by:** `AC-8`, `AC-9`, `AC-11`

## Qualité, sécurité et contraintes

- **NFR-1 - MUST:** La librairie est testable sans LLM réel, avec un fournisseur simulé et des sessions rejouables.
    - **Source:** user instruction, 2026-10-24

- **NFR-2 - MUST:** Plusieurs sessions s'exécutent concurremment dans un même processus, avec au plus un tour actif par
  session.
    - **Source:** user instruction, 2026-10-24 (« si on fait de l'async, les store doivent être sous mutex »)

- **NFR-3 - MUST:** Le code passe sans exception injustifiée le harnais de qualité, point d'entrée `scripts/check` : formatage,
  clippy avec les groupes `all`, `pedantic`, `nursery` et `cargo` et une sélection de lints `restriction`, tous en `deny`,
  lints rustc et rustdoc, tests, plancher de couverture de 90 % des lignes et audit des dépendances. Les tests de mutation
  tournent périodiquement et avant chaque jalon. La configuration ne se contourne pas : aucun `#[allow]`, seulement
  `#[expect]` avec une raison.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-10`

- **CON-1:** La librairie est pure : elle n'effectue aucune E/S de stockage persistant et définit des traits que l'hôte
  implémente (session, Memory, ToolsetContext, définitions d'agents et de projets, toolsets, skills). Elle possède en
  revanche les connexions MCP et celles aux moteurs d'inférence.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-3`

- **CON-2:** Rust ; `rig` pour l'abstraction LLM ; `vynil-core` pour le moteur Rhai, qui peut évoluer selon les besoins
  de la librairie.
    - **Source:** user instruction, 2026-10-24

- **CON-3:** La librairie ne divulgue jamais un secret qu'elle détient (authentifications que l'hôte résout pour les
  moteurs d'inférence et les serveurs MCP) : ni dans l'état persisté, ni dans l'historique, les événements, les journaux,
  les messages d'erreur ou les sorties renvoyées au LLM. Un ToolsetContext ne contient que des références que l'hôte
  résout en endpoint et authentification. Une donnée sensible renvoyée par un serveur MCP n'est pas identifiable comme
  telle, MCP ne déclarant pas ce qui est secret : sa maîtrise relève de la conception du toolset (BR-7).
    - **Source:** user instruction, 2026-10-24 ; périmètre précisé, user instruction, 2026-10-25
    - **Verified by:** `AC-7`

- **CON-4:** En Rhai, une fonction définie par script ne voit pas les variables du scope mais reçoit ses arguments, et une
  closure ou un pointeur de fonction se sérialise sans erreur en chaîne ou en tableau qui ne se restaure pas en valeur
  appelable. Ces limites sont documentées pour le LLM et pour les auteurs de toolsets.
    - **Source:** user instruction, 2026-10-24 ; Observed in: spikes/rhai-1-25 (`scope_visibility`, `persistence`)

- **CON-5:** Les connexions aux moteurs d'inférence et aux serveurs MCP sont uniquement distantes, en HTTPS, ou en HTTP
  quand l'adresse de destination appartient à une plage privée IPv4 (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`) ;
  la librairie manipule mécaniquement les secrets qu'elles exigent. Aucun transport local (socket, entrée et sortie
  standard) n'est pris en charge. La connexion de serveurs MCP stdio du commerce relève dès lors d'un relais HTTP que l'hôte
  place sur une adresse privée, hors de la librairie : coût assumé, les applications visées n'ont pas la capacité de lancer
  un processus. Aucun délai applicatif ne borne une requête, qu'un serveur peut légitimement mettre en file : un pair
  disparu sans fermer la connexion est détecté par le keepalive TCP, et par le ping HTTP/2 quand la connexion est en h2.
    - **Source:** user instruction, 2026-10-24 (« uniquement remote, pas de socket supporté ») ; HTTP sur plage privée et
      keepalive et exclusion stdio, user instruction, 2026-10-25 ; coût du relais stdio consigné, user instruction,
      2026-10-26

- **CON-6:** Rhai est compilé avec la feature `sync`, et les appels asynchrones (MCP, moteurs d'inférence) faits depuis
  un script passent par un pont bloquant (`vynil_core::rt::block_on`, feature `rt`) : sur un runtime tokio multi-thread fourni par l'hôte il bloque
  sur place ; un fil sans runtime reçoit un runtime temporaire, et un runtime `current_thread` est refusé avec une erreur
  explicite.
    - **Source:** user instruction, 2026-10-24 ; pont de référence : vynil-core `src/rt.sdd`

- **CON-7:** L'hôte garantit qu'une session n'est active que dans un seul processus à la fois ; la librairie garantit un
  seul tour actif par session au sein d'un processus (NFR-2) et ne détecte aucun conflit entre processus. L'agent Matrix y
  satisfait par l'unicité que Matrix fournit et que miryad-core implémente ; vanyline doit traiter ce point de son côté
  (voir `session`, DEC-1).
    - **Source:** user instruction, 2026-10-24

- **CON-8:** La librairie est publiée sous licence BSD-3-Clause, comme `vynil-core`.
    - **Source:** user instruction, 2026-10-24

- **CON-9:** Rust édition 2024, version minimale 1.95.0, celle de `rig-core` 0.43.
    - **Source:** CON-2 ; Observed in: rig-core 0.43.0, `Cargo.toml`

## Paysage des Solutions

- `agent-sessions/session` : cycle de vie, persistance, restauration, annulation, événements, reprise sur erreur
  d'inférence.
- `agent-sessions/context-window` : suivi des tokens, auto-compaction, stratégies.
- `agent-sessions/scratchpad` : moteur Rhai, scope persistant, introspection, limites, rendu des sorties.
- `agent-sessions/modules` : todo, Memory, modules MCP, connaissance optionnelle, modules fournis par l'hôte.
- `agent-sessions/toolsets` : toolsets, skills, ToolsetContext, templating.
- `agent-sessions/delegation` : sous-agents, runs asynchrones, watchers.
- `agent-sessions/agents` : définitions d'agents, projets, mapping fournisseur et modèle.

Relations : `delegation` et `agents` dépendent de `toolsets` et `session` ; `scratchpad` héberge les modules de `modules`
et `toolsets` ; `context-window` opère sur l'historique de `session`. Aucune dépendance circulaire n'est prévue.

## Décisions du domaine

- **DEC-1 - Un tool d'exécution et un tool de lecture des skills**
    - **Context:** définir une trentaine de tools coûte environ 30 000 tokens dont une majorité reste inutilisée. Mais si
      les skills ne se lisent que par Rhai, le skill qui enseigne `rhai_pad` est inaccessible au modèle qui ne sait pas
      encore s'en servir.
    - **Decision:** le LLM a un tool d'exécution Rhai et un tool `skill` de lecture des skills ; tout le reste est une
      fonction découvrable par Rhai. Un skill intégré enseigne `rhai_pad`.
    - **Alternatives:** un tool natif par fonction exposée ; un seul tool `rhai_pad`, écarté le 2026-10-25 pour la raison
      ci-dessus ; deux tools de skills (liste et lecture), écarté au profit d'un seul.
    - **Consequences:** le contexte de départ reste constant ; la qualité dépend de l'introspection, des skills et des
      messages d'erreur, et d'un LLM qui connaît déjà Rhai, ce qui exclut les SLM (voir Périmètre et Risques).
    - **Source:** user instruction, 2026-10-24 ; révisé, user instruction, 2026-10-25

- **DEC-2 - Sécurité par capacités, sans couche d'approbation**
    - **Context:** un agent déclenché par une alerte agit sans humain présent.
    - **Decision:** les droits du LLM sont définis par les toolsets accordés ; aucun mécanisme d'approbation n'existe.
    - **Alternatives:** classement lecture/écriture des fonctions avec politique d'approbation côté hôte, rejeté.
    - **Consequences:** la conception des toolsets porte la responsabilité (BR-7) ; la définition d'agent est la seule
      frontière de sécurité (BR-2).
    - **Source:** user instruction, 2026-10-24

- **DEC-3 - Librairie pure, connexions MCP incluses**
    - **Decision:** stockage par traits implémentés par l'hôte (typiquement PostgreSQL) ; la librairie gère le cycle de
      vie des connexions MCP et des connexions aux moteurs d'inférence, toutes distantes, et l'hôte ne résout que les
      références.
    - **Consequences:** voir CON-1 et CON-3 ; les connexions ne sont pas persistées et se rétablissent à la restauration.
    - **Source:** user instruction, 2026-10-24

- **DEC-4 - Le run batch est l'unité de base**
    - **Decision:** un sous-agent est un run batch autonome ; une session interactive est la même mécanique avec un humain
      dans la boucle.
    - **Consequences:** miryad-core utilise un run batch comme étape de workflow ; une sortie structurée est
      nécessaire (voir `delegation`).
    - **Source:** user instruction, 2026-10-24

- **DEC-5 - Un seul ToolsetContext par session**
    - **Context:** autrement, quatre sous-agents à deux toolsets chacun demanderaient une dizaine de configurations.
    - **Decision:** un ToolsetContext unique, plat, partagé par l'agent principal et ses sous-agents.
    - **Consequences:** le vocabulaire des rôles et paramètres devient un contrat entre auteurs de toolsets.
    - **Source:** user instruction, 2026-10-24

- **DEC-6 - Une seule notion de mémoire, paramétrée par le niveau**
    - **Decision:** la Memory remplace faits de session, connaissance projet et connaissance utilisateur.
    - **Source:** user instruction, 2026-10-24

- **DEC-7 - Suivi plutôt que budgets**
    - **Context:** l'inférence visée est locale ; il n'y a pas de coût marginal à plafonner.
    - **Decision:** comptage des tokens (taux de remplissage, audit de coût) et événements de dépassement, sans coupure
      automatique (FR-5).
    - **Source:** user instruction, 2026-10-24

- **DEC-8 - Modèle asynchrone unifié**
    - **Decision:** délégation et watchers reposent sur un modèle asynchrone : la délégation rend un handle et sa fin
      est un événement ; une attente synchrone n'en est qu'un sucre.
    - **Consequences:** le pont entre rig (async) et Rhai (synchrone) est celui de `vynil-core` : Rhai compilé avec la
      feature `sync`, appels asynchrones depuis un script par un pont bloquant ; l'hôte fournit un runtime tokio
      multi-thread (CON-6).
    - **Source:** user instruction, 2026-10-24

- **DEC-9 - Volume et format de sortie : volume borné à la source, plafond mécanique, format laissé au LLM**
    - **Context:** un script Rhai rend un `Dynamic`, qui peut être une chaîne ou un objet structuré ; c'est le LLM qui
      choisit ce qu'il fait revenir dans la session.
    - **Decision:** les toolsets bornent le volume à la source (BR-7) ; la librairie plafonne en plus le rendu du tool et
      signale la troncature au modèle ; le format n'est pas une responsabilité de toolset : la librairie fournit au LLM
      l'outillage de formatage (voir `scratchpad`), en s'appuyant sur ce que `vynil-core` offre déjà.
    - **Source:** user instruction, 2026-10-24

- **DEC-10 - Noms**
    - **Decision:** la crate s'appelle `lymaille`, le tool d'exécution `rhai_pad` et le tool de lecture des skills `skill`.
    - **Consequences:** `rhai_pad` évoque la persistance du scratchpad ; le nom `lymaille` était libre sur crates.io au
      2026-10-24.
    - **Source:** user instruction, 2026-10-24

- **DEC-11 - Types de contexte nommés, vérification structurelle**
    - **Context:** un ToolsetContext unique par session doit convenir à plusieurs toolsets (DEC-5), et le vocabulaire de
      ses rôles et paramètres est un contrat entre auteurs de toolsets.
    - **Decision:** un ContextType est un schéma nommé de paramètres et de rôles ; un toolset exige un ou plusieurs
      types ; la compatibilité se vérifie structurellement, sans déclaration de conformité du contexte, dès la définition
      de l'agent ou du projet comme au démarrage de session.
    - **Alternatives:** typage nominal, écarté car chaque contexte devrait lister ses types ; schémas en ligne sans type
      partagé, écartés car le vocabulaire dériverait.
    - **Consequences:** un contexte peut satisfaire plusieurs types ; les types sont des entités nommées stockées par
      trait (voir `toolsets`) ; l'interface de l'hôte peut ne proposer que les contextes compatibles avec un agent.
    - **Source:** user instruction, 2026-10-24

- **DEC-12 - Capturer maintenant, exporter plus tard**
    - **Context:** une LoRA pourrait un jour apprendre à écrire du Rhai et le protocole de `rhai_pad` à partir de sessions réelles.
      Le besoin n'est pas avéré, mais ce qui n'a pas été conservé ne se rattrape pas : prompt tel qu'envoyé, sorties telles que
      le modèle les a reçues, signaux de qualité.
    - **Decision:** la session conserve ces données (`session` FR-11 à FR-13) ; l'export, son format, son tri et sa
      confidentialité restent une extension réservée.
    - **Alternatives:** concevoir l'export complet maintenant, écarté car il s'écrirait sans trace réelle ni essai
      d'entraînement ; ne rien conserver, écarté car la capture est la seule partie coûteuse à rattraper.
    - **Consequences:** quelques champs de plus à persister par segment ; l'export se greffera ensuite sans changer le
      format de session.
    - **Source:** user instruction, 2026-10-24

- **DEC-13 - Amélioration continue des toolsets à partir des sessions**
    - **Context:** les fonctions que le LLM définit dans le scratchpad et réutilise d'une session réussie à l'autre signalent
      un manque des toolsets. La capture (DEC-12) et le module de création (`toolsets` FR-8) existent ; il reste à les
      brancher.
    - **Decision:** la boucle est consignée comme contrat : la part de la librairie est la suffisance de la capture —
      fonctions persistées en source (`agent-sessions/session#FR-2`) et signaux de qualité par run
      (`agent-sessions/session#FR-13`) doivent permettre l'agrégation « fonctions par agent producteur, sessions à issue
      valide, contre versions de toolsets » ; le rapport d'agrégation et son ordonnancement relèvent de l'hôte (CON-1), qui
      en fera son propre sujet durable. Le raffinement est exécuté par l'agent de configuration via le module de
      `agent-sessions/toolsets#FR-8` et passe par la version sémantique (`agent-sessions/toolsets#FR-10`).
    - **Consequences:** un canal d'influence nouveau : du code né dans des sessions exposées à du contenu non fiable
      remonte vers des toolsets exécutés par des agents productifs. Un candidat n'est jamais promu tel quel : l'agent de
      configuration le réécrit, l'extraction de contrat s'y applique, et le diff est relu par un humain : un agent
      relecteur lirait les mêmes candidats issus de sessions exposées. Ce que l'agent de configuration enregistre est un
      brouillon ; l'activation d'une version pour des agents productifs est une action de l'hôte, jamais un outil du LLM
      (`agent-sessions/toolsets#FR-8`). Le relecteur reçoit le delta de capacités du diff (modules appelés, écritures) en
      plus du texte. Les sessions de nature `Config` sont exclues des candidatures (propriété de leur nature, BR-8). Seules les sessions étiquetées
      positivement par un humain (`agent-sessions/session#FR-15`) sont candidates : l'étiquette est la qualification que
      les signaux automatiques ne donnent pas. Aucun outil de rapport n'est exigé avant qu'un volume de sessions le
      justifie : une requête au stockage suffit les premiers mois.
    - **Source:** user instruction, 2026-10-26

## Risques

- **Rhai est un langage de niche.** Le modèle doit déjà le connaître, ce qui borne la cible aux modèles assez capables
  (voir Périmètre). Mitigation : introspection, skills avec exemples, messages d'erreur utiles, fonctions de
  formatage fournies. Un petit jeu de tâches de référence, versionné dans le dépôt et exécuté à la demande contre un modèle réel hors CI, mesure
  le taux de réussite.
- **Injection de prompt.** Le contenu d'alertes et de logs est non fiable et atteint des agents pouvant agir. Mitigation :
  capacités étroites (BR-2, BR-7). Les toolsets d'écriture sont à concevoir en connaissance de ce risque. Le module de
  création de toolsets n'est accordable qu'à un agent de configuration (`toolsets` FR-8, BR-8) : un agent de travail ne
  peut pas, même par un sous-agent, réécrire le code qu'il fera exécuter. Les fonctions candidates de l'amélioration
  continue (DEC-13) viennent de sessions exposées et sont du contenu non fiable : leur réécriture par l'agent de
  configuration et la relecture du diff font foi.
- **Contenu non fiable vers l'agent de configuration.** Les descriptions de tools d'un serveur MCP (modules d'inspection) et les sessions
  passées lues par l'agent de configuration sont du contenu non fiable qui atteint un agent écrivant du code pour d'autres agents.
  Mitigation : brouillons seulement, extraction de contrat, relecture humaine du diff (DEC-13), secrets déclarés masqués dans les
  sessions lues ; des données sensibles non déclarées peuvent subsister dans les valeurs de variables.
- **Dérive du vocabulaire du ToolsetContext.** Un nom de rôle ou de paramètre inventé sans déclaration casse la
  portabilité entre environnements. Mitigation : les ContextType nommés (DEC-11).
- **Communication entre agents** (idée réservée, non décidée). Tout canal par lequel un sous-agent exposé à du contenu
  non fiable parle à l'agent principal est un canal d'influence ; le résultat structuré en est déjà un. Une Memory
  partagée a été écartée pour cette raison, aggravée par les écritures concurrentes et la perte de reproductibilité d'un
  batch ; l'idée d'un MessageBoard est consignée dans `delegation`.

## Critères d'acceptation

- **AC-1:** Un agent dont la définition n'accorde qu'un toolset en lecture ne peut exécuter aucune fonction d'écriture ni
  atteindre un module non accordé, même via une fonction que le LLM a définie lui-même ; l'erreur est rendue au LLM et
  n'a aucun effet de bord.
- **AC-2:** Un même toolset lié à deux ToolsetContext différents offre la même surface de fonctions ; seuls changent les
  valeurs de paramètres, les MCP liés et les textes templatés.
- **AC-3:** Une session portant une variable et une fonction définie par le LLM, sauvegardée puis restaurée dans un
  autre processus, permet de réutiliser les deux.
- **AC-4:** Un run batch d'un sous-agent, lancé sans humain avec un fournisseur simulé, se termine par un résultat
  structuré.
- **AC-5:** Une Memory de même identifiant existe aux niveaux utilisateur et projet ; chacune se lit indépendamment.
- **AC-6:** La requête initiale envoyée au fournisseur pour un agent de travail ou un sous-agent contient exactement deux définitions de tool, `rhai_pad` et `skill`.
- **AC-6bis:** Celle d'un agent de configuration en contient exactement trois, `rhai_pad`, `skill` et `rhai_prod_inspect` ; une fonction d'un module MCP appelée dans `rhai_prod_inspect` rend l'erreur d'inspection seule, sans connexion ni appel au serveur.
- **AC-7:** Après une session utilisant un serveur MCP et un moteur d'inférence authentifiés, aucun secret n'apparaît
  dans l'état persisté, les événements, les journaux ni les messages renvoyés au LLM.
- **AC-8:** Avec un fournisseur simulé qui échoue deux fois puis répond, le tour aboutit, la session reste intacte, et
  deux événements de réessai à délais croissants ont été émis.
- **AC-9:** Avec un fournisseur simulé qui répond 404 (modèle introuvable), le tour échoue sans réessai, avec une erreur
  explicite et un événement, et la session reste reprenable.
- **AC-10:** `scripts/check` passe sur toute la base de code, et chaque lint critique du harnais se déclenche sur une faute
  volontaire, vérifié par un script (modèle : `spikes/lint-harness/check.sh`).
- **AC-11:** Avec un fournisseur simulé qui répond 400 pour dépassement de contexte, le tour échoue sans réessai avec une
  erreur et un événement classés « dépassement de contexte ».

## Open Issues

- **Décidé:** un secret déclaré peut exister et s'utiliser dans le pad (un `http::post` authentifié, par exemple) : le masquage porte sur toute sortie, jamais sur l'exécution. Une variable ou une source de fonction qui contient un secret déclaré n'est pas persistée (avertissement qui la nomme, absente à la restauration) au lieu d'une valeur masquée trompeuse (`CON-3` : ni dans l'état persisté).
    - **Source:** user instruction, 2026-11-01

- **Décidé:** le titre d'une session est posé par le LLM lui-même par `session::set_title` (module `session`, agents `Work` et `Config`) : il a tout le contexte et suit les règles de nommage de l'auteur de l'agent, sans second modèle ni requête dédiée. Le dernier qui écrit gagne (renommage humain à posteriori compris). Sans titre posé, la librairie calcule un titre par défaut (agent, contexte, date, début du premier message). Le titre est un texte non fiable : normalisé, borné à 120 caractères, masqué, rendu entre guillemets à l'agent de configuration.
    - **Source:** user instruction, 2026-11-01

- **Décidé:** l'agent de travail peut signaler sa session pour analyse (`session::flag(raison)`) ; l'agent de configuration la marque analysée (`sessions::mark_analyzed`). Les deux ajoutent une étiquette en ajout seul (`review-requested`, `review-done`), le dernier mot gagne dans l'état dérivé.
    - **Source:** user instruction, 2026-11-01

- **Décidé:** la mémoire d'un script reste bornée par collection, pas par imbrication, et le rendu complet d'une structure monstrueuse peut coûter de la mémoire ; mesuré le 2026-11-01 (`scratchpad/render.sdd` #2), accepté tel quel avec l'annulation de l'hôte comme seul garde-fou, cohérent avec l'absence de limite de durée.
    - **Source:** user instruction, 2026-11-01

- **Décidé:** le pad de workflow (`scratchpad.md`, extension réservée) est hors du périmètre 0.1 ; ses quatre questions sont tranchées dans `scratchpad.md` (nature `Workflow`, refus, kind de step distinct, authentification MCP à l'hôte).
    - **Source:** user instruction, 2026-10-31

- **Décidé:** le contexte de travail, non l'agent, sélectionne la session (BO-3, BR-9).
    - **Source:** user instruction, 2026-11-03 (`sebt3/lymaille#4`, `#5`)

- **Décidé:** le `ContextType` porte, en option, le nom de l'agent principal par défaut et la nature de session (travail ou configuration), pour que
  deux hôtes ne réécrivent pas le même lien. Ajout facultatif, sans rupture ni effet sur la version du type, livré dans la 0.2.0 (`toolsets` FR-21). La crate résout le nom contre le
  catalogue d'agents (`resolve_start`) ; l'hôte décide seul quand le type ne porte rien.
    - **Source:** `sebt3/lymaille#5` point 3 ; user instruction, 2026-11-04 (tranche le TBD du 2026-11-03)

- **Décidé:** le niveau intermédiaire de la Memory se nomme `Shared` comme celui des fournisseurs et des ToolsetContext : `MemoryLevel::Shared`, module
  `memory_shared`, texte `shared` dans les erreurs et les événements. Pas d'alias de l'ancien nom `memory_project` : aucun hôte n'est encore livré
  (`modules` FR-2, FR-4, `AC-2`). Tranche le TBD du 2026-11-03, qui excluait la Memory de la 0.2.0.
    - **Source:** `sebt3/lymaille#5` point 2 ; user instruction, 2026-11-04

- **Décidé:** la librairie n'est publiée (0.1, fin de `publish = false`) que lorsque ce DSD est complet : aucun cas d'usage n'est
  livré partiellement, hors idées non finalisées.
    - **Source:** user instruction, 2026-10-27

- **Décidé:** `syn` figure dans `allowed-duplicate-crates` de `clippy.toml` (doublon inhérent à `rhai_codegen`, syn 2, contre syn 3 pour `serde_derive`, `thiserror` et `tokio-macros`), décision du développeur principal du 2026-10-27 ; `rhai` est déclaré sans features par défaut (retire le doublon `getrandom`).

- **Décidé:** le harnais est complet (2026-10-31) : `deny.toml` (licences, avis de sécurité, sources) tourne dans `scripts/check`, les doublons de versions inévitables sont déclarés dans
  `allowed-duplicate-crates` avec une raison, et `scripts/mutants` (mutation reproductible avec autotest à canaris, report du mécanisme de
  `spikes/lint-harness`) est livré. `cargo-mutants` reste installé sur la machine de développement ; `scripts/mutants` est l'alternative cadrée par le harnais.

## Références

- Méthode : https://specdd.ai/llms.txt et le plugin DSD (`.specdd/plugins/github.com/specdd/specdd/dsd/plugin.md`).
- Langue : documents DSD et specs `.sdd` en français, règle posée dans `.specdd/bootstrap.project.md`.
- Format des skills : https://agentskills.io/specification.
