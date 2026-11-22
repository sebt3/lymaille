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
  templaté, modèle et paramètres, limites de sandbox, stratégie de fenêtre de contexte et, pour un agent de travail, ses sous-agents
  délégables ; pour un sous-agent, le schéma de sa sortie et, facultativement, une restriction de la liste de toolsets du ToolsetContext. Elle ne porte
  ni liste de modules, ni droit de Memory, ni liste de toolsets pour un agent principal : les toolsets viennent du ToolsetContext, les modules natifs de
  la nature de la session (`agent-sessions#BR-2`, `DEC-16`). La nature impose des règles de validation : un agent de configuration n'a aucun sous-agent
  délégable ni schéma de sortie, et n'ouvre aucune connexion MCP d'appel (règle du démarrage de session) ; seul lui reçoit le module de création de
  toolsets et les modules d'inspection (`inspect`), `contexts` et de lecture de sessions ; un sous-agent n'a ni sous-agent délégable ni restriction qui étende la liste du contexte.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** Le modèle (désigné par un nom de profil facultatif, FR-8 et FR-9) et son paramétrage font partie de la
  définition d'agent.
    - **Source:** user instruction, 2026-10-24

- **FR-3 - SHOULD:** Une définition d'agent est sérialisable en un format simple et éditable à la main (frontmatter
  structuré et corps de prompt).
    - **Source:** user instruction, 2026-10-24 ; maintenu en SHOULD par le développeur principal, 2026-10-27 (analyse YAML
      par `yaml_serde`)

- **FR-4 - MUST:** Un projet regroupe des Memory et un manifeste d'agents principaux, de sous-agents, de toolsets, de
  ContextType et de définitions de ToolsetContext prédéterminés, stockés par trait.
    - **Source:** user instruction, 2026-10-24

- **FR-5 - MUST:** Une session démarre sur un contexte de travail (un ToolsetContext, `agent-sessions#BR-9`) : l'agent principal
  (de travail ou de configuration) en découle, et la session sait ainsi quel environnement elle pilote. L'hôte désigne à la
  librairie l'agent et le ToolsetContext ; l'utilisateur ne choisit pas d'agent, et la librairie ne demande aucune approbation en
  cours de session pour élargir ce que le contexte a fixé avant son début. Le type du contexte peut porter le nom de l'agent par
  défaut et la nature de session (`agent-sessions/toolsets#FR-21`) ; la librairie résout ce nom contre le catalogue et refuse un agent inconnu, un sous-agent ou une nature contradictoire (`FR-21` de `toolsets`) ; l'hôte décide seul quand le type ne porte rien.
    - **Source:** agent-sessions#BO-3 ; reformulé, `sebt3/lymaille#4` et `#5`, 2026-11-03 : le texte d'origine disait que
      l'utilisateur choisit un agent principal au démarrage
    - **Verified by:** `AC-1`

## Mapping fournisseur, modèle et agent

Les fournisseurs exposent des modèles détectables, mais les informations qu'ils fournissent (taille de fenêtre,
capacités, tokenisation) ne sont pas toujours fiables : une couche de surcharge s'intercale entre ce qui est détecté
et ce que l'agent consomme.

- **FR-6 - MUST:** Un fournisseur est une entité nommée (type de fournisseur `rig`, adresse et authentification par
  références) qui vit à un niveau (utilisateur par défaut, ou partagé), et se stocke par un trait.
    - **Source:** user instruction, 2026-10-24

- **FR-7 - MUST:** Un profil de modèle est une entité nommée, de niveau utilisateur ou partagé et stockée par un trait,
  qui désigne un fournisseur et un modèle et peut surcharger les métadonnées détectées : taille maximale de contexte,
  support des appels d'outils, paramètres d'échantillonnage, indice de tokenisation, nombre maximal de requêtes simultanées, mode de rejeu du raisonnement. Le profil résolu est la détection
  recouverte par les surcharges ; la surcharge l'emporte toujours.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-8 - MUST:** Une définition d'agent désigne son modèle, quand elle le fait, par le nom d'un profil de modèle (FR-9) et ne
  référence jamais un fournisseur directement.
    - **Source:** user instruction, 2026-10-24

