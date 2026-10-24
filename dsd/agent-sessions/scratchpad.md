# Solution Design: Scratchpad

## Objectif

Fournir le moteur d'exécution Rhai lié à la session : un scope persistant, le tool natif unique, l'introspection, les
limites de sandbox et le rendu borné des sorties.

## Contexte applicable du domaine

`agent-sessions#FR-1` (un seul tool natif) et `agent-sessions#FR-2` (introspection avec recherche) sont réalisées ici.
`agent-sessions#BR-2` s'applique : les modules accessibles à l'exécution sont ceux de la définition d'agent, et aucune
fonction définie par le LLM n'élargit cet ensemble. `agent-sessions#CON-4` décrit les limites de Rhai sur les fonctions
et closures. `agent-sessions#CON-2` place le moteur dans `vynil-core`.

## Exigences fonctionnelles

- **FR-1 - MUST:** Chaque appel du tool `rhai_pad` exécute un script Rhai dans le scope de la session ; les variables sont
  conservées d'un appel à l'autre.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-2 - MUST:** Les fonctions définies par le LLM sont conservées sous forme de texte source et réutilisables dans
  les appels suivants ; leur persistance suit `agent-sessions/session#FR-3`.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-1`

- **FR-3 - MUST:** L'introspection expose les variables du scope, les modules accordés, leurs fonctions avec signature et
  documentation, les toolsets et les skills, avec une recherche lexicale sur noms, documentations et titres.
    - **Source:** agent-sessions#FR-2
    - **Verified by:** `AC-2`

- **FR-4 - MUST:** Le moteur conserve les plafonds de profondeur de `vynil-core` (expression 256 et 128, appels 512) et
  n'impose aucune limite de durée ni de nombre d'opérations à un script : une opération longue, comme une compilation
  lancée dans un sandbox MCP, est légitime, un dépassement de délai n'est qu'un événement (`agent-sessions#FR-5`), et
  l'arrêt d'une boucle infinie est l'annulation par l'hôte. Il borne en revanche la mémoire, par défaut à 16 Mio par
  chaîne, 1 000 000 d'éléments par tableau et 100 000 entrées par map, valeurs surchargeables par définition d'agent :
  ces garde-fous protègent le processus hôte et ses autres sessions d'un emballement.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-3`, `AC-7`

- **FR-5 - MUST:** Le résultat d'un script est une valeur `Dynamic` (chaîne ou objet structuré) dont le rendu renvoyé au
  LLM est plafonné en caractères, à 8 000 par défaut, valeur surchargeable par définition d'agent. Une sortie tronquée
  conserve son début et sa fin, porte au milieu un avis explicite indiquant la troncature et la taille d'origine, et
  invite à extraire la partie utile depuis une variable.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-6 - MUST:** Une erreur Rhai (syntaxe, exécution, limite) est rendue au LLM avec sa position et un message
  exploitable.
    - **Source:** agent-sessions, section Risques

- **FR-7 - MUST:** À la fin d'un script, si une variable persistante contient une closure, un pointeur de fonction ou une
  valeur non sérialisable, le rendu porte un avertissement qui nomme la variable et suggère de définir une `fn`, et la
  valeur est exclue de la persistance ; le script n'est pas rejeté. Les closures passagères ne sont pas concernées.
    - **Source:** user instruction, 2026-10-24 ; agent-sessions#CON-4
    - **Verified by:** `AC-8`

- **FR-8 - MUST:** La librairie met à disposition du LLM des fonctions de formatage (conversion d'un `Dynamic` en texte,
  JSON ou tableau, extraction, troncature) ; le format de ce qui revient dans la session reste le choix du LLM.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-5`

- **FR-9 - MUST:** Les limites de `agent-sessions#CON-4` sont exposées au LLM (prompt système ou skill intégré accessible
  par introspection), avec la façon de les contourner : passer les données en argument, et lire les constantes par
  `global::`.
    - **Source:** agent-sessions#CON-4
    - **Verified by:** `AC-6`

- **FR-10 - MUST:** Un appel MCP ou d'inférence en cours depuis un script est abandonné à l'annulation du run, sans
  attendre sa fin.
    - **Source:** agent-sessions#FR-4 ; Observed in: spikes/rhai-1-25 (`persistence`, S5)
    - **Verified by:** `AC-9`

## Décisions

- **DEC-1 - Persistance par source des définitions, pas par rejeu**
    - **Context:** les fonctions Rhai vivent dans l'AST et non dans le Scope ; l'AST n'est pas sérialisable (à vérifier).
    - **Decision:** persister les variables par serde et les fonctions par leur texte source ; ne jamais rejouer les
      scripts exécutés.
    - **Consequences:** un rejeu répéterait les effets de bord (appels MCP) ; la recompilation peut échouer
      (`agent-sessions/session#FR-3`).
    - **Source:** user instruction, 2026-10-24

