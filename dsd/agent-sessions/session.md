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
  serde), le texte source des fonctions définies, la todo, les watchers, les handles des runs de sous-agents en cours (`agent-sessions/delegation#FR-14`), les compteurs de tokens, la
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

- **FR-7 - MUST:** Pendant le backoff qui suit une erreur transitoire du moteur d'inférence (`agent-sessions#FR-6`), le tour reste
  annulable et l'historique n'est pas altéré par l'appel échoué. Le backoff démarre à 1 s et double à chaque réessai.
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

- **FR-10 - MUST:** Une session se restaure toujours avec les définitions courantes, figées ensuite pour le nouveau segment
  (`agent-sessions/toolsets#FR-16`). Un message système signale au LLM ce qui
  a changé depuis la sauvegarde : une version majeure de toolset différente, une définition d'agent dont l'empreinte
  diffère de celle enregistrée, un toolset plus ancien que celui de la sauvegarde. Les évolutions mineures du format de
  session n'ajoutent que des champs optionnels ; seule une version majeure incompatible refuse la restauration.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-6`

- **FR-11 - MUST:** Au début de chaque segment de contexte (début de session, sortie de compaction, restauration), la session
  persiste le contexte tel que le modèle l'a reçu : le prompt système rendu, les définitions des tools natifs (`rhai_pad` et `skill`) et le profil
  de modèle résolu (identifiant et mode de rejeu du raisonnement). Une restauration qui re-rend le prompt avec des
  définitions plus récentes (FR-10) ne remplace pas ce qui a été envoyé auparavant.
    - **Source:** user instruction, 2026-10-24 ; agent-sessions#DEC-12 ; contenu confirmé, user instruction, 2026-10-27
    - **Verified by:** `AC-7`

- **FR-12 - MUST:** L'historique persisté garde chaque message tel que le modèle l'a reçu ou produit : sorties de `rhai_pad`
  après plafonnement et troncature, sorties de `skill`, raisonnement avec son émetteur, appels d'outils. Les frontières de segment
  (compaction, restauration) y sont marquées.
    - **Source:** user instruction, 2026-10-24 ; agent-sessions#DEC-12
    - **Verified by:** `AC-7`

- **FR-13 - MUST:** Chaque tour et chaque run enregistre des signaux de qualité exploitables après coup : issue (résultat
  valide, échec ou annulation), nombre d'erreurs Rhai et de corrections, agent producteur du run, sous-agent et étape de
  workflow concernés, versions de toolsets et profil de modèle.
    - **Source:** user instruction, 2026-10-24 ; agent-sessions#DEC-12 ; référence de l'agent pour exclure les sessions de
      l'agent de configuration du filtrage des candidatures (`agent-sessions#DEC-13`), user instruction, 2026-10-26 ;
      liste des signaux confirmée, user instruction, 2026-10-27
    - **Verified by:** `AC-8`

- **FR-14 - MUST:** Un message utilisateur reçu pendant un tour actif n'ouvre pas de nouveau tour : il est injecté dans
  l'historique avant la prochaine requête d'inférence de ce tour, jamais entre un `tool_call` et son résultat, et la
  condition de compaction (`agent-sessions/context-window#FR-2`) l'inclut. Reçu hors tour, il en ouvre un.
    - **Source:** user instruction, 2026-10-25
    - **Verified by:** `AC-9`

- **FR-15 - MUST:** L'hôte peut étiqueter, pendant ou après la session, la session, un tour ou un run (par exemple en
  réponse à une question posée à l'utilisateur en cours de session). Une étiquette est une entrée ajoutée à la session,
  jamais une modification : libellé tiré d'un vocabulaire que l'hôte définit, émetteur (humain ou automatique) et date
  posés par la librairie, persistée avec la session. La librairie n'impose ni le moment ni la forme de la question, qui
  relèvent de l'hôte ; elle émet déjà les fins de tour et de run qui servent de points d'accroche.
    - **Source:** user instruction, 2026-10-26 ; agent-sessions#DEC-13 ; granularité et vocabulaire d'étiquettes libres (définis par l'hôte) confirmés, user instruction, 2026-10-27
    - **Verified by:** `AC-10`

## Décisions

