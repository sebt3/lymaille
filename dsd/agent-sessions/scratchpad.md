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

- **FR-11 - MUST:** Le moteur enregistre d'office le seul groupe pur du socle commun de `vynil-core` (`core_pure_rhai_register`) ; les
  autres groupes (hôte : `get_env`, `log_*` ; attente : `sleep` ; fichiers ; `shell_run`) ne le sont que si la définition
  d'agent les accorde, quelles que soient les features avec lesquelles le processus hôte compile `vynil-core`. Livré par
  `vynil-core` 0.9.0 (`#18`).
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
  ToolsetContext par `session_context()` (`agent-sessions/toolsets#FR-17`) ; `const` n'existe pas (FR-12). Il dit aussi ce que
  les modèles qui connaissent mal Rhai se trompent à écrire : une chaîne brute qui contient `"#` s'écrit `##"…"##` (le `r#"…"#`
  de Rust n'existe pas en Rhai), une chaîne entre accents graves ne peut pas contenir d'accent grave, et toute fonction d'un
  module MCP prend une seule table `#{ … }` (FR-14).
    - **Source:** agent-sessions#CON-4 ; trois règles ajoutées, `sebt3/lymaille#3` (« Related »), 2026-11-03 : appliquées dans
      les exécutions des hôtes, elles ont ramené les erreurs de script de 14 à 0 et les appels de 60 à 9-10
    - **Verified by:** `AC-6`, `AC-14`

- **FR-10 - MUST:** Un appel MCP ou d'inférence en cours depuis un script est abandonné à l'annulation du run, sans
  attendre sa fin.
    - **Source:** agent-sessions#FR-4 ; Observed in: spikes/rhai-1-25 (`persistence`, S5)
    - **Verified by:** `AC-9`

- **FR-13 - MUST:** L'agent de configuration (`agent-sessions#BR-8`) dispose d'un second tool, `rhai_prod_inspect`, qui exécute du Rhai
  dans un moteur d'inspection de la cible courante (module `inspect`, `agent-sessions/toolsets#FR-19`). Les fonctions
  d'introspection (variables, modules, fonctions, toolsets, skills, recherche) s'exécutent réellement. Les fonctions des modules
  de toolsets projetés et des modules MCP de la cible ont les mêmes noms, signatures et documentation que les modules réels,
  mais tout appel rend l'erreur « inspection seule » : ni connexion ni effet de bord. Pour une session passée, les variables
  du scope sont lisibles en valeur et en lecture seule (toute écriture échoue) ; le texte source des fonctions que le LLM y
  a définies est lisible, et ces fonctions ne sont pas appelables. Aucun script n'est rejoué pour ouvrir la cible (`DEC-1`).
    - **Source:** user instruction, 2026-10-29
    - **Verified by:** `AC-13`

- **FR-14 - MUST:** Quand une fonction existe mais est appelée avec un nombre ou des types d'arguments qui ne lui conviennent
  pas, l'erreur rendue au LLM montre comment l'appeler. Pour une fonction à un seul paramètre map, cas de toute fonction
  générée d'un module MCP (`agent-sessions/modules#FR-10`), l'appel attendu est écrit sous la forme Rhai d'une table littérale
  construite d'après le schéma d'entrée du tool (`read_file(#{ path: "…" })`), les champs obligatoires d'abord. Toute fonction de
  module peut déclarer cette ligne d'usage dans sa documentation ; l'introspection la lit (`agent-sessions#FR-2`) et ne la
  montre qu'avec les éléments accordés (`agent-sessions#BR-2`).
    - **Source:** `sebt3/lymaille#3`, 2026-11-03 : « function not found … Closest candidates: read_file(arguments: Map) » ne
      montre pas la syntaxe `#{…}` ; Qwen3.6 et Qwen3.8-flash en ont dérivé dès le premier appel (60 appels et 14 erreurs
      de script pour une tâche de 9-10 appels)
    - **Verified by:** `AC-14`

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
      (`agent-sessions#CON-6`) ; l'annulation suit DEC-3. Le pont `rt::block_on` de `vynil-core` est public depuis la
      version 0.9.0, derrière la feature `rt` (`#23`).
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
    - **Amendement (user instruction, 2026-10-27) :** l'évaluation ne tourne pas dans `spawn_blocking` (pile de 2 Mio par défaut, qui
      déborde à 512 niveaux d'appels en débogage et ferait avorter le processus hôte) mais dans un fil dédié
      (`std::thread::Builder::stack_size`, taille mesurée) joint depuis une tâche async ; la portée `cancel::scope` et le hook
      `on_progress` restent ceux décrits ci-dessus. Le nombre de scripts simultanés est borné par la librairie, plus par le pool bloquant.
      Taille de pile (user instruction, 2026-10-27 ; mesurée par un spike sur Rhai 1.27.0, plafonds inchangés 512 / 256 / 128) : 256 Mio en
      release et 1 Gio en débogage (`cfg!(debug_assertions)`), soit le pire cas construit (125 Mio / 606 Mio) avec une marge de 2 ; la pile
      est réservée et non engagée. À documenter pour l'hôte : `overcommit_memory=2` ou une limite de mémoire virtuelle l'empêchent. Les
      frames natives (pont `rt::block_on`, client MCP) n'ont pas été mesurées : test de bout en bout à écrire avec `run`.

