# Solution Design: Toolsets

## Objectif

Permettre de composer un espace de travail réutilisable (fonctions Rhai et skills associés) et de l'adapter à un
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
    - **Source:** user instruction, 2026-10-24
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

- **FR-6 - MUST:** Le prompt système et le texte des skills sont des templates Handlebars rendus avec les données du
  ToolsetContext et des métadonnées de session figées au rendu (identifiant d'agent, date de début, toolsets accordés avec leur
  description et, pour un agent principal, capacité de sous-agents parallélisables, `agent-sessions/delegation#FR-11`),
  y compris des sections conditionnelles selon la présence d'un rôle. Le prompt système est rendu une
  seule fois, à la création ou à la restauration de la session ; aucun état changeant en cours de session (todo, Memory,
  scope) n'y entre.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-7 - MUST:** Un skill est un objet — métadonnées de frontmatter et corps markdown (une chaîne) — compatible avec le
  format `SKILL.md` de la spécification Agent Skills : `name` (1 à 64 caractères : minuscules, chiffres et tirets) et
  `description` (1 à 1024 caractères) obligatoires, `license`, `compatibility` et `metadata` optionnels. Il est rattaché
  à un toolset, disponible tant que le toolset l'est, découvrable par introspection (nom et description) ; son corps se
  lit à la demande et n'est jamais préchargé dans la fenêtre. Aucun champ de skill n'accorde de capacité : le champ
  `allowed-tools` est ignoré.
    - **Source:** user instruction, 2026-10-24 (y compris `allowed-tools` ignoré) ;
      https://agentskills.io/specification (consultée le 2026-10-24)

- **FR-8 - MUST:** Un module permet à un LLM de créer et modifier des toolsets via un trait de stockage, jamais ceux de
  sa propre session.
    - **Source:** user instruction, 2026-10-24

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

## Questions ouvertes

- **TBD:** fichiers annexes d'un skill (`references/`, `scripts/`, `assets/` de la spécification Agent Skills) : le skill
  étant un objet et non un répertoire, l'équivalent (documents annexes lus à la demande) reste à définir.
- **TBD:** la `description` du frontmatter est-elle aussi rendue par Handlebars ?
- **TBD:** faisabilité de la résolution privée des dépendances dans `vynil-core`.
- **TBD:** forme d'un ContextType : champs typés, valeurs par défaut, composition de types, et règle d'évolution d'un type
  sans casser les toolsets qui l'exigent.

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

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
