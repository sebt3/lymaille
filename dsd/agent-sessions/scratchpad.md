# Solution Design: Scratchpad

## Objectif

Fournir le moteur d'exécution Rhai lié à la session : un scope persistant, le tool natif `rhai_pad`, l'introspection, les
limites de sandbox et le rendu borné des sorties.

## Contexte applicable du domaine

`agent-sessions#FR-1` (tool natif `rhai_pad` ; le tool `skill` relève de `toolsets`) et `agent-sessions#FR-2` (introspection avec recherche) sont réalisées ici.
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

- **FR-11 - MUST:** Le moteur enregistre d'office le seul groupe pur du socle commun de `vynil-core` ; les autres groupes
  (autorité : `get_env`, `sleep`, `log_*` ; fichiers ; `shell_run`) ne le sont que si la définition d'agent les accorde,
  quelles que soient les features avec lesquelles le processus hôte compile `vynil-core`. Prérequis :
  `vynil-org/vynil-core#18` (`docs/vynil-core-shopping-list.md`).
    - **Source:** agent-sessions#BR-2, agent-sessions#CON-3 ; user instruction, 2026-10-25
    - **Verified by:** `AC-10`

- **FR-12 - MUST:** Le moteur de session et le moteur qui construit les modules de toolset désactivent le mot-clé `const`
  (`Engine::disable_symbol`) ; l'hôte et la librairie ne posent aucune constante dans le scope de session. Un script qui
  emploie `const` reçoit une erreur de syntaxe.
    - **Source:** user instruction, 2026-10-25 ; Observed in: panique de Rhai sur affectation à une constante map (Faits
      vérifiés)
    - **Verified by:** `AC-11`

- **FR-5 - MUST:** Le résultat d'un script est une valeur `Dynamic` (chaîne ou objet structuré) dont le rendu renvoyé au
  LLM est plafonné en caractères, à 8 000 par défaut, valeur surchargeable par définition d'agent. Une sortie tronquée
  conserve son début et sa fin, porte au milieu un avis explicite indiquant la troncature et la taille d'origine, et
  invite à extraire la partie utile depuis une variable. Un `Dynamic` non textuel est rendu par défaut en JSON
  compact.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-4`

- **FR-6 - MUST:** Une erreur Rhai (syntaxe, exécution, limite) est rendue au LLM avec sa position et un message
  exploitable. Quand une fonction ou un module est introuvable, le message nomme les candidats les plus proches parmi les
  seuls modules accordés.
    - **Source:** agent-sessions, section Risques ; suggestions de candidats, user instruction, 2026-10-26
    - **Verified by:** `AC-12`

- **FR-7 - MUST:** À la fin d'un script, si une variable persistante contient une closure, un pointeur de fonction ou une
  valeur non sérialisable, le rendu porte un avertissement qui nomme la variable et suggère de définir une `fn`, et la
  valeur est exclue de la persistance ; le script n'est pas rejeté. Les closures passagères ne sont pas concernées.
    - **Source:** user instruction, 2026-10-24 ; agent-sessions#CON-4
    - **Verified by:** `AC-8`

- **FR-8 - MUST:** La librairie met à disposition du LLM des fonctions de formatage (conversion d'un `Dynamic` en texte,
  JSON ou tableau aligné à partir d'une liste de maps, extraction, troncature début et fin) ; le format de ce qui revient dans la session reste le choix du LLM.
    - **Source:** user instruction, 2026-10-24
    - **Verified by:** `AC-5`

- **FR-9 - MUST:** Les limites de `agent-sessions#CON-4` sont exposées au LLM par le skill intégré sur `rhai_pad`
  (`agent-sessions/toolsets#FR-18`), avec la façon de les contourner : passer les données en argument et lire le
  ToolsetContext par `session_context()` (`agent-sessions/toolsets#FR-17`) ; `const` n'existe pas (FR-12).
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
    - **Decision:** réutiliser cette feature et la stratégie de ce pont pour les appels MCP et d'inférence faits depuis un
      script.
    - **Consequences:** moteur et valeurs sont `Send + Sync` ; l'hôte fournit un runtime multi-thread
      (`agent-sessions#CON-6`) ; l'annulation suit DEC-3. Le pont `rt::block_on` de `vynil-core` est `pub(crate)` et
      n'existe qu'avec les features `http`, `oci`, `s3` ou `k8s` : `lymaille` ne peut pas l'appeler tel quel (voir Questions
      ouvertes).
    - **Source:** user instruction, 2026-10-24 (feature `sync`) ; Observed in: vynil-core `8076ee4` (`src/rt.rs`, `src/lib.rs`)

