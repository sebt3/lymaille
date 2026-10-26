# Solution Design: Toolsets

## Objectif

Permettre de composer un outillage réutilisable (fonctions Rhai et skills associés) et de l'adapter à un
environnement cible par un ToolsetContext, avec templating des prompts et des skills.

## Contexte applicable du domaine

`agent-sessions#BR-3` (valeurs d'environnement dans le ToolsetContext), `agent-sessions#BR-7` (écriture étroite,
sorties bornées), `agent-sessions#DEC-5` (un ToolsetContext par session) et `agent-sessions#CON-3` (références
uniquement) s'appliquent.

## Exigences fonctionnelles

- **FR-1 - MUST:** Un toolset comprend un module Rhai écrit en Rhai, des skills et la déclaration de ses exigences sur le
  ToolsetContext : un ou plusieurs ContextType, plus éventuellement des paramètres (typés, avec valeurs par défaut) et
  des rôles MCP propres, obligatoires ou optionnels.
    - **Source:** user instruction, 2026-10-24

- **FR-2 - MUST:** Les dépendances d'un toolset envers d'autres modules lui sont accessibles sans l'être au LLM : elles
  n'apparaissent ni dans le scope, ni dans l'introspection, sauf si la définition d'agent les accorde.
    - **Source:** user instruction, 2026-10-24 ; Observed in: spikes/rhai-1-25 (`private_dependencies`) : les appels sont
      bloqués, mais l'export brut des métadonnées de Rhai expose les dépendances en sous-module du toolset ; l'introspection
      de la librairie doit donc les filtrer
    - **Verified by:** `AC-1`

- **FR-3 - MUST:** Le ToolsetContext est un jeu plat de paramètres nommés et de liaisons de rôles, avec une section
  optionnelle de surcharge par toolset ; les valeurs par défaut vivent dans la déclaration du toolset.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-4 - MUST:** La librairie vérifie structurellement la compatibilité du ToolsetContext avec les ContextType exigés
  par les toolsets d'un agent, dès que l'hôte charge ou enregistre une définition d'agent ou de projet, puis à la
  création de la session. Pour l'agent principal, une exigence non satisfaite fait échouer le démarrage avec un message
  explicite. Pour un sous-agent, elle le retire de la liste délégable de la session, sans erreur.
    - **Source:** agent-sessions#DEC-11 ; user instruction, 2026-10-24
    - **Verified by:** `AC-3`, `AC-4`

- **FR-5 - MUST:** Un ToolsetContext est une entité nommée qui vit à un niveau (utilisateur, projet ou session) et se
  stocke par un trait ; il ne contient que des références, pas de secret.
    - **Source:** user instruction, 2026-10-24 ; agent-sessions#CON-3

- **FR-6 - MUST:** Le prompt système, ainsi que le corps et la description des skills, sont des templates Handlebars rendus avec les données du
  ToolsetContext et des métadonnées de session figées au rendu (identifiant d'agent, date de début, toolsets accordés avec leur
  description, noms des skills accordés sans leur description ni leur corps et, pour un agent principal, capacité de
  sous-agents parallélisables, `agent-sessions/delegation#FR-11`),
  y compris des sections conditionnelles selon la présence d'un rôle. Le prompt système est rendu une
  seule fois, à la création ou à la restauration de la session ; aucun état changeant en cours de session (todo, Memory,
  scope) n'y entre.
    - **Source:** user instruction, 2026-10-24 ; noms des skills dans le prompt système, user instruction, 2026-10-26
    - **Verified by:** `AC-2`, `AC-12`

- **FR-7 - MUST:** Un skill est un objet — métadonnées de frontmatter et corps markdown (une chaîne) — compatible avec le
  format `SKILL.md` de la spécification Agent Skills : `name` (1 à 64 caractères : minuscules, chiffres et tirets) et
  `description` (1 à 1024 caractères) obligatoires, `license`, `compatibility` et `metadata` optionnels. Il est rattaché
  à un toolset, disponible tant que le toolset l'est, découvrable par le tool `skill` (FR-18) et par introspection (nom et
  description) ; son corps se lit à la demande et n'est jamais préchargé dans la fenêtre. Aucun champ de skill n'accorde
  de capacité : le champ `allowed-tools` est ignoré.
    - **Source:** user instruction, 2026-10-24 (y compris `allowed-tools` ignoré) ;
      https://agentskills.io/specification (consultée le 2026-10-24)