- **DEC-2 - Pont async/sync de vynil-core, Rhai en feature `sync`**
    - **Context:** rig est asynchrone, Rhai exécute de façon synchrone ; `vynil-core` compile déjà Rhai avec `sync`,
      `serde` et `internals` et fournit un pont `block_on` (Observed in: vynil-core `Cargo.toml`, `src/rt.sdd`).
    - **Decision:** réutiliser cette feature et ce pont pour les appels MCP et d'inférence faits depuis un script.
    - **Consequences:** moteur et valeurs sont `Send + Sync` ; un script occupe un thread du runtime pendant ses attentes ;
      l'hôte fournit un runtime multi-thread (`agent-sessions#CON-6`) ; le pont ne sait pas annuler, il faut un hook de
      progression (voir Questions ouvertes).
    - **Source:** user instruction, 2026-10-24 (feature `sync`) ; Observed in: vynil-core

## Faits vérifiés (Rhai 1.25.1)

Mesurés par les programmes de `spikes/rhai-1-25`, avec les features de `vynil-core` plus `metadata`.

- Avec `sync`, `Engine`, `AST`, `Scope` et `Dynamic` sont `Send + Sync`, et un moteur partagé entre threads fonctionne.
- L'`AST` n'est pas sérialisable : la persistance des fonctions passe par leur texte source (DEC-1).
- `AST::iter_functions` rend nom, paramètres, accès et commentaires de documentation des fonctions. Rhai ne type pas
  statiquement : un contrat de toolset se réduit à des noms et des paramètres (`toolsets`, FR-10).
- Un pointeur de fonction ou une closure se sérialise **sans erreur** en chaîne, ou en tableau s'il capture ; relue, la
  valeur n'est plus appelable. `char` revient en chaîne et `blob` en tableau. La détection doit donc se faire par type,
  avant la sérialisation.
- Dans le chemin d'exécution par scripts successifs, une `fn` ne lit pas les variables du scope (erreur
  `Variable not found`), reçoit ses arguments, et lit les constantes par `global::`. `Engine::call_fn` fait exception,
  mais le LLM ne l'utilise pas.
- Les limites de taille s'appliquent aussi aux valeurs rendues par des fonctions natives : un appel MCP trop gros échoue
  avec une erreur, sans troncature.
- `on_progress` interrompt une boucle Rhai en 200 ms environ ; pendant un appel natif bloquant, l'arrêt n'a lieu qu'au
  retour de l'appel (1,5 s dans le test).
- Avec la feature `metadata`, signatures et commentaires de documentation des fonctions, scripts compris, sont exportables
  pour l'introspection. `vynil-core` n'active pas cette feature aujourd'hui.

## Questions ouvertes

- **TBD:** hook de progression du moteur pour l'annulation (`agent-sessions#FR-4`) et la durée maximale d'un script
  (FR-4) ; `vynil-core` n'en pose aucun et son `sleep` bloque le thread sans point d'annulation (Observed in:
  vynil-core `src/engine.rs`).
- **TBD:** accord des capacités à l'exécution, par agent : prérequis côté `vynil-core` (voir domaine, shopping-list). Tant
  qu'il n'est pas levé, `get_env` et `sleep` restent accessibles au LLM dans le moteur de `vynil-core`.
- **TBD:** évolutions de `vynil-core` : introspection du scope, des modules et des fonctions ; modules non fichiers ;
  export des métadonnées et doc-comments.
- **TBD:** compléter les fonctions de formatage (FR-8). Observé dans `vynil-core` : `json_encode`, `json_decode`,
  `yaml_encode`, `yaml_decode` ; à inventorier au-delà : rendu en tableau, troncature.
- **TBD:** rendu par défaut d'un `Dynamic` non textuel (JSON compact ou indenté).

## Acceptation

- **AC-1:** Un premier appel définit `let x = 3` et `fn f(a) { a * 2 }` ; un second appel évalue `f(x)` et rend `6`,
  y compris après restauration de session.
- **AC-2:** La recherche `find("loki")` rend les fonctions et skills dont le nom ou la documentation contient `loki`,
  limités aux modules accordés.
- **AC-3:** Un script qui dépasse la profondeur d'appel autorisée s'arrête avec une erreur rendue au LLM, et un script
  qui dure plus de deux minutes n'est pas interrompu.
- **AC-4:** Un script dont la valeur finale dépasse le plafond rend un texte tronqué avec avis explicite.
- **AC-5:** Un script qui rend un objet structuré peut le faire afficher en JSON ou en tableau par une fonction de
  formatage de la librairie, sans définition de format dans le toolset.
- **AC-6:** L'introspection ou le prompt système mentionne que les fonctions ne voient pas le scope et que les closures
  ne sont pas persistées.
- **AC-7:** Un script qui construit une chaîne de plus de 16 Mio s'arrête avec une erreur rendue au LLM, sans affecter les
  autres sessions du processus.
- **AC-8:** Une variable qui contient une closure produit l'avertissement et est absente après restauration, le reste
  du scope étant restauré.
- **AC-9:** Un appel MCP simulé qui ne rend jamais est abandonné dans le délai d'une annulation, sans attendre sa fin.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
