# Solution Design: Session

## Objectif

Gérer le cycle de vie d'une session : création, exécution de tours, persistance, restauration, annulation et émission
d'événements. La Solution définit les traits de stockage que l'hôte implémente.

## Contexte applicable du domaine

`agent-sessions#CON-1` impose une librairie pure : la persistance passe par des traits. `agent-sessions#DEC-3` précise
que les connexions MCP ne sont pas persistées. `agent-sessions#NFR-2` impose un tour actif au plus par session.
`agent-sessions#FR-3`, `agent-sessions#FR-4` et `agent-sessions#FR-5` définissent événements, annulation et
non-interruption sur le temps.

## Exigences fonctionnelles

- **FR-1 - MUST:** La librairie définit un trait de stockage de session que l'hôte implémente ; elle n'écrit jamais
  elle-même.
    - **Source:** agent-sessions#CON-1
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** L'état persisté d'une session comprend l'historique (raisonnement compris, avec l'émetteur qui seul peut le rejouer), les variables du scratchpad (sérialisation
  serde), le texte source des fonctions définies, la todo, les watchers, les Memory de niveau session, les compteurs de tokens, la
  référence à l'agent avec l'empreinte de sa définition et au ToolsetContext (jamais de secret, `agent-sessions#CON-3`), une version de format et la
  version de chaque toolset utilisé.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-3 - MUST:** À la restauration, les fonctions sont recompilées depuis leur source ; une définition qui échoue
  n'empêche pas la restauration du reste, et la liste des définitions cassées est signalée au LLM par un message système.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-4 - MUST:** Au plus un tour est actif par session au sein d'un processus.
    - **Source:** agent-sessions#NFR-2

- **FR-5 - MUST:** Un tour en cours est annulable par un handle d'annulation rendu à l'hôte au lancement.
    - **Source:** agent-sessions#FR-4

- **FR-6 - SHOULD:** Un tour qui dépasse un délai configurable émet un événement `Overrun` sans être interrompu.
    - **Source:** agent-sessions#FR-5

- **FR-7 - MUST:** Pendant le backoff qui suit une erreur du moteur d'inférence (`agent-sessions#FR-6`), le tour reste
  annulable et l'historique n'est pas altéré par l'appel échoué.
    - **Source:** agent-sessions#FR-6, agent-sessions#FR-4
    - **Verified by:** `AC-3`

- **FR-8 - MUST:** Chaque script exécuté produit un événement d'audit portant la session, l'agent, le texte complet du
  script, son résultat tronqué au plafond de rendu avec la taille d'origine, sa durée et l'erreur éventuelle ; chaque
  appel MCP produit un événement portant le rôle, la fonction, les arguments, le statut et la durée. La librairie masque
  mécaniquement dans ces événements les secrets qu'elle détient (`agent-sessions#CON-3`).
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-9 - MUST:** Le format de session étiquette le type réel des valeurs que JSON ne distingue pas (`char`, `blob` et
  tout autre type identifié), pour une restauration fidèle. Une valeur non sérialisable est exclue avec l'avertissement de
  `agent-sessions/scratchpad#FR-7`.
    - **Source:** user instruction, 2026-10-24 ; Observed in: spikes/rhai-1-25 (`persistence`, S8)
    - **Verified by:** `AC-5`

- **FR-10 - MUST:** Une session se restaure toujours avec les définitions courantes. Un message système signale au LLM ce qui
  a changé depuis la sauvegarde : une version majeure de toolset différente, une définition d'agent dont l'empreinte
  diffère de celle enregistrée, un toolset plus ancien que celui de la sauvegarde. Les évolutions mineures du format de
  session n'ajoutent que des champs optionnels ; seule une version majeure incompatible refuse la restauration.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-6`

## Décisions

- **DEC-1 - Collisions d'écriture laissées au stockage**
    - **Context:** le stockage cible est PostgreSQL dans l'immense majorité des cas.
    - **Decision:** la gestion des écritures concurrentes en multi-processus revient à l'implémentation du trait ; la
      librairie ne garantit que l'exclusion intra-processus (FR-4).
    - **Consequences:** le trait est un `load` et un `save` simples, sans jeton de version ; la librairie ne détecte aucun
      conflit entre processus. Avec plusieurs réplicas, deux processus peuvent charger la même session et le dernier
      `save` l'emporte : l'unicité d'une session entre processus est garantie par l'hôte (`agent-sessions#CON-7`).
    - **Source:** user instruction, 2026-10-24

## Questions ouvertes

- **TBD:** inventaire des types `Dynamic` à étiqueter au-delà de `char` et `blob` (horodatages, décimaux selon les features).
- **TBD:** délai initial et facteur de croissance du backoff d'inférence. Proposition : 1 s, doublement à chaque réessai,
  jusqu'au plafond de cinq minutes fixé par `agent-sessions#FR-6`.

## Acceptation

- **AC-1:** Une session sauvegardée via un stockage simulé puis rechargée restitue historique, variables, fonctions,
  todo et Memory de session.
- **AC-2:** Restaurer une session dont une fonction référence une API de toolset disparue restaure les variables et les
  autres fonctions, et le premier message système lui signale la fonction cassée.
- **AC-3:** Une annulation émise pendant un backoff termine le tour sans ajouter de message d'erreur à l'historique.
- **AC-4:** Un script qui appelle un MCP authentifié produit un événement de script et un événement d'appel MCP,
  tronqués au plafond de rendu et sans aucun secret.
- **AC-5:** Une session portant un `char`, un `blob` et un entier, sauvegardée puis restaurée, rend des valeurs du même type
  que l'original.
- **AC-6:** Une session restaurée après modification de sa définition d'agent, ou avec un toolset d'une autre version
  majeure, reçoit un message système qui nomme le changement et reprend avec les définitions courantes.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore ; à créer avec le squelette du crate.
