# Solution Design: Delegation

## Objectif

Permettre à un agent principal de déléguer des tâches à des sous-agents autonomes, et à un LLM de se faire notifier
d'événements par des watchers, sur un modèle asynchrone commun.

## Contexte applicable du domaine

`agent-sessions#BR-6` (sous-agent batch, sans délégation), `agent-sessions#DEC-4` (le run batch est l'unité de base),
`agent-sessions#DEC-8` (modèle asynchrone unifié), `agent-sessions#FR-4` et `agent-sessions#FR-5` (annulation,
événements de dépassement) s'appliquent. `agent-sessions#BR-2` implique que l'agent principal ne choisit que parmi des
sous-agents prédéfinis.

## Exigences fonctionnelles

- **FR-1 - MUST:** Un agent principal délègue une tâche en choisissant un sous-agent parmi ceux que sa définition
  autorise et en fournissant la tâche en entrée ; il ne peut ni accorder de toolset à un sous-agent ni en créer un.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** Le module de délégation n'existe pas dans le moteur d'un sous-agent : l'impossibilité de déléguer est
  garantie par le type de session, pas par une vérification à l'exécution.
    - **Source:** user instruction, 2026-10-24 ; « les sous-agents ne peuvent pas en avoir »
    - **Verified by:** `AC-1`

- **FR-3 - MUST:** Un sous-agent a ses propres scratchpad, todo et Memory de session, partage le ToolsetContext de la
  session en lecture seule et peut disposer de toolsets que l'agent principal n'a pas.
    - **Source:** user instruction, 2026-10-24

- **FR-4 - MUST:** Un run de sous-agent rend un statut et un résultat conforme à un JSON Schema déclaré par sa
  définition, de sorte qu'un workflow puisse en dépendre. Le sous-agent rend son résultat par une fonction Rhai ; si la
  validation échoue, l'erreur lui est renvoyée pour qu'il corrige dans le même run, trois fois au plus par défaut (valeur surchargeable par
  définition de sous-agent). S'il ne peut pas aboutir, il rend un
  échec explicite qui dit ce qui manque.
    - **Source:** agent-sessions#DEC-4; user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-6 - MUST:** La délégation rend un handle ; la fin du sous-agent est un événement, et une attente synchrone n'est
  qu'un sucre au-dessus.
    - **Source:** agent-sessions#DEC-8
    - **Verified by:** `AC-3`

- **FR-7 - MUST:** L'annulation d'un run se propage à ses sous-agents en cours.
    - **Source:** agent-sessions#FR-4

- **FR-8 - MUST:** Un watcher est une souscription déclarative (source d'événement, filtre exprimé comme prédicat Rhai)
  dont la seule action est d'injecter une notification dans le tour suivant ou de demander à l'hôte d'en lancer un. Les watchers sont persistés avec la session.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-9 - MUST:** Les sources d'événements se branchent par un trait ; l'hôte les alimente par un appel de notification.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-10 - MUST:** La librairie fournit par défaut quatre sources d'événements : fin d'un sous-agent, notifications des
  serveurs MCP, changements de todo et de Memory, minuteurs (échéance et périodique).
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-11 - MUST:** Le nombre de requêtes simultanées vers un moteur d'inférence est limité par le profil de modèle ; au-delà,
  la librairie met les requêtes en file. Un agent principal reçoit dans son contexte initial la capacité réelle de
  sous-agents parallélisables : la limite du profil, moins les requêtes déjà consommées au démarrage de la session,
  moins une pour la session de l'agent principal.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-5`

- **FR-12 - MUST:** À la restauration d'une session, une échéance de minuteur manquée se déclenche une fois, immédiatement,
  avec son retard ; un minuteur périodique saute les occurrences manquées et se réarme à la prochaine.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-6`

- **FR-13 - MUST:** Le décompte des requêtes en cours vers un moteur est local au processus par défaut ; un trait optionnel
  permet à l'hôte de fournir un décompte partagé. La capacité annoncée au contexte initial est un instantané, que le LLM peut
  relire par une fonction d'introspection.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-7`

## Extension réservée : Memory partagée

Idée consignée, hors du premier jalon et sans exigence : le besoin n'est pas avéré et c'est lui qui déterminera les choix le
moment venu. Un sous-agent a aujourd'hui sa propre Memory de session (FR-3) et ne communique avec son parent que par son
résultat structuré (FR-4). Si l'idée est reprise, les risques à traiter sont l'influence d'un sous-agent exposé à du
contenu non fiable sur l'agent principal (contournement de `agent-sessions#BR-2`), les écritures concurrentes de
sous-agents parallèles et la perte de reproductibilité d'un batch. Piste prudente : lecture seule d'un instantané de la
Memory de session du parent.

Source: user instruction, 2026-10-24

## Questions ouvertes


## Acceptation

- **AC-1:** Un sous-agent exécuté avec un fournisseur simulé n'expose aucune fonction de délégation, et un agent principal
  ne peut pas désigner un sous-agent absent de sa définition.
- **AC-2:** Un run de sous-agent rend un résultat qui valide son schéma, ou un échec explicite.
- **AC-3:** Une délégation rend immédiatement un handle ; la fin déclenche l'événement attendu.
- **AC-4:** Un watcher dont le filtre accepte un événement injecte une notification au tour suivant ; un filtre qui
  refuse n'injecte rien.
- **AC-5:** Avec un profil limité à 8 requêtes simultanées dont 2 consommées au démarrage, l'agent principal reçoit une
  capacité annoncée de 5 sous-agents ; une neuvième requête simultanée est mise en file.
- **AC-6:** Une session restaurée après l'échéance d'un minuteur reçoit un seul événement qui porte le retard ; un minuteur
  périodique manqué plusieurs fois se réarme sans rejouer les occurrences manquées.
- **AC-7:** Avec un décompte partagé fourni par l'hôte, la capacité annoncée tient compte des requêtes d'un autre processus, et
  la fonction d'introspection rend la valeur à jour.
- **AC-8:** Un sous-agent dont les trois corrections successives restent invalides vis-à-vis du JSON Schema fait échouer le run
  avec la liste des erreurs de validation.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
