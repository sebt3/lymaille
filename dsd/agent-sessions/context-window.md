# Solution Design: Context Window

## Objectif

Suivre le remplissage de la fenêtre de contexte, déclencher la compaction et rendre les stratégies de gestion de
contexte interchangeables.

## Contexte applicable du domaine

`agent-sessions#DEC-7` : suivi et pas de budget. `agent-sessions#FR-5` : jamais d'interruption sur le nombre de tokens.
L'état de travail (scratchpad, todo, Memory) vit hors de la fenêtre et survit donc à la compaction, ce qui est la
raison d'être de `agent-sessions#BO-4`.

## Exigences fonctionnelles

- **FR-1 - MUST:** La librairie compte les tokens de la session (taux de remplissage de la fenêtre, usage cumulé pour
  l'audit de coût), par agent principal et par sous-agent, et expose ces valeurs à l'hôte. L'usage rapporté par le
  fournisseur fait foi ; une estimation locale le remplace quand il est absent, nul ou inférieur au précédent sans compaction entre-temps, et couvre
  les messages ajoutés depuis le dernier appel.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** Avant chaque requête d'inférence, la compaction se déclenche si `MAX - PROMPT - SESSION - 2R - T < 0`,
  avec :
    - `MAX` : la taille maximale de contexte du modèle, issue du profil de modèle résolu ;
    - `PROMPT` : la taille du prompt de demande de résumé ;
    - `SESSION` : la taille de la session qui serait envoyée au modèle ;
    - `R` : la taille maximale d'une réponse du LLM, raisonnement compris ;
    - `T` : le plafond de rendu d'une sortie de `rhai_pad` ou de `skill` (`agent-sessions/scratchpad#FR-5`, `agent-sessions/toolsets#FR-18`, en caractères), converti en
      tokens à raison de 2,5 caractères par token par défaut, valeur surchargeable.

  Le pire cas couvert est une réponse qui est un appel d'outil (`R`), suivie de son résultat (`T`), puis de la demande de
  résumé (`PROMPT`) et de sa réponse (`R`). Avant une requête, l'historique se termine par un message utilisateur ou un
  résultat d'outil : la compaction ne tombe donc jamais entre un `tool_call` et son résultat.
    - **Source:** user instruction, 2026-10-24 ; révisé le 2026-10-25 (évaluation avant requête, `R` et `T` distincts)
    - **Verified by:** `AC-2`

- **FR-3 - MUST:** La production du résumé et le choix de ce qui est compacté passent par des traits remplaçables par
  l'hôte (un résumeur et une stratégie).
    - **Source:** user instruction, 2026-10-24 (stratégies avancées de gestion de contexte)

- **FR-4 - SHOULD:** Le résumé de compaction porte un digest minimal de l'état hors fenêtre (nombre de tâches ouvertes,
  nombre de Memory) sans en reprendre le contenu, afin que le LLM se souvienne qu'ils existent.
    - **Source:** user instruction, 2026-10-24

- **FR-5 - MUST NOT:** Le contenu de la todo et des Memory n'est pas réinjecté dans la fenêtre à chaque tour.
    - **Source:** user instruction, 2026-10-24

- **FR-6 - MUST:** La session ne démarre pas si `SYS > MAX - PROMPT - 3R - T`, `SYS` étant la taille du prompt système
  rendu et des définitions des tools natifs ; un message explicite indique la cause. Après une compaction, la session vaut au
  plus `SYS + R` : cette condition garantit qu'elle ne redéclenche pas aussitôt la compaction de FR-2.
    - **Source:** user instruction, 2026-10-24 ; révisé le 2026-10-25 (absence de boucle de compaction)
    - **Verified by:** `AC-3`

- **FR-10 - MUST:** Un message utilisateur qui, ajouté à la session vide de tout échange (`SYS` seul), rendrait FR-2
  négative est refusé avec une erreur explicite rendue à l'hôte, sans compaction ni appel au modèle.
    - **Source:** user instruction, 2026-10-25
    - **Verified by:** `AC-6`

- **FR-7 - MUST:** La stratégie fournie par défaut fait résumer par le LLM de l'agent la session intégrale, sans élagage ni
  extrait conservé : le modèle décide de ce qui compte. La session qui suit se compose du prompt système, inchangé,
  suivi de la sortie de la compaction, et marque une frontière de segment (`agent-sessions/session#FR-12`). D'autres
  stratégies se branchent par le trait de FR-3.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-8 - MUST:** Les estimations locales sont majorées de 10 % dans le calcul de `SESSION` de FR-2 ; l'usage rapporté par le
  fournisseur ne l'est pas. La valeur est surchargeable par définition d'agent.
    - **Source:** user instruction, 2026-10-24

- **FR-9 - MUST:** Le raisonnement (`thinking`) fait partie de l'historique stocké. Un profil de modèle déclare son mode de
  rejeu : aucun, intra-tour (entre appels d'outils d'un même tour) ou tous les tours (`preserve_thinking`). Le raisonnement
  rejoué compte dans `SESSION` et est fourni au résumeur, qui en tire les décisions et leurs raisons ; non rejoué, il
  n'est conservé que pour l'audit. Le mode est transmis explicitement au serveur d'inférence
  (`chat_template_kwargs`), jamais déduit du défaut du gabarit, qui varie d'un modèle à l'autre.
    - **Source:** user instruction, 2026-10-24 ; Observed in: spikes/inference-probe ; https://github.com/earendil-works/pi/issues/3325 ;
      https://docs.fireworks.ai/guides/reasoning.md
    - **Verified by:** `AC-5`

## Questions ouvertes

