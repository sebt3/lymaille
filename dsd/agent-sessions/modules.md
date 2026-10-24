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
  session ; une Memory a un identifiant, un titre et un corps ; la clé est le niveau et l'identifiant.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-3 - MUST:** La liste des Memory ne rend que l'identifiant et le titre ; le corps se lit à la demande.
    - **Source:** user instruction, 2026-10-24

- **FR-4 - MUST:** La définition d'agent détermine par niveau si le module Memory est en lecture seule ou en lecture et
  écriture.
    - **Source:** agent-sessions#BR-2

- **FR-5 - MUST:** Les changements de Memory émettent un événement.
    - **Source:** agent-sessions#BR-5

- **FR-6 - MUST:** Pour chaque rôle MCP lié par le ToolsetContext, la librairie ouvre la connexion, génère un module Rhai
  à partir des tools du serveur (les descriptions MCP servant de documentation des fonctions) et rétablit la
  connexion à la restauration ; les serveurs sont distants, en HTTPS (`agent-sessions#CON-5`).
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-3`

- **FR-7 - MUST:** Les notifications d'un serveur MCP alimentent la source d'événements de même nom fournie aux watchers.
    - **Source:** agent-sessions/delegation#FR-10

- **FR-8 - MUST:** L'hôte peut enregistrer des modules Rhai supplémentaires, par exemple le module de gestion de
  workflows de vanyline ; ils sont soumis à la même règle d'accord par définition d'agent.
    - **Source:** user instruction, 2026-10-24

## Extension réservée : connaissance

Hors du premier jalon et sans exigence, le besoin n'étant pas avéré. Principe consigné : un module Rhai optionnel de
connaissance s'appuierait sur le trait de recherche vectorielle de `rig`, avec un stockage fourni par l'hôte
(`agent-sessions#CON-1` interdit l'E/S de stockage, pas l'usage de `rig`), et le modèle d'embedding serait désigné par
un profil de modèle de type « embedding » (`agent-sessions/agents#FR-7`).

Source: user instruction, 2026-10-24

## Questions ouvertes

- **TBD:** typage des arguments et valeurs de retour des fonctions générées à partir des schémas MCP.
- **TBD:** reconnexion et gestion d'erreur réseau d'un MCP au milieu d'un run.

## Acceptation

- **AC-1:** Un changement d'état de tâche fait émettre un événement à l'hôte et survit à une restauration.
- **AC-2:** Une Memory de même identifiant créée aux niveaux utilisateur et projet est lue indépendamment ; un agent en
  lecture seule sur le niveau utilisateur ne peut pas la modifier.
- **AC-3:** Avec un serveur MCP simulé exposant deux tools, la session voit un module à deux fonctions documentées par les
  descriptions MCP.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
