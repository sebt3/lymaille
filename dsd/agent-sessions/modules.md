# Solution Design: Modules

## Objectif

Fournir les modules Rhai de travail d'une session : todo, Memory, modules MCP, connaissance optionnelle, et le point
d'extension pour les modules fournis par l'hôte.

## Contexte applicable du domaine

`agent-sessions#BR-4` (niveaux cohabitants), `agent-sessions#BR-5` (observabilité par l'hôte), `agent-sessions#DEC-6`
(Memory unique), `agent-sessions#CON-1` (stockage par traits, MCP possédé par la librairie) et `agent-sessions#CON-3`
(références et non secrets) s'appliquent.

## Exigences fonctionnelles

- **FR-1 - MUST:** Un module todo gère la liste de tâches de la session : création, changement d'état, lecture ; chaque
  changement émet un événement et la todo est persistée avec la session.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** Un module Memory crée, lit, modifie, supprime et liste des Memory sur les niveaux utilisateur, partagé et
  session ; une Memory a un identifiant, un titre et un corps ; la clé est le niveau et l'identifiant. Les trois niveaux
  passent par le même trait de stockage, le niveau session étant qualifié par l'identifiant de la session : l'état
  persisté de la session ne contient pas de Memory.
    - **Source:** user instruction, 2026-10-24 ; stockage unique, user instruction, 2026-10-25 ; niveau partagé (`Shared`) à la place de
      projet, sans alias, `sebt3/lymaille#5` point 2, user instruction, 2026-11-04
    - **Verified by:** `AC-2`

- **FR-3 - MUST:** La liste des Memory ne rend que l'identifiant et le titre ; le corps se lit à la demande.
    - **Source:** user instruction, 2026-10-24

- **FR-4 - MUST:** La Memory est exposée par trois modules, un par niveau (utilisateur, partagé, session ; `memory_user`, `memory_shared`, `memory_session`). L'accès
  d'un agent à un niveau se décide par la nature de sa session et ce que l'hôte lui fournit (`agent-sessions#DEC-16`) : `memory_session` est celle de la
  session de chaque agent ; `memory_shared` se donne quand l'hôte fournit un niveau de Memory partagé à cette session — l'hôte le dit à la table des
  modules natifs (`src/modules/native.sdd`), aucune clé du ToolsetContext ne le désigne — et ne concerne que les agents productifs ; `memory_user` est en lecture
  pour tous et en écriture pour l'agent de configuration seul. La définition d'agent ne porte aucun droit de Memory.
    - **Source:** agent-sessions#BR-2 ; user instruction, 2026-10-27 ; révisé, user instruction, 2026-11-21 ; niveau partagé dit par l'hôte, user instruction, 2026-11-22

- **FR-5 - MUST:** Les changements de Memory émettent un événement.
    - **Source:** agent-sessions#BR-5

- **FR-6 - MUST:** Pour chaque rôle MCP lié par le ToolsetContext, la librairie ouvre la connexion, génère un module Rhai
  à partir des tools du serveur (les descriptions MCP servant de documentation des fonctions) et rétablit la
  connexion à la restauration ; les serveurs sont distants (`agent-sessions#CON-5`). Le module porte le nom du rôle
  (`loki::query`).
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-3`

- **FR-7 - MUST:** Les notifications d'un serveur MCP alimentent la source d'événements de même nom fournie aux watchers.
    - **Source:** agent-sessions/delegation#FR-10

- **FR-8 - MUST:** L'hôte peut enregistrer des modules Rhai supplémentaires, par exemple le module de gestion de
  workflows de vanyline ; ils sont des capacités brutes (`agent-sessions#DEC-16`) : disponibles comme dépendances des toolsets, jamais présentés directement au LLM productif.
    - **Source:** user instruction, 2026-10-24

- **FR-9 - MUST:** Quand un serveur MCP devient injoignable en cours de run, la librairie tente de se reconnecter quelques
  fois avec un backoff court (3 essais, 1 s puis doublement, par défaut), émet un événement par tentative, reste
  annulable, puis rend une erreur Rhai explicite à l'appelant. La connexion se rétablit paresseusement à l'appel suivant.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-10 - MUST:** Chaque tool MCP devient une fonction Rhai à un paramètre map, validé contre le JSON Schema du tool avant
  l'appel ; la documentation introspectable de la fonction reprend ce schéma, et le retour est le JSON converti en
  `Dynamic`. Le nom de la fonction est celui du tool dont chaque caractère invalide dans un identifiant Rhai devient `_`
  (`loki.query` donne `loki_query`) ; deux tools d'un même serveur qui aboutissent au même nom font échouer le chargement
  du module avec une erreur explicite. Le nom MCP d'origine figure dans la documentation de la fonction.
    - **Source:** user instruction, 2026-10-24 ; nommage, user instruction, 2026-10-25
    - **Verified by:** `AC-5`, `AC-6`

- **FR-11 - MUST:** La génération du module d'un rôle MCP à partir de `tools/list` existe en deux modes : *réel* (les fonctions
  appellent `tools/call`) et *coquille* (mêmes noms, signatures et documentation, mais tout appel rend l'erreur d'inspection
  seule, `agent-sessions/scratchpad#FR-13`). Le connecteur MCP expose séparément `list_tools` et `call_tool` ; en mode
  coquille la librairie ne reçoit que `list_tools`, si bien que l'impossibilité d'appeler tient au type. L'hôte résout les
  références d'authentification du ToolsetContext cible pour le listage ; un serveur injoignable fait échouer l'ouverture de
  cette cible avec une erreur explicite.
    - **Source:** user instruction, 2026-10-29
    - **Verified by:** `AC-7`