## Faits vérifiés (Rhai 1.25.1)

Mesurés par les programmes de `spikes/rhai-1-25`, avec les features de `vynil-core` plus `metadata`.

- Avec `sync`, `Engine`, `AST`, `Scope` et `Dynamic` sont `Send + Sync`, et un moteur partagé entre threads fonctionne.
- L'`AST` n'est pas sérialisable : la persistance des fonctions passe par leur texte source (DEC-1).
- `vynil-core` 0.8.0 enregistrait d'office `get_env`, `sleep` et `log_*` ; avec les features `fs`, `shell` ou `password` du processus,
  il enregistre aussi les accès fichiers et `shell_run`. Le moteur est public, donc `on_progress` et les limites de taille se
  posent depuis `lymaille`. La version 0.9.0 scinde ce socle en groupes publics.
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

## Extension réservée : pad de workflow

Hors du premier jalon (0.1), sans exigence `MUST` : le besoin est posé par le développeur principal (2026-10-30), l'intégration à miryad-core
venant après la livraison de `lymaille`. Principe consigné : un workflow de miryad-core peut posséder un pad Rhai qui vit avec lui, sans
session d'agent (ni historique, ni inférence, ni tour). Les steps d'un workflow utilisent alors les modules de `lymaille` (MCP avec validation de
schéma, reconnexion et audit, Memory, toolsets) au lieu de n'appeler `vynil-core` que seul, et les variables du pad sont lisibles par les évaluations
du workflow (gabarits, conditions).

Exigences envisagées, à qualifier à l'ouverture du lot :

- **PW-1 :** cycle de vie d'un pad sans boucle de tour : ouverture, restauration, exécution d'un script (`Scratchpad::run`), instantané, fermeture.
  Les capacités sont celles que la définition du workflow accorde (`agent-sessions#BR-2`), jamais celles d'un LLM.
- **PW-2 :** l'état du pad se persiste sous une `SessionState` d'une nature distincte (`Workflow`, à côté de `Work`, `Config` et `Sub`), pour
  réutiliser le format, l'audit et les événements (`agent-sessions#BR-5`) ; l'identifiant est celui du workflow.
- **PW-3 :** les variables du pad se lisent par le workflow comme un instantané JSON en lecture seule (valeurs étiquetées, closures et pointeurs de
  fonction exclus avec avertissement, comme `FR-7`).
- **PW-4 :** exécution déterministe vis-à-vis de la relecture du moteur durable de miryad-core (Restate) : l'exécution du script et la sauvegarde de
  l'instantané forment une seule opération journalisée, de sorte qu'une relecture rende le résultat journalisé sans réexécuter le script ni ses
  appels MCP.