- **FR-9 - MUST:** Le nom du profil de modèle d'une définition d'agent est facultatif. Une définition sans profil est valide et
  se sérialise sans le champ (JSON comme texte à frontmatter) ; une clé absente ou `null` se lit comme l'absence de profil ; une
  définition qui porte un profil se comporte comme avant, et son empreinte est celle qu'elle avait. Un nom de profil présent mais
  vide ou blanc reste refusé. La résolution d'un agent sans profil est l'affaire de l'hôte à l'ouverture de session : il fournit le
  profil par défaut, ou une erreur qui nomme la cause ; la librairie n'appelle jamais la résolution d'un profil avec un nom vide.
    - **Source:** `sebt3/lymaille#11` (miryad-core#75 : agents par défaut qui suivent un profil par défaut modifiable sans réécrire
      chaque agent), 2026-11-13 ; forme (`new` sans profil et `with_profile`, champ omis), user instruction, 2026-11-13
    - **Verified by:** `AC-3`

- **FR-10 - SHOULD:** Sous la feature Cargo optionnelle `ts-rs` (hors des features par défaut), les types de définition de ce
  domaine dérivent `TS` : la définition d'agent et les types qu'elle sérialise, le profil de modèle et le fournisseur. La
  sérialisation serde ne change pas ; la déclaration TypeScript d'un type correspond à sa forme serde. La librairie n'écrit aucun
  fichier : l'hôte appelle l'export de `ts-rs` (`T::export_all_to`).
    - **Source:** `sebt3/lymaille#10`, 2026-11-13 ; pas de fonction d'export fournie, user instruction, 2026-11-13 ;
      `agent-sessions#NFR-4`
    - **Verified by:** `AC-4`

- **FR-11 - MUST:** Un fournisseur d'inférence partagé par `Arc` est lui-même un fournisseur d'inférence : l'hôte partage un même
  fournisseur entre la boucle de tour et un résumeur (`context-window` FR-11) sans écrire d'enveloppe.
    - **Source:** `sebt3/lymaille#17` point 1, 2026-11-17
    - **Verified by:** `AC-5`

- **FR-12 - MUST:** La librairie définit le port d'embedding de texte, le trait `Embedder` (feature `embedder`, `agent-sessions#NFR-5`), au
  même niveau que le fournisseur d'inférence. Il expose l'identifiant du modèle (`model_id`) et la dimension des vecteurs
  (`dimensions`), et embede un lot de textes : autant de vecteurs que d'entrées, dans l'ordre des entrées, tous de la dimension annoncée.
  Un vecteur persisté se stocke avec le `model_id` qui l'a produit (changer de modèle invalide l'index) : c'est le contrat avec les
  stockages de l'hôte. Les échecs sont typés : service indisponible, entrée trop longue, réponse mal formée, dimension inattendue,
  nombre de vecteurs erroné. Aucune de ces erreurs ne montre l'adresse du serveur, la clé ni le texte embedé (`agent-sessions#CON-3`).
    - **Source:** `sebt3/lymaille#18`, 2026-11-20 ; la localité du modèle n'est pas portée (`agent-sessions#DEC-15`, Open Issues du 2026-11-20)
    - **Verified by:** `AC-6`, `AC-7`

- **FR-13 - MUST:** Le client d'embedding de production réutilise la définition de fournisseur (`FR-6`) et le profil de modèle (`FR-7`),
  sans configuration parallèle : l'adresse et l'authentification sont celles du fournisseur, un profil désigne le modèle d'embedding et
  porte sa dimension attendue et la taille maximale d'un lot. Le profil d'un modèle d'embedding ne porte ni taille de contexte de
  discussion ni paramètre d'échantillonnage. L'implémentation s'appuie sur le client d'embedding de `rig` (`agent-sessions#DEC-15`).
    - **Source:** `sebt3/lymaille#18`, 2026-11-20 ; user instruction, 2026-11-20 (`rig` autant que possible)
    - **Verified by:** `AC-6`