- **FR-12 - MUST:** La documentation introspectable de chaque fonction MCP contient une ligne d'usage, l'appel Rhai attendu
  construit d'après le schéma d'entrée du tool (`agent-sessions/scratchpad#FR-14`) : `read_file(#{ path: "…" })`, champs
  obligatoires d'abord avec une valeur d'exemple par type, champs facultatifs nommés à la suite. La ligne est bornée en taille
  et ne porte ni valeur par défaut ni énumération du schéma (`agent-sessions#CON-3`).
    - **Source:** `sebt3/lymaille#3`, 2026-11-03
    - **Verified by:** `AC-8`

- **FR-13 - MUST:** Le trait d'authentification d'un connecteur MCP et les en-têtes qu'il rend se compilent sans la feature Cargo
  `mcp-http` : un hôte qui n'écrit que son implémentation d'authentification, ou qui teste la sienne, n'active pas le connecteur
  réel (`rmcp`, `http`). Le jeton d'un jeu d'en-têtes se lit par un accesseur en lecture seule, pour qu'un hôte vérifie ce que sa
  propre authentification rend ; le jeton ne se montre toujours ni dans `Debug`, ni dans une erreur, ni dans un événement
  (`agent-sessions#CON-3`).
    - **Source:** `sebt3/lymaille#17` point 3 et `sebt3/lymaille#13` point 2, 2026-11-17
    - **Verified by:** `AC-9`

- **FR-14 - MUST:** Sous la feature `embedder` (`agent-sessions#NFR-5`), les Memory se cherchent par similarité sémantique. Chaque module
  Memory (`memory_user`, `memory_shared`, `memory_session`) gagne une fonction de recherche, une requête texte et un nombre maximal de résultats en entrée,
  qui rend, par ordre de pertinence, l'identifiant, le titre et le score de chaque Memory trouvée, jamais le corps (`FR-3`). L'accès suit
  les toolsets de la définition d'agent comme tout le reste (`FR-4`) : un agent qui n'a pas le niveau n'y cherche pas. La recherche d'un niveau
  ne rend que les Memory de sa portée (`FR-2`).
    - **Source:** user instruction, 2026-11-20 ; extension « connaissance » du 2026-10-24 devenue exigence
    - **Verified by:** `AC-10`

- **FR-15 - MUST:** Sous la même feature, les skills se cherchent de la même façon : une recherche sémantique rend le nom, la description et le score,
  jamais le corps, que le tool `skill` lit à la demande. Elle complète sans remplacer la recherche lexicale de `find` (`scratchpad` FR-3).
    - **Source:** user instruction, 2026-11-20
    - **Verified by:** `AC-11`