- **DEC-3 - Exécution des scripts hors des workers et annulation par jeton de fil**
    - **Context:** un script est synchrone et peut durer (compilation dans un sandbox MCP) ; `on_progress` n'a la main
      qu'entre deux opérations Rhai ; `vynil-core` fournit depuis `#21` un jeton d'annulation porté par le fil
      (`cancel::scope`), honoré par `block_on` et `sleep`, qui abandonne le futur en cours en 25 ms environ.
    - **Decision:** chaque appel de `rhai_pad` s'évalue dans `tokio::task::spawn_blocking`, qui ouvre sur ce fil une portée
      `cancel::scope` avec le jeton du run ; le hook `on_progress` lit le même jeton pour interrompre les boucles Rhai. Tout
      pont bloquant utilisé par `lymaille` honore ce jeton.
    - **Alternatives:** évaluer le script dans une tâche async, écarté car une boucle Rhai y monopoliserait un worker du
      runtime de l'hôte.
    - **Consequences:** les workers du runtime restent libres pour l'inférence et les autres sessions ; le nombre de scripts
      simultanés est borné par le pool bloquant de tokio ; FR-10 est réalisable sans attendre le retour d'un appel.
    - **Source:** user instruction, 2026-10-25 ; Observed in: vynil-core `src/cancel.sdd`, `src/rt.sdd`

## Faits vérifiés (Rhai 1.25.1)

Mesurés par les programmes de `spikes/rhai-1-25`, avec les features de `vynil-core` plus `metadata`.

- Avec `sync`, `Engine`, `AST`, `Scope` et `Dynamic` sont `Send + Sync`, et un moteur partagé entre threads fonctionne.
- L'`AST` n'est pas sérialisable : la persistance des fonctions passe par leur texte source (DEC-1).
- `vynil-core` enregistre d'office `get_env`, `sleep` et `log_*` ; avec les features `fs`, `shell` ou `password` du processus,
  il enregistre aussi les accès fichiers et `shell_run`. Le moteur est public, donc `on_progress` et les limites de taille se
  posent depuis `lymaille` (`docs/vynil-core-shopping-list.md`).
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
  retour de l'appel (1,5 s dans le test). `vynil-core` a depuis ajouté le jeton de fil de DEC-3 (`#21`).
- Avec la feature `metadata`, signatures et commentaires de documentation des fonctions, scripts compris, sont exportables
  pour l'introspection. `vynil-core` offre depuis cette feature (`#22`).
- Une constante posée dans le scope de session par l'hôte se lit depuis les scripts du LLM et depuis les `fn` qu'il
  définit, mais pas depuis une fonction d'un module de toolset (`Variable not found`, avec ou sans `global::`). Une
  fonction native enregistrée sur le moteur de session est, elle, appelable depuis un module de toolset. Mesuré le
  2026-10-25 hors dépôt, Rhai 1.25.1.
- **Panique de Rhai :** affecter une propriété ou un index d'une constante map (`const C = #{a: 1}; C.a = 2`, ou constante
  posée par `Scope::push_constant`) fait paniquer le processus (`unreachable!`, `eval/stmt.rs`) avec l'optimiseur par
  défaut ; avec `OptimizationLevel::None`, Rhai rend l'erreur `Cannot modify constant`. Reproduit sur 1.25.1 et 1.27.0, le
  2026-10-25, aucun ticket trouvé en amont. Avec `Engine::disable_symbol("const")`, `const` et `export const` deviennent
  une erreur de syntaxe et les affectations de propriétés de variables `let` restent possibles : c'est la parade retenue
  (FR-12).

## Questions ouvertes

- **TBD:** pont bloquant pour les appels MCP et d'inférence de `lymaille` : `block_on` public derrière une feature dédiée,
  demandé dans `vynil-org/vynil-core#23` (2026-10-25) ; prérequis du code qui appelle MCP ou l'inférence depuis un script.


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
- **AC-10:** Dans le moteur d'un agent qui n'accorde aucun groupe de `vynil-core`, `json_encode` répond, tandis que
  `get_env`, `sleep`, `file_read` et `shell_run` sont introuvables et absents de l'introspection, y compris quand le
  processus est compilé avec les features `fs` et `shell` de `vynil-core`.
- **AC-11:** Dans un moteur de session comme dans le moteur de construction des toolsets, `const C = #{a: 1}; C.a = 2` rend une
  erreur de syntaxe rendue au LLM, sans panique, et `let c = #{a: 1}; c.a = 2; c.a` rend `2`.
- **AC-12:** Un script qui appelle `grafana::loki_queryy` rend une erreur qui nomme `grafana::loki_query` comme candidat le
  plus proche ; aucune suggestion ne nomme une fonction d'un module non accordé.

## Réalisation SpecDD

`TBD` : aucune spec `.sdd` n'existe encore.
