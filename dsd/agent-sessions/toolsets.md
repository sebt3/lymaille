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

- **FR-5 - MUST:** Un ToolsetContext est une entité nommée qui vit à un niveau (utilisateur, partagé ou session) et se
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
  accordable qu'à un agent de configuration (`agent-sessions#BR-8`) : la librairie refuse, au chargement ou à l'enregistrement,
  une définition d'agent qui l'accorde à un agent de travail ou à un sous-agent. Les sessions des
  agents de configuration sont exclues des candidatures à l'amélioration continue (`agent-sessions#DEC-13`). Ce que
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

- **FR-9 - MUST:** Un ContextType est une entité nommée qui vit à un niveau (utilisateur ou partagé) et se stocke par un
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

- **FR-19 - MUST:** Un agent de configuration reçoit trois familles de modules, accordés par sa définition (`agent-sessions#BR-2`) :
  `contexts` (`check(agent, contexte)` rend les écarts de compatibilité structurelle ; `resolve(agent, contexte)` rend les valeurs (défauts des toolsets de l'agent compris)
  résolues et les noms des rôles liés, jamais de référence), `inspect` (`open_context(agent, contexte)`, `open_session(id)`,
  `close()`, `target()` : une seule cible d'inspection à la fois, une nouvelle ouverture remplace la précédente) et la lecture
  des sessions passées (liste, signaux de qualité par run, erreurs, fonctions définies par le LLM), dont les sorties sont
  masquées par le Redactor. Aucun de ces modules n'écrit : les écritures de l'agent de configuration passent par le module
  de création de toolsets (FR-8).
    - **Source:** user instruction, 2026-10-29 ; agent-sessions#BR-8
    - **Verified by:** `AC-14`

- **FR-20 - MUST:** Le nom d'un ContextType est un identifiant non vide de lettres ASCII, chiffres, tirets et soulignés
  (`[A-Za-z0-9_-]+`), majuscules comprises : un hôte y porte un préfixe réservé de son catalogue (`G_cluster`) que l'identifiant
  soit lisible comme variable Rhai (`_`) ou non (`-`). Les noms qui ne diffèrent que par la casse sont deux types distincts.
  Aucun mécanisme d'espace de noms n'est ajouté : le préfixe est une convention de l'hôte.
    - **Source:** `sebt3/lymaille#5` point 1 (hôte miryad-core : le `-` est exclu des identifiants Rhai, le `_` y est valide),
      2026-11-03 ; absence d'espace de noms, user instruction, 2026-11-03
    - **Verified by:** `AC-15`

- **FR-21 - MUST:** Un ContextType peut porter, en option, le nom de l'agent principal par défaut des sessions ouvertes sur un contexte de
  ce type, et la nature de ces sessions (travail ou configuration ; jamais sous-agent, qui est lié à la session qui le délègue). Absents,
  l'hôte décide seul. La librairie fournit l'aide qui résout ce choix contre un catalogue d'agents (`resolve_start`) : elle rend l'agent et la nature
  de la session, ou refuse un agent absent du catalogue, un sous-agent, ou une nature qui ne correspond pas au genre de l'agent
  (`agent-sessions/agents#FR-5`) ; quand le type ne porte pas d'agent, elle ne rend rien et l'hôte décide seul. Ils ne sont pas hérités par inclusion (le type qui
  inclut un autre garde les siens) et ne comptent pas dans la version du type : en changer n'est ni mineur ni majeur
  (`bump_since` ne regarde que les champs et les rôles).
    - **Source:** `sebt3/lymaille#4` et `#5` point 3, user instruction, 2026-11-04
    - **Verified by:** `AC-16`

- **FR-22 - SHOULD:** Sous la feature Cargo optionnelle `ts-rs` (hors des features par défaut), les types de définition de ce
  domaine dérivent `TS` : le toolset (avec ses skills, exigences de type, paramètres et rôles déclarés), le ContextType (avec ses
  rôles) et le ToolsetContext (avec ses niveaux). La sérialisation serde ne change pas ; la déclaration TypeScript d'un type
  correspond à sa forme serde. La librairie n'écrit aucun fichier : l'hôte appelle l'export de `ts-rs`.
    - **Source:** `sebt3/lymaille#10`, 2026-11-13 ; `agent-sessions#NFR-4`
    - **Verified by:** `AC-17`

## Questions ouvertes

- **Question (non bloquante):** un nom d'agent par défaut à espace de bord (`ops `) est accepté tel quel par le ContextType (seuls le vide et le blanc sont refusés, FR-21) ; la résolution contre le catalogue échoue alors avec une erreur explicite (agent absent). Faut-il le normaliser ou le refuser plus tôt ? Cas limite relevé à la validation du 2026-11-04, volontairement non traité : aucun hôte ne l'a rencontré (approche XP). À reprendre en issue si un hôte le rencontre ; ne bloque pas la livraison.
    - **Source:** validation de `context_type.sdd` #D1, 2026-11-04 ; user instruction, 2026-11-04


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
- **AC-8:** Une définition qui accorde le module de création de toolsets à un sous-agent ou à un agent de travail, est refusée au chargement avec un message qui nomme la règle.
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

- **AC-14:** Un agent de configuration ouvre un contexte qui lie le rôle `grafana`, puis `contexts::check` d'un agent de
  travail contre ce contexte rend la liste des écarts sans exécuter de fonction ; la session de configuration lit les erreurs d'une
  session passée sans que le texte ne contienne un secret déclaré.
- **AC-15:** Un ContextType nommé `G_cluster-2` est accepté et retrouvé par ce nom exact, `g_cluster-2` en est un autre ; un nom vide ou
  qui contient une espace, un point ou un caractère non ASCII est refusé.

- **AC-16:** Un ContextType construit avec l'agent par défaut `ops` et la nature `config` les rend ; sans eux il rend rien ; un nom
  d'agent vide ou blanc et la nature sous-agent sont refusés ; le type se sérialise et se relit à l'identique, et un type enregistré
  avant l'ajout des champs se relit sans eux ; deux types qui ne diffèrent que par ces champs sont différents mais `bump_since` rend aucune
  nature de changement ; l'aplatissement d'un type qui en inclut un autre ne reprend pas les champs de l'inclus. La résolution contre un
  catalogue rend l'agent et la nature (celle du type, sinon celle du genre de l'agent) ; elle refuse un agent absent du catalogue, un
  sous-agent et une nature contradictoire avec le genre de l'agent, et ne lit pas le catalogue quand le type ne porte pas d'agent.

- **AC-17:** Avec la feature `ts-rs`, un test de contrat atteste que la déclaration TypeScript de `Toolset`, `Skill`, `TypeRequirement`,
  `ParamDecl`, `RoleDecl`, `ContextType`, `RoleSpec`, `ToolsetContext` et `ContextLevel` décrit exactement les clés de la sérialisation
  serde d'une valeur complète et d'une valeur minimale ; la sérialisation est inchangée avec et sans la feature.

## Réalisation SpecDD

Specs dans `src/toolsets/`, implémentées sauf mention contraire (des tâches de durcissement restent listées dans les `Tasks` de certaines specs) :

- `toolsets.sdd` : spec de répertoire.
- `version.sdd`, `contract.sdd` : exigences de version et contrat de sortie extrait de l'AST, nature d'un changement (FR-10, FR-13, FR-14).
- `context_type.sdd`, `toolset_context.sdd` : types de contexte versionnés et inclusion, ToolsetContext sans secret, valeurs résolues (FR-3, FR-4, FR-9, FR-20, FR-21).
- `skill.sdd`, `toolset.sdd` : skills `SKILL.md` et déclaration d'un toolset (FR-1, FR-7, FR-15, FR-22).
- `template.sdd` : rendu Handlebars du prompt et des skills (FR-6).
- `store.sdd` : traits de stockage, stockage mémoire, cascade (FR-8, FR-16).
- `compat.sdd` : compatibilité structurelle, sous-agents retirés signalés par `Delegation`, règle du module de création (FR-4, FR-8, AC-3, AC-4, AC-8).
- `assemble.sdd` : construction des modules, toolsets et skills accordés, dépendances privées (FR-2, AC-1).
- `start.sdd` : résolution de l'agent par défaut et de la nature de session d'un type de contexte contre le catalogue d'agents (FR-21, AC-16).
- `config.sdd` : modules de l'agent de configuration `contexts`, `inspect`, `sessions` et événement `TargetOpened` (FR-19, BR-8).
- `native.sdd` : tool `skill` et fonction `session_context()` (FR-17, FR-18, AC-10, AC-11).
- `error.sdd` : erreur typée du module.

État au 2026-11-01 : tout est livré. Le tool `skill` et `session_context()` (`native.sdd`), la validation JSON Schema complète des valeurs du contexte (`toolsets.sdd` #3, FR-4, `jsonschema` admis le 2026-10-30) et l'analyseur de frontmatter des skills (`Skill::parse`, `yaml_serde` admis le 2026-10-31) ; les entrées sont bornées (texte de skill ou de définition d'agent : 256 Kio ; schéma de type de contexte : 64 Kio, compilé une seule fois) et `session_context()` comme le rendu des skills masquent les secrets déclarés par le `Redactor` de la session. Le skill intégré sur `rhai_pad` est revérifié contre le vrai pad puis réécrit (`native.sdd` #2 et #4, lot P2) ; le tool `skill` est branché dans la boucle de tour (`runtime/turn.sdd` #6, `session/native_tool.sdd`, lot P1). Le module de création de toolsets (FR-8) est un module de l'hôte ou du domaine `modules`, hors de la librairie (décision du 2026-10-31, `compat.sdd` #2) ; la librairie en tient la règle d'accord (`compat.sdd`, `../agents/definition.sdd`) et le nom réservé (`../agents/definition.sdd` #13).

État au 2026-11-04 (lot 0.2.0) : livré, `FR-20` (alphabet des noms de ContextType) et `FR-21` (agent par défaut et nature de session portés par le ContextType, aide `resolve_start` dans `start.sdd`, `AC-15` et `AC-16`) ; le niveau `Shared` des ToolsetContext.