- **FR-16 - MUST:** La recherche s'appuie sur `VectorStoreIndex` de `rig` (`agent-sessions#DEC-15`) : l'hôte fournit à la librairie, par portée de
  Memory et pour les skills, un index qu'il a construit avec le modèle d'embedding de son choix et qu'il maintient. La librairie interroge l'index
  par identifiants (elle n'impose aucun type de document), relit titre ou nom auprès du stockage de l'hôte et ne stocke aucun vecteur
  (`agent-sessions#CON-1`). Elle fournit le texte à embeder pour une Memory (titre et corps) et pour un skill (nom, description et corps), borné par
  la politique de troncature de `agents` FR-14, mais ne réindexe rien d'elle-même : à chaque changement d'une Memory ou d'un skill, l'hôte réindexe
  explicitement. Un index absent pour une portée donne une erreur Rhai explicite, pas un résultat vide.
    - **Source:** user instruction, 2026-11-20
    - **Verified by:** `AC-10`, `AC-11`, `AC-12`

## Questions ouvertes

- **Exposition de la recherche sémantique des skills** : tranché le 2026-11-22, extension du tool natif `skill` (`toolsets` FR-18), point d'accès déjà établi aux skills ; aucun module Rhai de plus. Un module séparé ne s'ajoutera que si un hôte le demande.


## Acceptation

- **AC-1:** Un changement d'état de tâche fait émettre un événement à l'hôte et survit à une restauration.
- **AC-2:** Une Memory de même identifiant créée aux niveaux utilisateur et partagé est lue indépendamment ; un agent en
  lecture seule sur le niveau utilisateur (toolset qui n'expose que les fonctions de lecture) ne peut pas la modifier.
- **AC-3:** Avec un serveur MCP simulé exposant deux tools, la session voit un module à deux fonctions documentées par les
  descriptions MCP.
- **AC-4:** Avec un serveur MCP simulé qui coupe la connexion, l'appel rend une erreur Rhai explicite après les tentatives
  de reconnexion, un événement par tentative, et l'appel suivant rétablit la connexion.
- **AC-5:** Un appel de fonction MCP avec un argument invalide pour le JSON Schema du tool échoue avant tout appel réseau, et
  la documentation introspectée de la fonction contient le schéma.
- **AC-6:** Un serveur MCP simulé exposant `loki.query` et `get-pods` sous le rôle `grafana` donne `grafana::loki_query` et
  `grafana::get_pods`, documentées avec leur nom d'origine ; un serveur exposant `a.b` et `a_b` fait échouer le chargement.

- **AC-7:** Avec un serveur MCP simulé exposant deux tools et un connecteur qui n'offre que `list_tools`, le module coquille a deux
  fonctions documentées et l'appel de l'une rend l'erreur d'inspection seule ; un serveur injoignable fait échouer l'ouverture de la cible.
- **AC-8:** Avec un serveur MCP simulé dont un tool exige le champ texte `path` et accepte l'entier facultatif `limit`, la documentation de la
  fonction contient la ligne d'usage `read_file(#{ path: "…" })` suivie du champ facultatif `limit`.

- **AC-9:** Avec `--no-default-features`, une authentification d'hôte qui rend un jeu d'en-têtes compile et se teste : le jeton
  rendu est lisible par l'accesseur et le `Debug` du jeu d'en-têtes ne contient aucune valeur ; avec la feature `mcp-http`, le
  connecteur réel accepte la même authentification.

- **AC-10:** Avec l'implémentation de test de l'`Embedder` et un index en mémoire de `rig`, trois Memory dont une proche d'une requête : la fonction de recherche du
  niveau rend d'abord cette Memory, avec son identifiant, son titre et un score, sans corps ; une Memory du même identifiant dans un autre niveau n'apparaît
  pas ; un agent à qui sa nature de session ou son contexte ne donne pas le niveau n'a pas la fonction.
- **AC-11:** Avec les mêmes moyens, une recherche de skills rend le nom, la description et le score, sans corps, et la lecture par le tool `skill` rend le corps.
- **AC-12:** Une portée sans index fournit une erreur Rhai explicite ; après un changement de Memory que l'hôte réindexe, la recherche rend la version à jour ;
  la librairie ne réindexe pas d'elle-même (un changement non réindexé n'est pas trouvé).

## Réalisation SpecDD

Specs dans `src/modules/`, implémentées (des tâches de durcissement restent listées dans les `Tasks` de certaines specs) :

- `modules.sdd` : spec de répertoire.
- `error.sdd` : erreur typée du module.
- `todo.sdd` : liste de tâches et module Rhai `todo` (FR-1, AC-1).
- `memory.sdd`, `memory_module.sdd` : Memory par niveau, trait de stockage, modules `memory_user`, `memory_shared`, `memory_session` (FR-2 à FR-5, AC-2).
- `mcp_name.sdd`, `mcp.sdd`, `mcp_module.sdd` : nommage des fonctions, traits de connecteur, génération du module en mode réel et coquille, validation JSON Schema, reconnexion, ligne d'usage (FR-6, FR-9 à FR-12, AC-3 à AC-8).
- `mcp_auth.sdd` : en-têtes d'authentification et trait `McpAuth` de l'hôte, sans la feature `mcp-http` (FR-13, AC-9).
- `mcp_client.sdd` : connecteur MCP réel HTTP streamable sur `rmcp`, derrière la feature `mcp-http` (FR-6, CON-5).

État au 2026-11-01 : tout est livré, y compris `FR-7` (notifications MCP vers les watchers) : le connecteur MCP appelle `notify` sur la source `mcp` (`delegation/sources.sdd` #2, confirmé par le validateur le 2026-11-01 ; tests de notifications et d'annulation ajoutés). FR-8 (modules de l'hôte) ne demande aucun code : un module construit par l'hôte entre dans `Presentation`.
  Correction au 2026-11-22 (`DEC-16`) : la destination est devenue les capacités brutes (`Capabilities`), jamais la présentation ; la phrase ci-dessus est l'état de l'époque, `FR-8` et `agent-sessions#DEC-16` disent le contrat en vigueur.

État au 2026-11-04 (lot 0.2.0) : livré, `FR-12` (ligne d'usage des fonctions MCP) et le niveau `Shared` de la Memory (`FR-2`, `FR-4`, `AC-2` : `MemoryLevel::Shared`, module `memory_shared`, sans alias).

État au 2026-11-20 (lot 0.3.2) : l'« extension réservée : connaissance » devient `FR-14` à `FR-16` ; specs à écrire (`memory_search.sdd`, recherche des skills dans `src/toolsets/`). Rien n'est implémenté.