- **Décidé:** la librairie fournit un prompt de résumé par défaut, surchargeable par l'agent ou l'hôte, qui impose au résumeur de
  citer textuellement les demandes de l'utilisateur, seule donnée non régénérable ; le résumeur reçoit du texte et non des
  messages structurés. Le prompt par défaut est livré (`DEFAULT_SUMMARY_PROMPT`, `context/strategy.rs`).
    - **Source:** user instruction, 2026-10-27
- **Mesuré (2026-11-01, `spikes/rig-e2e/README.md`):** sur le vLLM du DGX (`qwen3.8-flash-next`, champ `reasoning`), le rejeu avec `preserve_thinking` passé par la table libre du profil est accepté et améliore la réponse sur une question qui dépend de la pensée précédente (sans rejeu, le modèle consomme le budget en raisonnement et rend un texte vide). Sur la passerelle `smart` (Qwen3.6, `reasoning_content`) le rejeu est accepté et arrive dans le prompt côté serveur (601 jetons contre 124), mais son bénéfice n'est pas démontré (scénario non discriminant). Reste ouvert : le démontrer sur Qwen3.6 avec un budget de sortie assez grand — clos par la décision du 2026-11-01 ci-dessous.
- **Décidé:** pour la 0.1.0, les essais déjà exécutés suffisent : le rejeu du raisonnement est accepté par les deux dorsaux et son bénéfice est démontré sur le vLLM du DGX ; la démonstration discriminante sur Qwen3.6 n'est pas un obstacle à la publication et reste à faire après. Le TBD de l'AC-5 est clos.
    - **Source:** user instruction, 2026-11-01
- **Texte d'origine du TBD :** essai de bout en bout du rejeu du raisonnement, à écrire avec le code comme test d'intégration (AC-5). Mesuré côté
  serveur (`spikes/inference-probe`) : sur vLLM le champ renvoyé est `reasoning`, les deux orthographes sont acceptées en
  entrée, et `preserve_thinking` s'obtient par `chat_template_kwargs` ; sur la passerelle `smart`, Qwen3.6 renvoie
  `reasoning_content`. Lu dans `rig` 0.43, non exécuté : il décode les deux champs, il fusionne `additional_params` à plat
  dans le corps, et il ne rejoue un raisonnement que pour son émetteur. La passerelle `smart` relaie `chat_template_kwargs` et `reasoning_tokens` (mesuré avec
  `qwen3.8-flash-next`) ; reste à vérifier que le backend de Qwen3.6 honore `chat_template_kwargs`.

## Acceptation

- **AC-1:** Avec un fournisseur simulé rapportant des usages connus, le taux de remplissage exposé à l'hôte correspond
  à la valeur attendue.
- **AC-2:** Avec `MAX`, `PROMPT`, `R` et `T` connus, aucune compaction n'a lieu tant que
  `MAX - PROMPT - SESSION - 2R - T >= 0` avant une requête, et elle a lieu avant la requête dès que cette valeur devient
  négative ; un fournisseur simulé qui rend un appel d'outil de taille `R` puis un résultat de taille `T` laisse toujours
  la place à la demande de résumé et à sa réponse. La compaction ne touche ni au scratchpad ni à la todo, et le résumé
  contient le digest de FR-4 s'il est retenu.
- **AC-3:** Avec un modèle dont `MAX - PROMPT - 3R - T` est inférieur à `SYS`, la session refuse de démarrer avec un message
  qui nomme la cause ; à `SYS` égal à cette borne, une compaction suivie d'un résumé de taille `R` ne redéclenche pas de
  compaction.
- **AC-4:** Une compaction par la stratégie par défaut envoie au résumeur la session intégrale (raisonnement compris s'il est
  rejoué) ; la session qui suit se compose du prompt système inchangé puis du résumé, et aucune compaction n'intervient
  entre un `tool_call` et son résultat.
- **AC-5:** Avec un profil en mode `preserve_thinking`, le raisonnement des tours précédents est renvoyé au modèle, compté
  dans `SESSION` et fourni au résumeur ; avec le mode aucun, il est stocké mais absent de la requête.
- **AC-6:** Un message utilisateur trop gros pour tenir avec `SYS` seul est refusé avec une erreur explicite, sans requête au
  fournisseur simulé, et la session reste intacte.

## Réalisation SpecDD

Specs dans `src/context/`, implémentées (des tâches de durcissement restent listées dans les `Tasks` de certaines specs) :

- `context.sdd` : spec de répertoire (seuils, suivi des tokens, mode de rejeu, traits de résumeur et de stratégie).
- `budget.sdd` : seuils de compaction et de démarrage, refus d'un prompt ou d'un message trop gros (FR-1, FR-2, AC-1).
- `usage.sdd` : suivi des tokens, taux de remplissage, usage cumulé, estimation locale (FR-3).
- `replay.sdd` : mode de rejeu du raisonnement et sélection du raisonnement renvoyé (FR-9, AC-5).
- `strategy.sdd` : traits remplaçables du résumé et du choix de ce qui est compacté, stratégie par défaut (FR-4 à FR-8).
- `error.sdd` : erreur typée du module.

État au 2026-11-01 : tout est livré. L'**intégration à la boucle de tour** (`context.sdd` #2) est réalisée par `src/runtime/turn.rs` (refus d'un message trop gros avant tout appel, compaction décidée avant chaque requête) ; le **prompt de résumé par défaut** est livré (`context.sdd` #3, `strategy.sdd` #7, texte validé le 2026-10-31) ; l'**essai de bout en bout du rejeu du raisonnement** (AC-5) est clos par la décision ci-dessus, sur les mesures de `spikes/rig-e2e`.

État au 2026-11-04 (lot 0.2.0) : livré, rejeu et stratégie de compaction alignés sur `Message::Discarded` (`replay.sdd` #5, `strategy.sdd` #P6).
