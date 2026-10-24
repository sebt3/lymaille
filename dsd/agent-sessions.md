# Domain Definition: Agent Sessions

## Résumé

Le domaine couvre la conduite d'un LLM comme agent de travail durable : une session conserve son historique, un espace
de travail programmable (scratchpad Rhai), des mémoires et une todo ; elle découvre à la demande les outils, skills et
sous-agents que sa définition d'agent lui accorde. Une librairie Rust pure, la crate `lymaille`, réalise ce domaine. Elle est destinée à
remplacer l'inférence actuelle de vanyline et à fournir à miryad-core des étapes de workflow pilotées par un LLM.

Statut du document : cadrage initial complet au 2026-10-24. Les points restants sont marqués `TBD`, dans les Open Issues
ou dans la Solution concernée.

## Responsabilité de l'hôte sur l'unicité des sessions

La librairie garantit un seul tour actif par session au sein d'un processus (NFR-2), et rien au-delà : elle ne détecte
aucun conflit entre processus (CON-7). C'est à l'hôte de garantir qu'une session n'est active que dans un seul processus
à la fois. L'agent Matrix y satisfait par l'unicité que Matrix fournit et que miryad-core implémente ; vanyline devra
traiter ce point de son côté. Voir `session`, DEC-1.

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

- **BO-3:** Un même espace de travail (fonctions et skills) s'utilise sur plusieurs environnements cibles sans être
  réécrit ; l'utilisateur choisit l'agent et l'environnement au démarrage.
    - **Source:** user instruction, 2026-10-24

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
- le module de connaissance (RAG) et un espace de Memory partagé entre agent principal et sous-agents, idées réservées
  hors du premier jalon (voir `modules` et `delegation`) ;
- l'export de données d'entraînement (LoRA), extension réservée : seule la capture des données est exigée (voir `session`) ;
- les budgets de tokens et de temps appliqués de force (voir DEC-7) ;
- les petits modèles (SLM) : la cible est la classe de Qwen3.6 et au-delà, assez capable pour écrire du Rhai et assez
  fiable pour agir en autonomie ; un besoin SLM avéré rouvrira la question.

Source: user instruction, 2026-10-24

## Acteurs

- **Utilisateur humain :** choisit l'agent et l'environnement, converse en session interactive, suit la todo et les
  Memory.
- **Agent principal (LLM) :** conduit une session, peut déléguer.
- **Sous-agent (LLM) :** exécute une tâche déléguée de façon autonome.
- **Hôte :** application qui embarque la librairie (vanyline, miryad-core, application de réception d'alertes avec agent
  Matrix). Il implémente les traits de stockage, résout les références vers les connexions, alimente les événements.
- **Agent de développement :** un LLM (Qwen3.8) écrit tout le code de la librairie. Le harnais de qualité (cargo, clippy et
  leurs configurations) est sa seule garde.
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
  délégables). *Agent principal* : interactif ou batch, peut déléguer. *Sous-agent* : batch autonome, ne délègue pas.
