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

- **FR-4 - MUST:** La Memory est exposée par trois modules, un par niveau (utilisateur, partagé, session ; `memory_user`, `memory_shared`, `memory_session`). L'accès d'un
  agent à un niveau, y compris la lecture seule, se décide par les toolsets de sa définition (`agent-sessions#BR-2`) : la
  définition d'agent ne porte aucun droit de Memory, et toute vérification passe par un toolset.
    - **Source:** agent-sessions#BR-2 ; user instruction, 2026-10-27

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
  workflows de vanyline ; ils sont soumis à la même règle d'accord par définition d'agent.
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

## Extension réservée : connaissance

Hors du premier jalon et sans exigence, le besoin n'étant pas avéré. Principe consigné : un module Rhai optionnel de
connaissance s'appuierait sur le trait de recherche vectorielle de `rig`, avec un stockage fourni par l'hôte
(`agent-sessions#CON-1` interdit l'E/S de stockage, pas l'usage de `rig`), et le modèle d'embedding serait désigné par
un profil de modèle de type « embedding » (`agent-sessions/agents#FR-7`).

Source: user instruction, 2026-10-24

## Questions ouvertes


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

## Réalisation SpecDD

Specs dans `src/modules/`, implémentées (des tâches de durcissement restent listées dans les `Tasks` de certaines specs) :

- `modules.sdd` : spec de répertoire.
- `error.sdd` : erreur typée du module.
- `todo.sdd` : liste de tâches et module Rhai `todo` (FR-1, AC-1).
- `memory.sdd`, `memory_module.sdd` : Memory par niveau, trait de stockage, modules `memory_user`, `memory_shared`, `memory_session` (FR-2 à FR-5, AC-2).
- `mcp_name.sdd`, `mcp.sdd`, `mcp_module.sdd` : nommage des fonctions, traits de connecteur, génération du module en mode réel et coquille, validation JSON Schema, reconnexion, ligne d'usage (FR-6, FR-9 à FR-12, AC-3 à AC-8).
- `mcp_client.sdd` : connecteur MCP réel HTTP streamable sur `rmcp`, derrière la feature `mcp-http` (FR-6, CON-5).

État au 2026-11-01 : tout est livré, y compris `FR-7` (notifications MCP vers les watchers) : le connecteur MCP appelle `notify` sur la source `mcp` (`delegation/sources.sdd` #2, confirmé par le validateur le 2026-11-01 ; tests de notifications et d'annulation ajoutés). FR-8 (modules de l'hôte) ne demande aucun code : un module construit par l'hôte entre dans `Grants`.

État au 2026-11-04 (lot 0.2.0) : livré, `FR-12` (ligne d'usage des fonctions MCP) et le niveau `Shared` de la Memory (`FR-2`, `FR-4`, `AC-2` : `MemoryLevel::Shared`, module `memory_shared`, sans alias).
