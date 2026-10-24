# Shopping-list vynil-core

Évolutions de `vynil-core` demandées par le cadrage de `lymaille` (`dsd/agent-sessions.md`). État vérifié le 2026-10-24 sur
`vynil-core` 0.8.0 (commit b2b1c9e), par les programmes de `spikes/` et par lecture du code.

- **Must have :** sans cela, `lymaille` ne peut pas satisfaire une exigence obligatoire de son cadrage.
- **Nice to have :** améliore la qualité ou la simplicité, sans bloquer.
- **Aucune action :** déjà couvert, ou réalisable depuis `lymaille` seul.

Issues ouvertes dans `vynil-org/vynil-core` le 2026-10-24 : #18 (M1) et #19 à #22 (N1 à N4).

## Must have

### M1 — Enregistrement granulaire du socle commun, sans rien casser

Issue : `vynil-org/vynil-core#18`.

**Constat.** `Script::new_bare` enregistre toujours `get_env`, `sleep` et `log_*`, et, selon les features de compilation du
processus, les accès fichiers (`file_read`, `file_write`, `read_dir`...) et `shell_run`. Les enregistreurs du socle commun
(`core_common_rhai_register`) et des fichiers (`fs_rhai_register`) sont privés. Seuls les modules `chrono`, `hashes`,
`semver`, `yaml`, `glob`, `password`, `key`, `shell`, `oci`, `s3`, `http` et `k8s*` ont un enregistreur public.
Observed in: `vynil-core/src/engine.rs` (lignes 50 et 131), `spikes/vynil-core-surface`.

**Besoin.** Scinder le socle commun en groupes publics, additifs :

| Groupe | Fonctions |
| --- | --- |
| Pur | `sha256`, `url_encode`, `to_decimal`, `base64_decode`, `base64_encode`, `json_encode`, `json_encode_escape`, `json_decode`, `basename`, `dirname` |
| Autorité | `get_env`, `sleep`, `log_debug`, `log_info`, `log_warn`, `log_error` |
| Fichiers | `file_read`, `file_write`, `file_copy`, `create_dir`, `read_dir`, `is_file`, `is_dir` (enregistreur rendu public) |

`Script::new_bare` appelle les mêmes groupes dans le même ordre : son comportement ne change pas pour kuberest et vynil,
et le changement est une version mineure. `lymaille` construit un `Engine::new()` et n'appelle que les groupes accordés
par la définition de l'agent. La migration complète des fonctions en modules optionnels, envisagée pour plus tard,
reste possible sur cette base.

**Exigences de lymaille.** `agent-sessions#BR-2`, `agent-sessions#CON-3`, `agent-sessions#AC-1`.

**Vérification.** Un test de `lymaille` construit un moteur avec le groupe pur seul : `json_encode` répond, `get_env` et
`sleep` sont introuvables. Côté `vynil-core`, un `Scenario` verrouille l'appartenance de chaque fonction à son groupe, comme
`new_restricted` l'est déjà.

## Nice to have

### N1 — Enregistrement sous forme de modules Rhai

Issue : `vynil-org/vynil-core#19`.

Pour chaque groupe, une variante qui rend un `rhai::Module` (appelable en `http::get(...)`) au lieu d'enregistrer des
fonctions globales. L'introspection attribue alors chaque fonction à son module sans astuce, et le filtrage par agent est
immédiat. Sans cela, `lymaille` attribue les fonctions à leur groupe en comparant les signatures avant et après chaque
enregistrement.

### N2 — Documentation des fonctions enregistrées

Issue : `vynil-org/vynil-core#20`.

Enregistrer les fonctions avec leurs commentaires de documentation et noms de paramètres (`FuncRegistration`), pour que
l'introspection apprenne au LLM à quoi sert `json_encode` au lieu de lui donner une signature nue. Demande un passage sur
une centaine d'enregistrements.

### N3 — Appels bloquants annulables

Issue : `vynil-org/vynil-core#21`.

`rt::block_on` accepte un jeton d'annulation, et `sleep` devient coopératif. Ne compte que pour les agents à qui l'on
accorde les modules `http`, `k8s`, `s3` ou `oci` de `vynil-core` : les appels MCP et d'inférence passent par le pont de
`lymaille` (`agent-sessions/scratchpad#FR-10`). Aujourd'hui, ces appels vont jusqu'à leur terme avant qu'une annulation
prenne effet.
Observed in: `spikes/rhai-1-25` (`persistence`, S5).

### N4 — Feature Cargo `metadata`

Issue : `vynil-org/vynil-core#22`.

Exposer une feature `metadata` de `vynil-core` qui active `rhai/metadata`, pour que les consommateurs obtiennent
signatures et commentaires de documentation des fonctions sans déclarer eux-mêmes la feature sur `rhai`. Coût faible.
Aujourd'hui `lymaille` la déclare sur sa propre dépendance `rhai`, et elle s'unifie avec celle de `vynil-core`
(Observed in: `spikes/vynil-core-surface`).

## Aucune action sur vynil-core

- **Rhai** : `sync`, `serde` et `internals` sont déjà activées (`Cargo.toml`).
- **Pont async vers sync** : `rt::block_on` existe ; il exige un runtime tokio multi-thread fourni par l'hôte
  (`agent-sessions#CON-6`).
- **Handlebars** : `HandleBars::new_restricted()` retire les six helpers qui touchent l'hôte (issue `vynil-core#13`) ;
  `lymaille` l'utilise, et n'appelle ni `register_helper_dir` ni `engine_mut`.
- **Moteur** : `Script.engine` et `Script.ctx` sont publics. `lymaille` pose lui-même `on_progress` (annulation),
  `set_max_string_size` et assimilés, `set_module_resolver` et `register_static_module` (toolsets stockés par trait).
- **Introspection** : l'API de Rhai (`gen_fn_metadata_to_json`, `Scope::iter`) suffit ; le filtrage des dépendances de
  toolset et la recherche lexicale sont à la charge de `lymaille`.
- **Formatage** : `json_encode`, `json_decode`, `yaml_encode` et `yaml_decode` existent ; le tableau aligné et la
  troncature sont ajoutés par `lymaille`.

## À vérifier avant de coder

- Les tests existants de `new_bare` restent verts après la scission de M1.
- `lymaille` compile avec `vynil-core` en `default-features = false, features = ["rhai"]` plus les modules choisis.