- **FR-8 - MUST:** Un module permet à un LLM de créer et modifier des toolsets via un trait de stockage. Il n'est
  accordable qu'à un agent principal sans sous-agent délégable : la librairie refuse, au chargement ou à l'enregistrement,
  une définition d'agent qui l'accorde à un sous-agent ou qui l'associe à des sous-agents délégables. Les sessions des
  agents de création de toolsets sont exclues des candidatures à l'amélioration continue (`agent-sessions#DEC-13`). Ce que
  le module enregistre est un brouillon, que le trait de stockage distingue des versions actives : l'activation est une
  opération de l'hôte, hors du module. Les
  agents de configuration sont ainsi séparés des agents productifs, et un agent exposé à du contenu non fiable ne peut pas
  réécrire le code d'un autre agent qu'il pilote.
    - **Source:** user instruction, 2026-10-25 ; exclusion des sessions de l'agent de configuration des candidatures,
      user instruction, 2026-10-26 ; agent-sessions#BR-2
    - **Verified by:** `AC-8`, `AC-13`

- **FR-16 - MUST:** Les définitions d'agents et de toolsets utilisées par une session, y compris celles de ses sous-agents
  délégables, sont figées au début de chaque segment (`agent-sessions/session#FR-11`) : un sous-agent lancé en cours de
  segment utilise cet instantané, pas la définition en stockage. Seule l'ouverture d'un nouveau segment par restauration
  reprend les définitions courantes (`agent-sessions/session#FR-10`).
    - **Source:** user instruction, 2026-10-25
    - **Verified by:** `AC-9`

- **FR-9 - MUST:** Un ContextType est une entité nommée qui vit à un niveau (utilisateur ou projet) et se stocke par un
  trait ; un toolset le référence par son nom, et ce schéma est la seule source du vocabulaire des rôles et paramètres
  partagés.
    - **Source:** agent-sessions#DEC-11

- **FR-10 - MUST:** Un toolset porte une version qui ne change que lorsque son contrat de sortie change, c'est-à-dire les
  signatures des fonctions publiques qu'il expose (nom et paramètres, Rhai ne typant pas statiquement) : mineure pour un
  ajout, majeure pour une rupture. Une modification de documentation ou d'implémentation qui laisse les signatures
  intactes ne change pas la version.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-5`

- **FR-11 - MUST:** La librairie extrait le contrat de sortie d'un toolset de son AST et le met à disposition de l'hôte.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-5`

- **FR-12 - SHOULD:** Au chargement ou à l'enregistrement d'un toolset, la librairie signale une divergence entre le
  contrat extrait et la version déclarée (signature changée sans version changée).
    - **Source:** user instruction, 2026-10-24 ; déduit de FR-11

- **FR-13 - MUST:** Un ContextType décrit ses paramètres par un JSON Schema et ses rôles MCP par une liste de noms,
  obligatoires ou optionnels ; il peut en inclure d'autres ; il porte une version sémantique : l'ajout d'un champ ou d'un
  rôle optionnel est une version mineure, l'ajout d'un champ requis ou un retrait est une version majeure.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-6`

- **FR-14 - MUST:** Un toolset désigne un ContextType par son nom et une version minimale (`cluster@1.2`), compatible avec
  toute version de même majeure égale ou supérieure.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-7`

- **FR-15 - MUST:** Un skill peut porter des documents annexes nommés, stockés avec lui par le même trait et lus à la demande
  par le tool `skill` ; ils ne sont jamais préchargés dans la fenêtre.
    - **Source:** user instruction, 2026-10-24