- **Module :** module Rhai exposant des fonctions au LLM (todo, Memory, un MCP, un module fourni par l'hôte).
- **Toolset :** ensemble nommé formé d'un module Rhai écrit en Rhai, de skills et d'une déclaration des paramètres et
  rôles MCP qu'il exige. Il est indépendant de l'environnement cible.
- **Skill :** objet (métadonnées de frontmatter et corps markdown) rattaché à un toolset, compatible avec le format
  `SKILL.md`, dont le corps est lu à la demande par le LLM.
- **ToolsetContext :** jeu nommé de paramètres et de liaisons de rôles MCP, unique par session, qui alimente le
  templating des prompts et des skills et décide quel MCP joue quel rôle.
- **Rôle MCP :** nom logique (par exemple `grafana`) qu'un ToolsetContext lie à un serveur MCP concret.
- **ContextType :** schéma nommé de paramètres et de rôles MCP (par exemple `cluster`). Un toolset exige un ou plusieurs
  ContextType ; un ToolsetContext leur est compatible s'il fournit les champs requis.
- **Niveau (Level) :** utilisateur, projet ou session. Les trois cohabitent.
- **Memory :** article rattaché à un niveau, avec un identifiant, un titre (ce que la liste affiche) et un corps. Clé :
  niveau et identifiant. Remplace les notions de « fait » et de « connaissance » évoquées au départ.
- **Projet :** niveau regroupant des Memory et un manifeste d'agents principaux, de sous-agents, de toolsets, de
  ContextType et de définitions de ToolsetContext prédéterminés. « Workspace » est synonyme.
- **Scratchpad :** scope Rhai lié à la session et définitions de fonctions associées, conservés d'un appel à l'autre.
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

## Exigences fonctionnelles transverses

- **FR-1 - MUST:** Le LLM interagit avec la librairie par un seul tool natif, `rhai_pad`, qui exécute du Rhai ; le
  contexte initial ne contient aucune autre définition de tool.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-6`

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
  avec une erreur explicite et un événement, la session restant intacte et reprenable.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-8`

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

- **CON-2:** Rust ; `rig` pour l'abstraction LLM ; `vynil-core` pour le moteur Rhai, qui peut évoluer selon les besoins
  de la librairie.
    - **Source:** user instruction, 2026-10-24

- **CON-3:** La librairie ne divulgue jamais de secret : ni dans l'état persisté, ni dans l'historique, les événements,
  les journaux, les messages d'erreur ou les sorties renvoyées au LLM. Un ToolsetContext ne contient que des références
  que l'hôte résout en endpoint et authentification.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-7`

- **CON-4:** En Rhai, une fonction définie par script ne voit pas les variables du scope mais reçoit ses arguments, et une
  closure ou un pointeur de fonction se sérialise sans erreur en chaîne ou en tableau qui ne se restaure pas en valeur
  appelable. Ces limites sont documentées pour le LLM et pour les auteurs de toolsets.
    - **Source:** user instruction, 2026-10-24 ; Observed in: spikes/rhai-1-25 (`scope_visibility`, `persistence`)

- **CON-5:** Les connexions aux moteurs d'inférence et aux serveurs MCP sont uniquement distantes, en HTTPS ; la librairie
  manipule mécaniquement les secrets qu'elles exigent. Aucun transport local (socket, entrée et sortie standard) n'est
  pris en charge.
    - **Source:** user instruction, 2026-10-24 (« uniquement remote, pas de socket supporté »)

- **CON-8:** La librairie est publiée sous licence BSD-3-Clause, comme `vynil-core`.
    - **Source:** user instruction, 2026-10-24

- **CON-9:** Rust édition 2024, version minimale 1.95.0, celle de `rig-core` 0.43.
    - **Source:** CON-2 ; Observed in: rig-core 0.43.0, `Cargo.toml`

- **CON-6:** Rhai est compilé avec la feature `sync`, et les appels asynchrones (MCP, moteurs d'inférence) faits depuis
  un script passent par un pont bloquant qui exige un runtime tokio multi-thread fourni par l'hôte.
    - **Source:** user instruction, 2026-10-24 ; pont de référence : vynil-core `src/rt.sdd`

- **CON-7:** L'hôte garantit qu'une session n'est active que dans un seul processus à la fois ; la librairie ne détecte
  aucun conflit entre processus.
    - **Source:** user instruction, 2026-10-24

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

- **DEC-1 - Un seul tool natif**
    - **Context:** définir une trentaine de tools coûte environ 30 000 tokens dont une majorité reste inutilisée.
    - **Decision:** le LLM n'a qu'un tool d'exécution Rhai ; tout le reste est une fonction découvrable.
    - **Alternatives:** un tool natif par fonction exposée.
    - **Consequences:** le contexte de départ est constant ; la qualité dépend de l'introspection, des skills et des
      messages d'erreur, et d'un LLM qui connaît déjà Rhai, ce qui exclut les SLM (voir Périmètre et Risques).
    - **Source:** user instruction, 2026-10-24

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
    - **Decision:** la crate s'appelle `lymaille` et le tool natif unique `rhai_pad`.
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

## Risques

- **Rhai est un langage de niche.** Le modèle doit déjà le connaître, ce qui borne la cible à la classe de Qwen3.6 et
  au-delà (voir Périmètre). Mitigation : introspection, skills avec exemples, messages d'erreur utiles, fonctions de
  formatage fournies. Un petit jeu de tâches de référence, versionné dans le dépôt et exécuté à la demande contre un modèle réel hors CI, mesure
  le taux de réussite.
- **Injection de prompt.** Le contenu d'alertes et de logs est non fiable et atteint des agents pouvant agir. Mitigation :
  capacités étroites (BR-2, BR-7). Les toolsets d'écriture sont à concevoir en connaissance de ce risque.
- **Dérive du vocabulaire du ToolsetContext.** Un nom de rôle ou de paramètre inventé sans déclaration casse la
  portabilité entre environnements. Mitigation : les ContextType nommés (DEC-11).
- **Espace de Memory partagé entre agent principal et sous-agents** (idée réservée, non décidée). Une Memory que des
  sous-agents exposés à du contenu non fiable écrivent et que l'agent principal lit ouvrirait un canal d'influence qui
  contourne la séparation des privilèges de BR-2, avec des écritures concurrentes de sous-agents parallèles et la perte
  de reproductibilité d'un batch. Hors du premier jalon : le besoin décidera (voir `delegation`).

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
- **AC-6:** La requête initiale envoyée au fournisseur contient exactement une définition de tool.
- **AC-7:** Après une session utilisant un serveur MCP et un moteur d'inférence authentifiés, aucun secret n'apparaît
  dans l'état persisté, les événements, les journaux ni les messages renvoyés au LLM.
- **AC-8:** Avec un fournisseur simulé qui échoue deux fois puis répond, le tour aboutit, la session reste intacte, et
  deux événements de réessai à délais croissants ont été émis.
- **AC-9:** Avec un fournisseur simulé qui répond 404 (modèle introuvable), le tour échoue sans réessai, avec une erreur
  explicite et un événement, et la session reste reprenable.
- **AC-10:** `scripts/check` passe sur toute la base de code, et chaque lint critique du harnais se déclenche sur une faute
  volontaire, vérifié par un script (modèle : `spikes/lint-harness/check.sh`).

## Open Issues

- **TBD:** compléter le harnais : installer `cargo-deny` et `cargo-mutants` (absents de la machine de développement), écrire
  `deny.toml` (licences, avis de sécurité, sources) et `scripts/mutants`, déclarer les doublons de versions inévitables dans
  `allowed-duplicate-crates` avec une raison, et reporter dans le dépôt le mécanisme de canaris de
  `spikes/lint-harness`.

- **TBD:** réaliser la shopping-list de `vynil-core` avant de coder : `docs/vynil-core-shopping-list.md`. Un seul Must
  (enregistrement granulaire du socle commun, additif : `vynil-org/vynil-core#18`), quatre Nice (`#19` à `#22`).

## Références

- Méthode : https://specdd.ai/llms.txt et le plugin DSD (`.specdd/plugins/github.com/specdd/specdd/dsd/plugin.md`).
- Shopping-list `vynil-core` : `docs/vynil-core-shopping-list.md`.
- Langue : documents DSD et specs `.sdd` en français, règle posée dans `.specdd/bootstrap.project.md`.
- Format des skills : https://agentskills.io/specification.
