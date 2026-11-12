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
  validation échoue, l'erreur lui est renvoyée pour qu'il corrige dans le même run : trois corrections au plus après la
  première tentative par défaut, soit quatre résultats soumis (valeur surchargeable par définition de sous-agent). S'il
  ne peut pas aboutir, il rend un échec explicite qui dit ce qui manque.
    - **Source:** agent-sessions#DEC-4; user instruction, 2026-10-24
    - **Verified by:** `AC-2`, `AC-8`

- **FR-6 - MUST:** La délégation rend un handle ; la fin du sous-agent est un événement, et une attente synchrone n'est
  qu'un sucre au-dessus.
    - **Source:** agent-sessions#DEC-8
    - **Verified by:** `AC-3`

- **FR-7 - MUST:** L'annulation d'un run se propage à ses sous-agents en cours.
    - **Source:** agent-sessions#FR-4

- **FR-8 - MUST:** Un watcher est une souscription déclarative (source d'événement, filtre exprimé comme prédicat Rhai)
  dont la seule action est d'injecter une notification dans le tour suivant ou de demander à l'hôte d'en lancer un. Les
  watchers sont persistés avec la session. Le prédicat est une expression évaluée dans un moteur isolé, avec le seul
  groupe `core` de `vynil-core` et l'événement pour unique entrée : ni scope de session, ni module accordé, ni fonction
  définie par le LLM. Il ne peut donc produire aucun effet de bord ni toucher l'état d'un tour en cours.
    - **Source:** user instruction, 2026-10-24 ; isolement du prédicat, user instruction, 2026-10-25
    - **Verified by:** `AC-4`, `AC-9`

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

- **FR-14 - MUST:** La session d'un sous-agent est persistée par le trait de session comme toute autre session, et la
  session principale persiste les handles de ses runs de sous-agents en cours. À la restauration de la session principale,
  un run qui n'est pas terminé n'est pas repris d'office : il produit un événement de fin au statut « interrompu », qui
  porte la référence de sa session, et l'agent principal peut le relancer en reprenant cette session sauvegardée.
    - **Source:** user instruction, 2026-10-25
    - **Verified by:** `AC-10`

## Idée réservée : MessageBoard

Idée consignée, sans cas d'usage identifié ni exigence : elle reste une idée. Un sous-agent a aujourd'hui sa propre Memory
de session (FR-3) et ne communique avec son parent que par son résultat structuré (FR-4).

Une Memory partagée entre agents a été écartée : espace modifiable, elle cumule écritures concurrentes de sous-agents
parallèles, perte de reproductibilité d'un batch, et une autorité de « fait établi » qui amplifie l'influence d'un
sous-agent exposé à du contenu non fiable (`agent-sessions#BR-2`).

L'alternative consignée est un MessageBoard : un journal en ajout seul de messages (date et heure, auteur, texte). Points
relevés le 2026-10-25 :

- l'ajout seul supprime les conflits d'écriture, et le journal se rejoue, s'audite et se projette à l'humain ;
- l'auteur et la date sont posés par la librairie, jamais déclarés par l'agent, faute de quoi la provenance ne vaut rien ;
- un tableau par arbre de session (principal et sous-agents), persisté avec la session principale, hors fenêtre ;
- lecture par curseur, un nouveau message alimentant une source d'événements pour les watchers (FR-10) ;
- taille de message plafonnée, pour ne pas contourner le plafond de rendu ;
- le canal d'influence demeure, plus large que le résultat structuré puisque le texte est libre et circule pendant le run ;
- un sous-agent qui lit le tableau perd la reproductibilité de son batch : piste, ne lui laisser lire que les messages de
  son parent.

Raffinements du 2026-10-26, qui ne lui donnent toujours pas de cas d'usage :

- un hint n'est pas un fait : l'auteur a d'autres objectifs que le lecteur. La réponse est dans le rendu, que la librairie
  contrôle. Chaque message est rendu dans une enveloppe posée par la librairie (auteur, rôle, date, mention « observation
  d'un voisin, non vérifiée ») ; le texte de l'auteur ne peut pas la contrefaire, la librairie neutralise toute tentative
  de l'imiter. Un message peut citer sa source, par exemple un appel d'outil de la session de l'auteur, que la librairie
  valide comme existant : le lecteur distingue une affirmation sans preuve d'un renvoi à une observation. Les messages
  restent du contenu non fiable au sens de `agent-sessions#BR-2` ;
- la péremption que l'hôte sait détecter se traite par l'hôte, sans que la librairie évalue quoi que ce soit (`agent-sessions#CON-1`)
  et sans comparaison confiée au LLM. Un message peut déclarer un sujet opaque que l'hôte comprend (ressource, fichier,
  objet d'un serveur MCP). L'hôte surveille ce sujet (watchers, FR-10) et, quand il change, ajoute au journal une entrée
  d'invalidation qui vise le message ; auteur et date de cette entrée sont posés par la librairie. À la lecture, le
  message est rendu « périmé depuis <date> : <raison> », et l'entrée alimente la source d'événements. L'invalidation est
  une entrée datée du journal et pas une évaluation à la lecture : le rejeu d'un batch voit les mêmes entrées, la
  reproductibilité tient. Seul l'hôte invalide : un auteur qui déclare un sujet fantaisiste n'invalide que son propre
  message. Défaut connu : granularité grossière, et un message sans sujet ne périme jamais (il se lit avec son âge) ;
- une pondération par score (confiance auto-évaluée, distance, alignement avec la demande) a été examinée et écartée : la
  confiance déclarée par un LLM est un bruit que le mécanisme amplifierait, et la réponse du domaine à l'influence d'un
  sous-agent reste structurelle (`agent-sessions#BR-2`, provenance posée par la librairie), pas probabiliste.

Source: user instruction, 2026-10-25 ; enveloppe, sujet et invalidation par l'hôte, rejet des scores, user instruction, 2026-10-26

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
- **AC-8:** Un sous-agent dont la première tentative et les trois corrections successives restent invalides vis-à-vis du
  JSON Schema fait échouer le run avec la liste des erreurs de validation.
- **AC-9:** Un prédicat de watcher qui tente d'appeler une fonction de module, de lire une variable du scope de session ou
  d'appeler une fonction définie par le LLM échoue sans effet de bord ; l'échec est signalé par un événement et le watcher
  n'injecte rien.
- **AC-10:** Une session principale restaurée alors qu'un sous-agent était en cours reçoit un événement « interrompu » qui
  référence la session du sous-agent ; une relance reprend cette session avec son scratchpad et sa todo.

## Réalisation SpecDD

Specs dans `src/delegation/` (domaine `delegation`) et `src/runtime/` (boucle de tour, dont la délégation a besoin pour exécuter un sous-agent) :

- `delegation.sdd` : spec de répertoire (frontière : aucune E/S propre, pas de module de délégation pour un sous-agent).
- `error.sdd` : erreur typée du module.
- `capacity.sdd` : limiteur de requêtes simultanées, décompte local ou partagé, capacité annoncée (FR-11, FR-13, AC-5, AC-7).
- `sources.sdd` : trait de source, concentrateur, quatre sources fournies, minuteurs et leur restauration (FR-9, FR-10, FR-12, AC-6).
- `watcher.sdd` : watchers persistés, prédicat Rhai isolé, notifications (FR-8, AC-4, AC-9).
- `result.sdd` : résultat structuré validé par JSON Schema avec corrections (FR-4, AC-2, AC-8).
- `run.sdd` : registre des runs, handles, annulation propagée, reprise après restauration, pilote batch (FR-1, FR-3, FR-6, FR-7, FR-14, AC-1, AC-3, AC-10).
- `module.sdd` : modules Rhai `delegate` (agent principal, preuve de type `MainAgent`) et `result` (sous-agent) (FR-1, FR-2, FR-4, FR-6, FR-11, FR-13).
- `../runtime/runtime.sdd`, `../runtime/turn.sdd` : boucle de tour (`session` FR-4 à FR-7, AC-3, AC-9).

Écrites le 2026-10-30, implémentées le 2026-10-31 (lots D1 à D7 et boucle de tour R1, chaque lot validé contre sa spec). Tout le domaine est livré : limiteur de capacité avec son point d'appel `tick` pour l'hôte, sources d'événements et minuteurs, watchers à prédicat borné, résultat structuré validé avec corrections, registre des runs (reprise après restauration, issues normalisées, runs restaurés conservés jusqu'à leur reprise) et modules `delegate` et `result`. Restent à la charge de l'hôte ou d'un autre domaine : l'implémentation du lanceur de sous-agent (`SubagentLauncher`), du décompte partagé de requêtes et des sources d'événements externes, et le branchement du connecteur MCP sur `notify` (`modules` FR-7). L'idée réservée MessageBoard n'a pas de spec (aucune exigence).