- **FR-14 - MUST:** Un lot plus grand que la taille maximale du profil est découpé de façon transparente, l'ordre des vecteurs suivant
  celui des entrées. Une entrée plus longue que ce que le modèle accepte suit une politique de troncature explicite et documentée, jamais
  silencieuse : la troncature se signale à l'appelant par un indicateur `truncated` rendu avec chaque vecteur (décision du 2026-11-22 : une Memory longue reste indexable, et l'hôte sait laquelle l'a été ; la limite se compte en caractères, comme l'indice de tokenisation de `FR-7`). Une implémentation de test déterministe,
  sans réseau, rend un vecteur dérivé du texte : mêmes entrées, mêmes vecteurs ; entrées différentes, vecteurs différents.
    - **Source:** `sebt3/lymaille#18`, 2026-11-20
    - **Verified by:** `AC-6`, `AC-7`

## Questions ouvertes

- **Résolu (2026-11-01):** détection des modèles : `rig` ne reporte pas `max_model_len` dans `context_length` (mesuré, `spikes/rig-probe`) ; `detect_models` de l'adaptateur (`rig.sdd`) lit `GET <base>/models` lui-même (vLLM : `max_model_len` 262144 pour `qwen3.8-flash-next`, `spikes/rig-e2e`). La passerelle annonce 8192 à tort (`kydah/kydah-ai-proxy#113`) : la surcharge du profil est nécessaire. Texte d'origine : détection des modèles (à vérifier : report de `max_model_len` par `rig`). `list_models` existe dans `rig` 0.43 sur les clients OpenAI, Anthropic, Ollama, Copilot et
  Gemini (Observed in: rig-core 0.43.0, `src/client`), et `ModelInfo` porte `context_length` et `max_output_tokens`
  optionnels. vLLM annonce `max_model_len` dans `/v1/models` (262 144 pour `qwen3.8-flash-next`, Observed in:
  spikes/inference-probe) ; `rig` ne le reporte pas dans `context_length` (mesuré le 2026-11-01), si bien que le profil de modèle le surcharge (FR-7). Décidé (user instruction, 2026-10-27) : un modèle référencé mais non détecté se résout si son profil porte
  une taille de contexte, sinon la résolution échoue avec une erreur explicite. La réponse maximale par défaut d'un profil
  est le contexte divisé par 8.
  Une passerelle de l'écosystème annonce `context_length: 8192` pour presque tous ses modèles, dont `qwen3.8-flash-next` que
  vLLM déclare à 262 144 (signalé : `kydah/kydah-ai-proxy#113`) : la détection ne suffit pas, la surcharge du profil
  (FR-7) est nécessaire, pas seulement prévue.

## Acceptation

- **AC-1:** Une session démarrée avec l'agent A et le ToolsetContext C rend un prompt système contenant les valeurs de
  C, et n'offre que les toolsets de A ; l'agent et le contexte viennent de l'hôte, sans choix de l'utilisateur ni approbation en
  cours de session.
- **AC-2:** Un profil qui surcharge la taille de contexte annoncée par le fournisseur est celle qu'utilise le calcul de
  compaction de `agent-sessions/context-window#FR-2`.
- **AC-3:** Une définition construite sans profil est valide, se sérialise sans clé `profile` (JSON et texte), se relit à l'identique
  depuis un JSON sans la clé ou avec `profile: null`, et diffère par son empreinte de la même définition avec un profil ; une
  définition construite avec un profil a le JSON, le texte et l'empreinte qu'elle avait avant ce changement ; un profil vide ou blanc
  est refusé.
