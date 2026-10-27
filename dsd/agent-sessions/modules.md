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

- **FR-2 - MUST:** Un module Memory crée, lit, modifie, supprime et liste des Memory sur les niveaux utilisateur, projet et
  session ; une Memory a un identifiant, un titre et un corps ; la clé est le niveau et l'identifiant. Les trois niveaux
  passent par le même trait de stockage, le niveau session étant qualifié par l'identifiant de la session : l'état
  persisté de la session ne contient pas de Memory.
    - **Source:** user instruction, 2026-10-24 ; stockage unique, user instruction, 2026-10-25
    - **Verified by:** `AC-2`

- **FR-3 - MUST:** La liste des Memory ne rend que l'identifiant et le titre ; le corps se lit à la demande.
    - **Source:** user instruction, 2026-10-24

- **FR-4 - MUST:** La Memory est exposée par trois modules, un par niveau (utilisateur, projet, session). L'accès d'un
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

## Extension réservée : connaissance

Hors du premier jalon et sans exigence, le besoin n'étant pas avéré. Principe consigné : un module Rhai optionnel de
connaissance s'appuierait sur le trait de recherche vectorielle de `rig`, avec un stockage fourni par l'hôte
(`agent-sessions#CON-1` interdit l'E/S de stockage, pas l'usage de `rig`), et le modèle d'embedding serait désigné par
un profil de modèle de type « embedding » (`agent-sessions/agents#FR-7`).

Source: user instruction, 2026-10-24

## Questions ouvertes


## Acceptation

- **AC-1:** Un changement d'état de tâche fait émettre un événement à l'hôte et survit à une restauration.
- **AC-2:** Une Memory de même identifiant créée aux niveaux utilisateur et projet est lue indépendamment ; un agent en
  lecture seule sur le niveau utilisateur (toolset qui n'expose que les fonctions de lecture) ne peut pas la modifier.
- **AC-3:** Avec un serveur MCP simulé exposant deux tools, la session voit un module à deux fonctions documentées par les
  descriptions MCP.
- **AC-4:** Avec un serveur MCP simulé qui coupe la connexion, l'appel rend une erreur Rhai explicite après les tentatives
  de reconnexion, un événement par tentative, et l'appel suivant rétablit la connexion.
- **AC-5:** Un appel de fonction MCP avec un argument invalide pour le JSON Schema du tool échoue avant tout appel réseau, et
  la documentation introspectée de la fonction contient le schéma.
- **AC-6:** Un serveur MCP simulé exposant `loki.query` et `get-pods` sous le rôle `grafana` donne `grafana::loki_query` et
  `grafana::get_pods`, documentées avec leur nom d'origine ; un serveur exposant `a.b` et `a_b` fait échouer le chargement.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
