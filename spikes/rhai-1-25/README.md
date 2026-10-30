# Spikes Rhai 1.25.1

Vérifications de comportements de Rhai sur lesquels s'appuient les documents de `dsd/`. Version et features
(`sync`, `serde`, `internals`) alignées sur `vynil-core`, plus `metadata`. Ces programmes jettent, ils ne font pas partie
de la librairie.

- `persistence` : `Send + Sync`, sérialisation des valeurs du scope (closures, `char`, `blob`), AST non sérialisable,
  métadonnées de fonctions, limites de taille, annulation par `on_progress`, introspection, partage du moteur.
- `private_dependencies` : un module de toolset appelle un module importé que le moteur du LLM ne connaît pas ; ce que
  l'export de métadonnées en révèle.
- `strip_submodules` : retrait des sous-modules d'import d'un module de toolset (mesuré le 2026-10-30).
- `scope_visibility` : visibilité des variables du scope pour les fonctions définies par script.

Lancer : `CARGO_TARGET_DIR=~/.cache/cargo-target/lymaille-spike cargo run --bin persistence`