- **DEC-1 - Collisions d'écriture laissées au stockage**
    - **Context:** le stockage cible est PostgreSQL dans l'immense majorité des cas.
    - **Decision:** la gestion des écritures concurrentes en multi-processus revient à l'implémentation du trait ; la
      librairie ne garantit que l'exclusion intra-processus (FR-4).
    - **Consequences:** le trait est un `load` et un `save` simples, sans jeton de version ; la librairie ne détecte aucun
      conflit entre processus. Avec plusieurs réplicas, deux processus peuvent charger la même session et le dernier
      `save` l'emporte : l'unicité d'une session entre processus est garantie par l'hôte (`agent-sessions#CON-7`).
    - **Source:** user instruction, 2026-10-24

## Extension réservée : export de données d'entraînement

Idée consignée, hors du premier jalon et sans exigence d'export : le besoin d'une LoRA n'est pas avéré. Seule la capture
(FR-11 à FR-13) est exigée, parce qu'elle est coûteuse à rattraper alors que l'export se greffe ensuite sans rien casser.
Les données capturées restent soumises à `agent-sessions#CON-3` et à la persistance par traits de `agent-sessions#CON-1`.

Pistes pour le jour où le besoin se confirmera :

- l'unité d'une ligne serait un segment de contexte, avec un indicateur « entraînable » sur chaque message produit par le
  modèle ;
- la crate attacherait les signaux de qualité (FR-13) et laisserait l'hôte filtrer ;
- l'export serait un opt-in par session, avec le masquage des secrets et un hook de rédaction fourni par l'hôte ;
- le format serait une structure neutre de type chat (messages, raisonnement, appels d'outils, définitions des tools natifs), le
  gabarit du modèle cible étant appliqué par l'outil d'entraînement ;
- avant tout entraînement, mesurer le taux de réussite du modèle de base sur le jeu de tâches de référence, qui ne doit pas
  servir d'entraînement.

Source: user instruction, 2026-10-24

## Questions ouvertes

- **Décidé:** forme du retour de texte vers l'hôte pendant un tour : les fragments de streaming sont des événements du canal
  de `agent-sessions#FR-3` ; la réponse complète figure dans l'événement de fin de tour. Pas de flux dédié.
    - **Source:** user instruction, 2026-10-27
- **Décidé:** les types `Dynamic` étiquetés restent `char` et `blob` en plus des types que JSON distingue. Les horodatages et
  décimaux sont hors périmètre tant qu'aucun besoin n'est établi ; une valeur non prise en charge à la sérialisation rend une
  erreur explicite, jamais une perte silencieuse. Le `blob` s'encode en base64.
    - **Source:** user instruction, 2026-10-27

## Acceptation

- **AC-1:** Une session sauvegardée via un stockage simulé puis rechargée restitue historique, variables, fonctions et
  todo ; ses Memory de niveau session, lues par le trait Memory (`agent-sessions/modules#FR-2`), restent accessibles.
- **AC-2:** Restaurer une session dont une fonction référence une API de toolset disparue restaure les variables et les
  autres fonctions, et le premier message système lui signale la fonction cassée.
- **AC-3:** Une annulation émise pendant un backoff termine le tour sans ajouter de message d'erreur à l'historique.
- **AC-4:** Un script qui appelle un MCP authentifié produit un événement de script et un événement d'appel MCP,
  tronqués au plafond de rendu et sans aucun secret.
- **AC-5:** Une session portant un `char`, un `blob` et un entier, sauvegardée puis restaurée, rend des valeurs du même type
  que l'original.
- **AC-6:** Une session restaurée après modification de sa définition d'agent, ou avec un toolset d'une autre version
  majeure, reçoit un message système qui nomme le changement et reprend avec les définitions courantes.
- **AC-7:** Une session compactée puis restaurée après modification de l'agent permet de reconstituer, pour chaque segment, le
  prompt système, les définitions des tools et le profil de modèle tels qu'ils ont été envoyés, ainsi que les sorties de `rhai_pad`
  telles que le modèle les a reçues.
- **AC-8:** Un run batch de sous-agent dont la sortie est invalide puis corrigée enregistre l'issue finale, le nombre d'erreurs et
  de corrections, l'étape de workflow, l'agent producteur et les versions de toolsets.
- **AC-9:** Un message utilisateur envoyé pendant qu'un script du tour s'exécute apparaît dans la requête d'inférence suivante
  du même tour, après le résultat de l'outil, sans ouvrir de second tour.
- **AC-10:** Une étiquette ajoutée à un run après la fin de la session est restituée à la restauration avec son émetteur et sa
  date ; ajouter une seconde étiquette conserve la première.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore ; à créer avec le squelette du crate.
