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
  fournisseur fait foi ; une estimation locale le remplace quand il est absent ou manifestement incohérent, et couvre
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

## Questions ouvertes

- **TBD:** critère d'un usage rapporté « manifestement incohérent » (FR-1).
- **TBD:** catalogue des stratégies (résumé simple, résumé glissant, élagage des sorties d'outils, autres).
- **TBD:** session non compactable par construction : si `MAX - PROMPT - 2 * OUT` ne laisse pas de place au prompt système
  et aux définitions de base, la compaction ne peut jamais aboutir. Proposition : refuser le démarrage avec un message
  explicite.
- **TBD:** marge d'erreur de l'estimation locale dans la condition de FR-2.
- **TBD:** interaction avec le cache de préfixe des fournisseurs (stabilité de l'ordre des messages).

## Acceptation

- **AC-1:** Avec un fournisseur simulé rapportant des usages connus, le taux de remplissage exposé à l'hôte correspond
  à la valeur attendue.
- **AC-2:** Avec `MAX`, `PROMPT` et `OUT` connus, la compaction ne se déclenche pas tant que
  `MAX - PROMPT - SESSION - 2 * OUT >= 0` et se déclenche dès que cette valeur devient négative ; elle ne touche ni au
  scratchpad ni à la todo, et le résumé contient le digest de FR-4 s'il est retenu.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