- **PW-5 :** un pad est séquentiel : un second step du même workflow qui demande le pad pendant qu'un script s'exécute est refusé avec une
  erreur explicite, jamais mis en file (comme le verrou de tour d'une session).

Décisions (2026-10-31) :

- **Décidé :** l'état du pad se persiste sous la nature `Workflow` de `SessionState`, pas sous un format d'instantané propre au pad.
    - **Source:** user instruction, 2026-10-31
- **Décidé :** pad séquentiel par refus, pas par file d'attente (PW-5).
    - **Source:** user instruction, 2026-10-31
- **Décidé :** le kind de step `rhai` de miryad-core reste inchangé ; un kind distinct (nom à choisir à l'ouverture du lot, côté miryad-core)
  porte le pad. Le moteur du pad est plus strict (`import` fermé, pas de `const`, les `fn` ne voient pas le scope) : les deux ne sont pas
  interchangeables.
    - **Source:** user instruction, 2026-10-31
- **Décidé :** la résolution des références d'authentification MCP d'un workflow revient à l'hôte, comme pour une session.
    - **Source:** user instruction, 2026-10-31
- **Décidé :** le pad de workflow est hors du périmètre 0.1 de `lymaille` ; il reste une extension réservée.
    - **Source:** user instruction, 2026-10-31

Source: user instruction, 2026-10-30

## Questions ouvertes

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

- **AC-13:** Dans `rhai_prod_inspect`, `functions()` liste les fonctions d'un module MCP de la cible avec leur documentation et l'appel de l'une
  d'elles rend l'erreur d'inspection seule sans appel réseau ; sur une session passée, la valeur d'une variable se lit et son écriture échoue.
- **AC-14:** Un script qui appelle `sandbox::read_file("path")`, fonction MCP dont le schéma exige le champ texte `path`, reçoit une
  erreur qui contient `read_file(#{ path: "…" })` ; le skill intégré sur `rhai_pad` contient les trois règles de FR-9, et chacune
  de ses affirmations est vraie quand on l'exécute.

## Réalisation SpecDD

Specs créées dans `src/scratchpad/` (une par fichier source prévu, spec de répertoire comprise) :

- `scratchpad.sdd` : spec de répertoire (frontière : aucune E/S ni horloge système, seuls les groupes accordés, `const` désactivé).
- `error.sdd` : erreur du module, erreurs de Rhai classées et rendues avec position (FR-6, AC-3, AC-7, AC-11).
- `limits.sdd` : plafonds de mémoire et profondeurs, aucune limite de durée (FR-4, AC-3, AC-7).
- `grant.sdd` : description abstraite de ce que la définition d'agent accorde (groupes, modules, toolsets, skills ; FR-11, FR-3).
- `engine.sdd` : moteur d'une session, groupes accordés, `const`, résolveur fermé, crochet d'annulation (FR-11, FR-12, DEC-3, AC-10, AC-11).
- `render.sdd` : rendu borné, JSON compact, troncature début et fin avec avis (FR-5, AC-4).
- `format.sdd` : fonctions de formatage exposées au LLM (FR-8, AC-5).
- `persist.sdd` : variables étiquetées, détection des closures, fonctions par texte source, restauration (FR-2, FR-7, DEC-1, AC-1, AC-8).
- `introspect.sdd` : catalogue, introspection et recherche `find`, ligne d'usage d'une fonction (FR-3, FR-9, FR-14, AC-2, AC-6, AC-14).
- `suggest.sdd` : candidats les plus proches parmi les seuls éléments accordés, appel attendu d'une fonction existante (FR-6, FR-14, AC-12, AC-14).
- `run.sdd` : le pad et l'exécution d'un appel de `rhai_pad` (FR-1, FR-5, FR-7, FR-10, DEC-3, AC-1, AC-4, AC-8, AC-9).

État au 2026-10-31 : toutes ces specs sont implémentées et couvertes par des tests de comportement et de mutation (`scripts/mutants`). Une annulation n'est pas rattrapable par `try/catch` d'un script (vérifié sur la version de Rhai épinglée), et l'absence de limite de durée est vérifiée par un test long `#[ignore]` de deux minutes réelles. Le tool `rhai_pad` est appelé par la boucle de tour (`src/runtime/turn.rs`).

État au 2026-11-04 (lot 0.2.0) : livré, `FR-14` (l'erreur d'appel montre l'appel attendu, à distance zéro pour les suggestions) et `FR-9`/`AC-14` (pièges de Rhai dans le skill intégré `rhai-pad`).