- **FR-17 - MUST:** Le ToolsetContext de la session est lisible depuis tout code Rhai, scripts du LLM et fonctions des
  modules de toolset compris, par la fonction native `session_context()`, déclarée déterministe auprès de Rhai. Elle rend
  une copie : paramètres résolus (valeurs par défaut des toolsets comprises) et noms des rôles liés, jamais de référence
  de secret. Une modification de la copie n'affecte pas le ToolsetContext.
    - **Source:** user instruction, 2026-10-25 ; Observed in: mesure hors dépôt du 2026-10-25 (`scratchpad`, Faits vérifiés) :
      une constante de scope n'est pas visible depuis un module de toolset, une fonction native l'est
    - **Verified by:** `AC-10`

- **FR-18 - MUST:** Le tool natif `skill` lit les skills sans passer par Rhai : sans argument, il liste les skills disponibles
  (nom et description) ; avec un nom, il rend le corps rendu du skill ; avec un nom et un nom de document, il rend ce
  document annexe. Un skill intégré, toujours disponible, enseigne l'usage de `rhai_pad`, l'introspection et les limites
  de `agent-sessions#CON-4`. Le corps d'un skill et chaque document annexe tiennent dans le plafond de rendu de
  `agent-sessions/scratchpad#FR-5` : un dépassement est refusé à l'enregistrement du toolset, ce qui garde valable la
  réserve `T` de `agent-sessions/context-window#FR-2`.
    - **Source:** user instruction, 2026-10-25 ; agent-sessions#DEC-1
    - **Verified by:** `AC-11`

## Questions ouvertes


## Acceptation

- **AC-1:** Un toolset dépendant d'un module non accordé fonctionne, et ce module reste absent de l'introspection.
- **AC-2:** Le même toolset lié à deux ToolsetContext rend des prompts et des skills qui diffèrent uniquement par leurs
  valeurs ; une section conditionnelle sur un rôle absent disparaît.
- **AC-3:** Sur un environnement sans le rôle `k8s`, le sous-agent qui l'exige n'est pas listé et le démarrage réussit.
- **AC-4:** Une définition d'agent dont un toolset exige le type `cluster` est signalée incompatible avec un
  ToolsetContext qui ne fournit pas ses rôles, avant tout démarrage de session.
- **AC-5:** Ajouter une fonction à un toolset change son contrat extrait et appelle une version mineure ; changer le
  corps d'une fonction ou sa documentation laisse le contrat extrait identique ; retirer une fonction appelle une version
  majeure.
- **AC-6:** Un type qui en inclut un autre exige les champs et rôles des deux ; ajouter un champ optionnel à un type
  appelle une version mineure, en ajouter un requis une version majeure.
- **AC-7:** Un toolset qui exige `cluster@1.2` est compatible avec un contexte de type en 1.3, et incompatible avec 1.1
  et avec 2.0.
- **AC-8:** Une définition qui accorde le module de création de toolsets à un sous-agent, ou à un agent principal doté de
  sous-agents délégables, est refusée au chargement avec un message qui nomme la règle.
- **AC-9:** Un toolset de sous-agent modifié en stockage pendant un segment n'affecte pas les délégations suivantes du même
  segment ; après restauration, la délégation utilise la version modifiée et le message système de restauration la signale.
- **AC-10:** Une fonction d'un module de toolset et un script du LLM lisent la même valeur par `session_context()` ; un script
  qui modifie la copie rendue ne change pas la valeur lue ensuite, et aucun processus ne panique.
- **AC-11:** Sans aucun appel à `rhai_pad`, le tool `skill` liste les skills, rend le skill intégré sur `rhai_pad` et rend un
  document annexe ; l'enregistrement d'un skill dont le corps dépasse le plafond de rendu est refusé.
- **AC-12:** Le prompt système rendu contient le nom de chaque skill des toolsets accordés, sans leur description ni leur
  corps ; une session peut donc lire un skill nommé au premier tour sans appeler `skill` en découverte.
- **AC-13:** Un toolset enregistré par le module de création n'est visible d'aucune session tant que l'hôte ne l'a pas activé ;
  aucune fonction du module ne permet l'activation.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
