# Solution Design: Agents

## Objectif

Définir ce qu'est un agent (principal ou sous-agent) et un projet, et organiser le mapping entre fournisseurs, modèles
détectés et agents.

## Contexte applicable du domaine

`agent-sessions#BR-2` : la définition d'agent est la seule frontière de capacités. `agent-sessions#BR-4` : le projet est
un niveau. `agent-sessions#CON-1` : toutes les définitions se stockent par traits. `agent-sessions#CON-2` : `rig`
fournit l'abstraction des fournisseurs.

## Exigences fonctionnelles

- **FR-1 - MUST:** Une définition d'agent comprend : identifiant, nature (`Work`, `Config` ou `Sub`, voir `agent-sessions#BR-8`), prompt système
  templaté, modèle et paramètres, modules et toolsets accessibles, droits de la Memory par niveau, limites de sandbox,
  stratégie de fenêtre de contexte et, pour un agent de travail, ses sous-agents délégables ; pour un sous-agent, le schéma de
  sa sortie. La nature impose des règles de validation : un agent de travail n'a ni module de création de toolsets ni module
  d'inspection ; un agent de configuration n'a aucun sous-agent délégable ni schéma de sortie, et n'ouvre aucune connexion MCP d'appel (règle du démarrage de session) ; seul lui
  reçoit le module de création de toolsets, les modules d'inspection et de lecture de sessions ; un sous-agent n'a ni sous-agent
  délégable, ni module de création, ni module d'inspection.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** Le modèle (désigné par un nom de profil, FR-8) et son paramétrage font partie de la définition
  d'agent.
    - **Source:** user instruction, 2026-10-24

- **FR-3 - SHOULD:** Une définition d'agent est sérialisable en un format simple et éditable à la main (frontmatter
  structuré et corps de prompt).
    - **Source:** user instruction, 2026-10-24 ; maintenu en SHOULD par le développeur principal, 2026-10-27 (analyse YAML
      par `yaml_serde`)

- **FR-4 - MUST:** Un projet regroupe des Memory et un manifeste d'agents principaux, de sous-agents, de toolsets, de
  ContextType et de définitions de ToolsetContext prédéterminés, stockés par trait.
    - **Source:** user instruction, 2026-10-24

- **FR-5 - MUST:** Au démarrage, l'utilisateur choisit un agent principal (de travail ou de configuration) et un ToolsetContext ; la session sait ainsi
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

- **Résolu (2026-11-01):** détection des modèles : `rig` ne reporte pas `max_model_len` dans `context_length` (mesuré, `spikes/rig-probe`) ; `detect_models` de l'adaptateur (`rig.sdd`) lit `GET <base>/models` lui-même (vLLM : `max_model_len` 262144 pour `qwen3.8-flash-next`, `spikes/rig-e2e`). La passerelle annonce 8192 à tort (`kydah/kydah-ai-proxy#113`) : la surcharge du profil est nécessaire. Texte d'origine : détection des modèles (à vérifier : report de `max_model_len` par `rig`). `list_models` existe dans `rig` 0.43 sur les clients OpenAI, Anthropic, Ollama, Copilot et
  Gemini (Observed in: rig-core 0.43.0, `src/client`), et `ModelInfo` porte `context_length` et `max_output_tokens`
  optionnels. vLLM annonce `max_model_len` dans `/v1/models` (262 144 pour `qwen3.8-flash-next`, Observed in:
  spikes/inference-probe) ; que `rig` le reporte dans `context_length` reste à vérifier, sinon le profil de modèle le
  surcharge (FR-7). Décidé (user instruction, 2026-10-27) : un modèle référencé mais non détecté se résout si son profil porte
  une taille de contexte, sinon la résolution échoue avec une erreur explicite. La réponse maximale par défaut d'un profil
  est le contexte divisé par 8.
  Une passerelle de l'écosystème annonce `context_length: 8192` pour presque tous ses modèles, dont `qwen3.8-flash-next` que
  vLLM déclare à 262 144 (signalé : `kydah/kydah-ai-proxy#113`) : la détection ne suffit pas, la surcharge du profil
  (FR-7) est nécessaire, pas seulement prévue.

## Acceptation

- **AC-1:** Une session démarrée avec l'agent A et le ToolsetContext C rend un prompt système contenant les valeurs de
  C, et n'offre que les toolsets de A.
- **AC-2:** Un profil qui surcharge la taille de contexte annoncée par le fournisseur est celle qu'utilise le calcul de
  compaction de `agent-sessions/context-window#FR-2`.

## Réalisation SpecDD

Specs dans `src/agents/`, implémentées (des tâches de durcissement restent listées dans les `Tasks` de certaines specs) :

- `agents.sdd` : spec de répertoire (définitions, fournisseurs, profils, catalogue, trait d'inférence).
- `definition.sdd` : définition d'agent de nature `Work`, `Config` ou `Sub`, bac à sable, plafonds, empreinte (FR-1, FR-2, BR-8).
- `provider.sdd` : fournisseur d'inférence, adresse et authentification par références (FR-6).
- `profile.sdd` : profil de modèle, surcharge de ce que le fournisseur détecte (FR-7).
- `catalog.sdd` : traits de stockage, stockage mémoire et cascade `Layered` par niveau (FR-5).
- `inference.sdd` : trait du fournisseur d'inférence, classement des erreurs, streaming de fragments, fournisseur scripté.
- `error.sdd` : erreur typée du module.

État au 2026-10-31 : la **sérialisation frontmatter + corps** d'une définition (FR-3, `agents.sdd` #5, `definition.sdd` #18, `yaml_serde`) est livrée. Restent le **manifeste de projet** (FR-4, `agents.sdd` #3, dépend de `toolsets`, maintenant livré) et l'**adaptateur `rig`** (clients, `list_models`, décodage du raisonnement : décision d'architecture du 2026-10-27, pas encore spécifié ni implémenté ; le TBD de détection des modèles ci-dessus en dépend).
