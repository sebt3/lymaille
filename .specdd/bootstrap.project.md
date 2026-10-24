# SpecDD project specific overrides

## Langue

La documentation du projet est rédigée en français : documents DSD (Domain Definitions et Solution Designs) et contenu
des specs `.sdd`. Les mots-clés du langage SpecDD (`Spec`, `Must`, `Scenario`...) et les marqueurs normatifs de DSD
(`MUST`, `SHOULD`, `TBD`, `ASSUMPTION`, `BR-1`...) restent tels que définis par SpecDD, en anglais.

Le code est en anglais : commentaires, commentaires de documentation (rustdoc) et nommage des fonctions, traits, types
et autres identifiants.

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
