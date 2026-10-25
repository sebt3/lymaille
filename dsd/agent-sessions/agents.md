# Solution Design: Agents

## Objectif

Définir ce qu'est un agent (principal ou sous-agent) et un projet, et organiser le mapping entre fournisseurs, modèles
détectés et agents.

## Contexte applicable du domaine

`agent-sessions#BR-2` : la définition d'agent est la seule frontière de capacités. `agent-sessions#BR-4` : le projet est
un niveau. `agent-sessions#CON-1` : toutes les définitions se stockent par traits. `agent-sessions#CON-2` : `rig`
fournit l'abstraction des fournisseurs.

## Exigences fonctionnelles

- **FR-1 - MUST:** Une définition d'agent comprend : identifiant, nature (principal ou sous-agent), prompt système
  templaté, modèle et paramètres, modules et toolsets accessibles, droits de la Memory par niveau, limites de sandbox,
  stratégie de fenêtre de contexte et, pour un principal, ses sous-agents délégables ; pour un sous-agent, le schéma de
  sa sortie.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** Le modèle (désigné par un nom de profil, FR-8) et son paramétrage font partie de la définition
  d'agent.
    - **Source:** user instruction, 2026-10-24

- **FR-3 - SHOULD:** Une définition d'agent est sérialisable en un format simple et éditable à la main (frontmatter
  structuré et corps de prompt).
    - **Source:** user instruction, 2026-10-24

- **FR-4 - MUST:** Un projet regroupe des Memory et un manifeste d'agents principaux, de sous-agents, de toolsets, de
  ContextType et de définitions de ToolsetContext prédéterminés, stockés par trait.
    - **Source:** user instruction, 2026-10-24

- **FR-5 - MUST:** Au démarrage, l'utilisateur choisit un agent principal et un ToolsetContext ; la session sait ainsi
  quel environnement elle pilote.
    - **Source:** agent-sessions#BO-3
    - **Verified by:** `AC-1`

## Mapping fournisseur, modèle et agent

Les fournisseurs exposent des modèles détectables, mais les informations qu'ils fournissent (taille de fenêtre,
capacités, tokenisation) ne sont pas toujours fiables : une couche de surcharge s'intercale entre ce qui est détecté
et ce que l'agent consomme.

- **FR-6 - MUST:** Un fournisseur est une entité nommée (type de fournisseur `rig`, adresse et authentification par
  références) qui vit à un niveau, utilisateur par défaut, et se stocke par un trait.
    - **Source:** user instruction, 2026-10-24

- **FR-7 - MUST:** Un profil de modèle est une entité nommée, de niveau utilisateur ou projet et stockée par un trait,
  qui désigne un fournisseur et un modèle et peut surcharger les métadonnées détectées : taille maximale de contexte,
  support des appels d'outils, paramètres d'échantillonnage, indice de tokenisation, nombre maximal de requêtes simultanées, mode de rejeu du raisonnement. Le profil résolu est la détection
  recouverte par les surcharges ; la surcharge l'emporte toujours.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-8 - MUST:** Une définition d'agent désigne son modèle par le nom d'un profil de modèle et ne référence jamais
  un fournisseur directement.
    - **Source:** user instruction, 2026-10-24

## Questions ouvertes

- **TBD:** détection des modèles. `list_models` existe dans `rig` 0.43 sur les clients OpenAI, Anthropic, Ollama, Copilot et
  Gemini (Observed in: rig-core 0.43.0, `src/client`), et `ModelInfo` porte `context_length` et `max_output_tokens`
  optionnels. vLLM annonce `max_model_len` dans `/v1/models` (262 144 pour `qwen3.8-flash-next`, Observed in:
  spikes/inference-probe) ; que `rig` le reporte dans `context_length` reste à vérifier, sinon le profil de modèle le
  surcharge (FR-7). Comportement quand un modèle référencé n'est pas détecté : à décider.
  Une passerelle de l'écosystème annonce `context_length: 8192` pour presque tous ses modèles, dont `qwen3.8-flash-next` que
  vLLM déclare à 262 144 (signalé : `kydah/kydah-ai-proxy#113`) : la détection ne suffit pas, la surcharge du profil
  (FR-7) est nécessaire, pas seulement prévue.

## Acceptation

- **AC-1:** Une session démarrée avec l'agent A et le ToolsetContext C rend un prompt système contenant les valeurs de
  C, et n'offre que les toolsets de A.
- **AC-2:** Un profil qui surcharge la taille de contexte annoncée par le fournisseur est celle qu'utilise le calcul de
  compaction de `agent-sessions/context-window#FR-2`.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