- **AC-4:** Avec la feature `ts-rs`, un test de contrat atteste que la déclaration TypeScript de `AgentDefinition`, `ModelProfile`
  et `Provider` (et des types qu'ils contiennent) décrit exactement les clés de la sérialisation serde d'une valeur complète et
  d'une valeur minimale ; sans la feature, `ts-rs` est absent du graphe de dépendances et la sérialisation est inchangée.

- **AC-5:** Un `Arc` autour d'un fournisseur scripté, utilisé comme fournisseur d'inférence, rend les mêmes réponses et les mêmes
  erreurs que le fournisseur nu, et les requêtes restent lisibles par le fournisseur partagé ; le flux de fragments passe aussi par
  l'`Arc`.

- **AC-6:** Avec l'implémentation de test, un lot nominal rend autant de vecteurs que d'entrées, de la dimension annoncée, dans l'ordre ; deux
  appels identiques rendent les mêmes vecteurs et deux entrées différentes des vecteurs différents. Avec un faux serveur qui répond des vecteurs
  d'une autre dimension que celle du profil, ou un nombre de vecteurs erroné, ou un corps illisible, ou qui est injoignable, l'erreur est typée
  distinctement et son texte ne contient ni l'adresse, ni la clé, ni le texte embedé. Un lot dépassant la taille maximale part en plusieurs
  requêtes et rend les vecteurs dans l'ordre des entrées.
- **AC-7:** Une entrée plus longue que la limite du modèle suit la politique de troncature documentée et la signale ; la même entrée n'est jamais
  tronquée en silence.
- **AC-8:** Sans la feature `embedder`, ni le trait `Embedder`, ni son client, ni la recherche sémantique ne sont compilés et le comportement des features
  par défaut est inchangé ; avec la feature, ils le sont.

## Réalisation SpecDD

Specs dans `src/agents/`, implémentées (des tâches de durcissement restent listées dans les `Tasks` de certaines specs) :

- `agents.sdd` : spec de répertoire (définitions, fournisseurs, profils, catalogue, trait d'inférence).
- `definition.sdd` : définition d'agent de nature `Work`, `Config` ou `Sub`, profil facultatif, bac à sable, plafonds, empreinte (FR-1, FR-2, FR-9, FR-10, BR-8).
- `provider.sdd` : fournisseur d'inférence, adresse et authentification par références (FR-6, FR-10).
- `profile.sdd` : profil de modèle, surcharge de ce que le fournisseur détecte (FR-7, FR-10).
- `catalog.sdd` : traits de stockage, stockage mémoire et cascade `Layered` par niveau (FR-5).
- `inference.sdd` : trait du fournisseur d'inférence, classement des erreurs, streaming de fragments, fournisseur scripté, partage par `Arc` (FR-11).
- `summarizer.sdd` : résumeur de production au-dessus d'un fournisseur d'inférence (`context-window` FR-11).
- `embedder.sdd` (lot 0.4.0, à réaliser) : trait `Embedder`, erreurs typées, lots, troncature, implémentation de test, client de production sur `rig` et adaptateur vers le modèle d'un index de `rig` (FR-12 à FR-14, AC-6 à AC-8).
- `error.sdd` : erreur typée du module.

État au 2026-11-01 : tout est livré. La **sérialisation frontmatter + corps** d'une définition (FR-3, `agents.sdd` #5, `definition.sdd` #18 et #19, `yaml_serde`) ; le **manifeste de projet** (FR-4, `agents.sdd` #3, `manifest.sdd`, lot M1) ; l'**adaptateur `rig`** (`rig.sdd`, lots R0 à R3, derrière la feature Cargo `rig`) : clients OpenAI compatibles, `list_models` et détection des modèles, décodage et rejeu du raisonnement. Le TBD de détection des modèles est levé par les mesures de `spikes/rig-probe` et `spikes/rig-e2e`.

État au 2026-11-04 (lot 0.2.0) : livré, `FR-5` (le contexte de travail, non l'agent, sélectionne la session) et le niveau `Shared` des fournisseurs et profils (alias de lecture `project`).
