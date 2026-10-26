# SpecDD project specific overrides

## Langue

La documentation du projet est rédigée en français : documents DSD (Domain Definitions et Solution Designs) et contenu
des specs `.sdd`. Les mots-clés du langage SpecDD (`Spec`, `Must`, `Scenario`...) et les marqueurs normatifs de DSD
(`MUST`, `SHOULD`, `TBD`, `ASSUMPTION`, `BR-1`...) restent tels que définis par SpecDD, en anglais.

Le code est en anglais : commentaires, commentaires de documentation (rustdoc) et nommage des fonctions, traits, types
et autres identifiants.

## Du DSD aux specs

- Deux niveaux : les documents DSD (`dsd/agent-sessions.md` et `dsd/agent-sessions/*.md`) disent le domaine et les Solutions
  (exigences `FR`, `AC`, `DEC`, `CON`…) ; les specs `.sdd` placées près du code en dérivent des contrats vérifiables.
- Une spec cite les exigences DSD qu'elle réalise (`agent-sessions/session#FR-13`) et ne les contredit pas. Une contradiction ou
  un manque découvert en écrivant ou en implémentant se remonte, il ne se tranche pas dans la spec. Les `TBD` et `ASSUMPTION` du
  DSD restent visibles dans la spec (ligne `[?]` dans `Tasks`).
- Chaque `AC` du DSD devient un ou plusieurs `Scenario` de la spec qui porte le comportement.

## Granularité des specs

- Une spec `.sdd` décrit un fichier source, posée à côté sous le même nom de base (`src/session.sdd` pour `src/session.rs`).
- Deux ou trois fichiers partagent une spec seulement s'ils sont inséparables (un trait et son unique implémentation, un
  fichier et son fichier de test exclusif). La raison figure en commentaire.
- Un fichier sans comportement substantiel ni subtil (ré-exports, module à constante unique) peut ne pas avoir de spec : le
  contrat du répertoire le couvre.
- La spec d'un fichier en donne le contrat observable complet (`Purpose`, `Exposes`, `Accepts`, `Returns`, `Raises`,
  `Handles`, et les invariants en `Must`) : un lecteur doit pouvoir réimplémenter le fichier à partir d'elle seule.
- Les specs de répertoire et de crate restent grossières : but, rôle des enfants immédiats, contraintes de frontière, et
  `References` vers les specs enfants. Elles ne répètent pas le comportement des fichiers.

## Versions des dépendances

- Une version qui change à chaque mise à jour d'une dépendance est nommée dans une seule spec : celle qui possède le manifeste
  qui l'épingle (pour `vynil-core`, la spec de `Cargo.toml`). Les autres specs nomment la dépendance sans version.
- Une version reste dans une spec seulement quand elle *est* l'information : un comportement modifié à partir d'une version, un
  défaut corrigé dans une version.

## Spécification pilotée par les tests

- Aucun changement de code produit n'est livré sans tests dérivés des `Scenario` de la spec propriétaire. Un `Scenario` donne un
  ou plusieurs tests, nommés d'après lui.
- Pour tout changement de comportement : d'abord les tests qui échouent, vus échouer pour la bonne raison, puis l'implémentation
  minimale, puis le vert. Le code satisfait les tests ; les tests ne courent pas après le code.
- Un test qui passe d'emblée est suspect (test inutile ou spec fausse) : on le signale.
- Une entrée de `Tasks` passe à `[x]` seulement quand `scripts/check` passe, et après validation (agent `validator`) ; elle est
  cochée par l'agent orchestrateur ou le développeur, jamais par l'agent qui a implémenté.

## Contraintes du code produit

- Ne pas élargir la surface publique d'un crate sans que la spec propriétaire ne le prévoie.
- Aucune dépendance ajoutée sans qu'une spec la nomme dans `Depends on`. `vynil-core` est consommé en version publiée, jamais
  par chemin.

## Cargo

- Un `CARGO_TARGET_DIR` par projet : `~/.cache/cargo-target/lymaille`. Ne jamais le partager avec un autre projet ni avec un
  worktree dont la branche diverge. Vérifier `df -h` avant un build lourd, et lancer `cargo clean` en fin de session.

## Harnais de qualité

Un agent autonome écrit le code de ce projet. cargo et clippy sont son garde-fou : la configuration est la plus stricte que
les outils permettent, et elle ne se contourne pas. Elle est définie et vérifiée dans `spikes/lint-harness`, à reporter à la
racine à la création du squelette du crate.

- **Point d'entrée unique :** `scripts/check` (complet) ou `scripts/check fast` (boucle interne : formatage, clippy, tests).
  Une tâche n'est terminée que lorsque `scripts/check` passe. Il n'y a pas de succès partiel.
- **Ne jamais modifier** `[workspace.lints]` du `Cargo.toml`, `clippy.toml`, `rustfmt.toml`, `deny.toml`,
  `rust-toolchain.toml` ni `scripts/check`. Si un lint semble faux, s'arrêter et le signaler.
  Aucun contrôle automatique ne l'empêche : le relecteur final (le développeur principal) examine tout diff qui touche ces
  fichiers.
- **Jamais `#[allow(...)]`** : `clippy::allow_attributes` le refuse. La seule dérogation est `#[expect(lint, reason = "...")]`,
  avec une raison précise, sur l'élément le plus étroit possible. Un `expect` devenu inutile fait échouer la compilation.
- **Les `disallowed_*` de `clippy.toml` encodent l'architecture** (`CON-1`, `CON-3`, `NFR-1`, `FR-4` de `dsd/agent-sessions.md`) :
  pas de fichier, d'environnement, de processus, d'horloge système ni de `thread::sleep` en direct. Une exception est un
  `#[expect]` à l'unique endroit prévu pour elle.
- **Pas de panique hors tests** : ni `unwrap`, `expect`, `panic!`, `todo!`, indexation directe, ni arithmétique non vérifiée.
- **Formatage :** `cargo fmt` sur la chaîne stable, avec des options stables seulement.
- **Documentation :** `missing_docs` pour tout élément public, pas pour le privé.
- **Chaîne :** Rust stable épinglée à 1.99.0, `rust-version` 1.95.0, édition 2024.

## Commandes de vérification

- `scripts/check` : point d'entrée unique (formatage, clippy, tests, et le reste du harnais). `scripts/check fast` en boucle interne.
- `specdd lint` : validation de la syntaxe des specs. Si `specdd` est absent du PATH, le binaire npm global est
  `~/.npm-global/bin/specdd`.

## Conventions git

- Messages de commit en français, première ligne `Thème : résumé` (`Cadrage`, `DSD`, `Harnais`, `Agents`…), le corps explique le pourquoi.
- Ne pas enchaîner automatiquement les tâches d'implémentation : une tranche de comportement par changement, puis rapport et attente.
