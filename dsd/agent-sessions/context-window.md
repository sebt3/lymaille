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

- **FR-2 - MUST:** La compaction se déclenche dès que `MAX - PROMPT - SESSION - 2 * OUT < 0`, avec :
    - `MAX` : la taille maximale de contexte du modèle, issue du profil de modèle résolu ;
    - `PROMPT` : la taille du prompt de demande de résumé ;
    - `SESSION` : la taille courante de la session envoyée au modèle ;
    - `OUT` : le maximum entre le plafond de rendu d'une sortie de `rhai_pad` (en caractères, converti en tokens à raison de
      2,5 caractères par token par défaut, valeur surchargeable) et la taille maximale d'une réponse du LLM.

  La condition est évaluée après chaque message ajouté à la session. Elle garantit qu'un dernier ajout de taille `OUT`
  laisse la place de demander le résumé et d'en recevoir la réponse.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-2`

- **FR-3 - MUST:** La production du résumé et le choix de ce qui est compacté passent par des traits remplaçables par
  l'hôte (un résumeur et une stratégie).
    - **Source:** user instruction, 2026-10-24 (stratégies avancées de gestion de contexte)

- **FR-4 - SHOULD:** Le résumé de compaction porte un digest minimal de l'état hors fenêtre (nombre de tâches ouvertes,
  nombre de Memory) sans en reprendre le contenu, afin que le LLM se souvienne qu'ils existent.
    - **Source:** user instruction, 2026-10-24

- **FR-5 - MUST NOT:** Le contenu de la todo et des Memory n'est pas réinjecté dans la fenêtre à chaque tour.
    - **Source:** user instruction, 2026-10-24

- **FR-6 - MUST:** Si `MAX - PROMPT - 2 * OUT` ne laisse pas la place au prompt système et aux définitions de base, la
  session ne démarre pas et un message explicite indique la cause.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-3`

- **FR-7 - MUST:** La stratégie fournie par défaut élague d'abord les anciens résultats de `rhai_pad`, puis fait résumer par
  le LLM de l'agent tout ce qui précède les derniers messages, qui restent intacts ; d'autres stratégies se branchent
  par le trait de FR-3.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-8 - MUST:** Les estimations locales sont majorées de 10 % dans le calcul de `SESSION` de FR-2 ; l'usage rapporté par le
  fournisseur ne l'est pas. La valeur est surchargeable par définition d'agent.
    - **Source:** user instruction, 2026-10-24

- **FR-9 - MUST:** Le raisonnement (`thinking`) fait partie de l'historique stocké. Un profil de modèle déclare son mode de
  rejeu : aucun, intra-tour (entre appels d'outils d'un même tour) ou tous les tours (`preserve_thinking`). Le raisonnement
  rejoué compte dans `SESSION` et est fourni au résumeur, qui en tire les décisions et leurs raisons ; non rejoué, il
  n'est conservé que pour l'audit.
    - **Source:** user instruction, 2026-10-24 ; https://github.com/earendil-works/pi/issues/3325 ;
      https://docs.fireworks.ai/guides/reasoning.md
    - **Verified by:** `AC-5`

## Questions ouvertes

- **TBD:** transmission du rejeu du raisonnement par `rig` : champ `reasoning_content` des messages ou paramètre de gabarit
  (`chat_template_kwargs`), selon le serveur d'inférence ; à vérifier dans `rig`.
- **TBD:** nombre de derniers messages conservés intacts par la stratégie par défaut (FR-7).
- **TBD:** interaction avec le cache de préfixe des fournisseurs (stabilité de l'ordre des messages).

## Acceptation

- **AC-1:** Avec un fournisseur simulé rapportant des usages connus, le taux de remplissage exposé à l'hôte correspond
  à la valeur attendue.
- **AC-2:** Avec `MAX`, `PROMPT` et `OUT` connus, la compaction ne se déclenche pas tant que
  `MAX - PROMPT - SESSION - 2 * OUT >= 0` et se déclenche dès que cette valeur devient négative ; elle ne touche ni au
  scratchpad ni à la todo, et le résumé contient le digest de FR-4 s'il est retenu.
- **AC-3:** Avec un modèle dont `MAX - PROMPT - 2 * OUT` est inférieur à la taille du prompt système, la session refuse de
  démarrer avec un message qui nomme la cause.
- **AC-4:** Une compaction par la stratégie par défaut supprime d'abord les anciens résultats de `rhai_pad`, laisse intacts
  les derniers messages, et remplace le reste par un résumé produit par le LLM de l'agent.
- **AC-5:** Avec un profil en mode `preserve_thinking`, le raisonnement des tours précédents est renvoyé au modèle, compté
  dans `SESSION` et fourni au résumeur ; avec le mode aucun, il est stocké mais absent de la requête.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
