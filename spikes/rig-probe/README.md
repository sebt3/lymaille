# Sonde `rig-core` 0.43.0 (lot R0 de `src/agents/rig.sdd`)

Programme autonome (hors workspace de lymaille, `CARGO_TARGET_DIR=~/.cache/cargo-target/rig-probe`) qui interroge
`rig-core` **=0.43.0** (rustc 1.99.0 pour les mesures) contre un faux serveur HTTP local (`tokio::net`), sans
modèle réel. Lancer : `cargo run` dans ce dossier. Légende : **[exécuté]** = mesuré par la sonde ou par `cargo` ;
**[code]** = lu dans `~/.cargo/registry/src/*/rig-core-0.43.0/src/`.

## Dépendance et API retenue

```toml
rig-core = { version = "=0.43.0", default-features = false, features = ["reqwest"] }
```

L'API 0.43 n'a plus rien de commun avec 0.38 (anciens `CompletionModel`/`Agent`) : un client `OpenAI` = une
`OpenAIConfig` posée sur un transport ; un modèle = une « wire » (`Chat` pour chat completions).

```rust
let cfg = OpenAIConfig::with_key(&LLAMACPP, key).with_base_url(url); // key = "" : pas d'en-tête Authorization
let model = cfg.client().chat("nom-du-modèle");                     // Model<Chat>, chat completions (pas Responses)
let rep   = model.call(CompletionRequest::new(prompt)).await?;      // CompletionResponse
let flux  = model.stream(req)?;                                      // Streamed<Completion> (Stream d'Item)
let mods  = cfg.client().list_models().await?;                       // ModelList
```

`OpenAIConfig::client()` (feature `reqwest`) prend le client reqwest partagé de `rig-reqwest` ; `cfg.connect(http)`
accepte un `DynHttpClient` fourni par l'hôte.

## (1) Base configurable, clé optionnelle — exécuté + code

- Base : `OpenAIConfig::with_base_url(..)` ; la requête part sur `{base}/chat/completions` (la base doit donc porter
  `/v1`) : `POST /v1/chat/completions` **[exécuté]**. `completion_path` = `/chat/completions`, `models_path` = `/models`
  (`providers/openai/wire.rs`, `Quirks::openai()`).
- Clé optionnelle : il faut un dialecte `Auth::OptionalBearer` (« omis entièrement quand la clé est vide »). Le seul
  dialecte public qui le fait est `LLAMACPP` (`wire/dialects.rs`). Clé vide : aucun en-tête `Authorization`
  **[exécuté]** ; clé `sk-secret` : `Authorization: Bearer sk-secret` **[exécuté]**. Avec le dialecte `OPENAI` ou une
  `Dialect::gateway(..)` (`Auth::Bearer`) la clé est obligatoire. On peut définir son propre `const Dialect`
  (`Dialect::gateway(nom, base, env).with_quirks(Quirks{ auth: Auth::OptionalBearer, ..Quirks::openai() })`), mais
  `Dialect` n'est (dé)sérialisable que s'il est enregistré (`dialects::by_name`) : sans effet tant qu'on ne sérialise pas
  la config. Effets de bord de `LLAMACPP` : `rewrite: LlamaCpp` (refuse `ToolChoice::Function`, sans objet pour
  lymaille), `supports_image_tool_results: true`, `verify_path`. Le nom du dialecte (`llamacpp`) est l'**émetteur** du
  raisonnement (voir 6).
- `Debug` de `OpenAIConfig` : la clé est `[redacted]` mais **l'adresse de base s'affiche en clair**, ainsi que tout le
  dialecte **[exécuté]** → `RigConfig` doit garder son propre `Debug` opaque (déjà prévu par `rig.sdd`).

## (2) `reasoning` et `reasoning_content` — exécuté + code

Réponse unaire avec `reasoning` seul puis `reasoning_content` seul : dans les deux cas
`choice = [Reasoning(émetteur llamacpp, "je réfléchis"), Text("Bonjour")]` **[exécuté]**. Les deux champs présents
avec des valeurs différentes : `reasoning_content` gagne (`"B"`) **[exécuté]**. Code : `AssistantMessageWire` porte les
deux champs, `deserialize_assistant` fait `reasoning_content.or(reasoning)` (`providers/openai/completion/mod.rs`
l. 219-265) ; le delta de flux fait de même. Le raisonnement sort comme `AssistantContent::Reasoning(Sealed<Reasoning>)`,
séparé du `Text` : il n'est pas dans le texte. `usage.reasoning_tokens` est repris de
`completion_tokens_details.reasoning_tokens` **[exécuté]**.

Piège : un objet `usage` présent mais vide (`"usage": {}`) fait échouer le décodage (`missing field prompt_tokens`)
**[exécuté]** ; un `usage` absent est accepté (`Usage` non rapporté, `is_reported() == false`).

## (3) `additional_params` — exécuté : **il peut écraser le noyau**

`CompletionRequest.additional_params` est un `#[serde(flatten)]` placé en dernier (`completion/mod.rs` l. 1469-1482),
fusionné dans un `serde_json::Value` : à clé identique, **la valeur de la table libre l'emporte**. Avec une table
contenant `model`, `messages`, `tools`, `stream`, `temperature`, `top_k`, `top_p`, `chat_template_kwargs` :

```
unaire : {"chat_template_kwargs":{"preserve_thinking":true},"messages":[{"content":"PIRATE","role":"user"}],
          "model":"PIRATE","stream":true,"temperature":0.99,"top_k":20,"top_p":0.9}
flux   : {"messages":[],"model":"PIRATE","stream":true,"stream_options":{"include_usage":false},"top_k":5}
```

- `model`, `messages`, `temperature` écrasés **[exécuté]**. `stream` : écrasable en unaire (`"stream":true` injecté),
  mais le mode flux le **force** à `true` après coup (`json_utils::merge(body, {"stream": true})`, `wire/chat.rs`).
- `tools` : une clé `tools` de la table est retirée puis ses outils `function` sont fusionnés aux outils typés
  (`completion/mod.rs` l. 1747-1790) ; non fonction, elle reste dans la table. Ne pas compter dessus.
- `stream_options.include_usage` est ajouté par défaut en flux (`stream_include_usage`), mais respecté si la table le fixe.
- Conséquence pour l'adaptateur : **filtrer `model`, `messages`, `tools`, `tool_choice`, `stream`, `stream_options`
  (et `temperature`/`max_tokens` si ces champs sont typés ailleurs) de la table libre avant de la passer à rig**, ou
  fusionner soi-même après. `rig.sdd` « sans qu'aucune clé du profil n'écrase… » n'est **pas** garanti par rig.
- Sortie : les clés sont triées alphabétiquement (`serde_json::Map` sans `preserve_order`), sans effet.

## (4) Flux — exécuté + code

`model.stream(req)?` rend un `Streamed<Completion>` : `Stream<Item = Result<Item<StreamEvent>, ProviderError>>`
(`streaming/mod.rs`). Rien n'est envoyé avant le premier `poll`. Séquence mesurée sur un flux réel avec raisonnement
(`reasoning` puis `reasoning_content`), `Bon`, `jour`, un appel d'outil en deux fragments, un usage final :

```
Start{Part(0),Reasoning}  Reasoning{Part(0),"pen"}  Reasoning{Part(0),"se"}
Start{Part(1),Text}       Text{Part(1),"Bon"}       Text{Part(1),"jour"}   End{Part(1),Text}
Start{Part(2),ToolCall}   Arguments{Part(2),"{\"a\":1}"}                   End{Part(2),ToolCall}
```

- Deltas de texte : `StreamEvent::Text { part, text }` ; raisonnement : `StreamEvent::Reasoning { part, text }`
  (variantes distinctes → on ne transmet que `Text`, comme l'exige `rig.sdd`) ; appels d'outil : `Start{kind:ToolCall}`,
  `Arguments{json}` (JSON brut), `End{content: AssistantContent::ToolCall}` avec arguments décodés.
- Usage final : lu de l'événement `usage` (chunk sans `choices`) ; `include_usage:true` est envoyé par défaut
  (corps flux : `"stream":true,"stream_options":{"include_usage":true}`). Il ne s'obtient **que** par `.finish().await`
  (consomme le flux et rend la `CompletionResponse` complète : `choice`, `usage{11,7,18,reasoning 2}`,
  `finish_reason() = ToolCalls`) ; les `Item` n'en portent pas. Flux sans chunk d'usage : `usage.is_reported() == false`.
  Deltas **et** réponse finale : itérer `st.next()` (par `&mut st`) jusqu'à `None`, puis appeler `st.finish().await`
  sur le même flux : `deltas=["Bon","jour"]` puis la `CompletionResponse` complète (raisonnement, texte, appel
  d'outil, usage) **[exécuté, 4c]**.
- Arguments d'outil invalides **en flux** : le flux se termine par `Err(ProviderError::MalformedToolInput { name, id,
  raw, error })` (`name=skill id=call_2 raw="{pas json"`) **[exécuté]** ; les événements déjà émis (texte, appels clos)
  ont été livrés avant. L'appel erroné est donc récupérable (nom, identifiant, texte brut), mais l'erreur ferme le flux
  et `finish()` rend `Err` : pas de réponse complète.
- Arguments invalides **en unaire** : `Err(ProviderError::Json(..))` (« key must be a string ») pour **toute** la
  réponse **[exécuté]** : le texte et les autres appels sont perdus, le nom de l'appel fautif n'est pas rendu.

## (5) `list_models` et `max_model_len` — exécuté + code : **non reporté**

`ModelEntry` (`wire/modality.rs` l. 925-982) ne lit que `context_window`, puis `context_length`, puis
`max_context_length` ; sortie `u32`. Aucune lecture de `max_model_len`. Mesure, `/v1/models` répondant
`{"id":"vllm-model","max_model_len":262144}`, `{"id":"sans-info"}`, `{"id":"gateway","context_length":8192}` :

```
vllm-model  context_length=None
sans-info   context_length=None
gateway     context_length=Some(8192)
```

La requête est `GET /v1/models`, une page, sans pagination. `max_output_tokens` : `max_completion_tokens` ou
`top_provider.max_completion_tokens`, jamais vLLM. → **@detect_models doit lire `/v1/models` lui-même** (ou ne pas
utiliser `list_models`). Il faudrait alors un second client HTTP : `rig` n'expose pas de GET générique public (la wire
`Models` est `pub` mais le driver n'accepte que ses propres opérations) ; reqwest est de toute façon dans l'arbre.

## (6) Rejeu du raisonnement — exécuté + code

Historique `[user q1, assistant{Reasoning scellé par X + Text r1}, user q2]`. Corps envoyé **[exécuté]** :

- X = `llamacpp` (nom du dialecte, donc émetteur courant) :
  `{"content":[{"text":"r1","type":"text"}],"reasoning_content":"ma pensée","role":"assistant"}`
- X = `autre-modele` : même message **sans** `reasoning_content` (retiré silencieusement, aucune erreur).

Règles **[code]** : champ émis = `reasoning_content` uniquement (`#[serde(rename = "reasoning_content")]`, l. 198) ; on
ne peut pas choisir `reasoning` (vLLM accepte les deux, `spikes/inference-probe`). Condition : le `Reasoning` doit être
construit avec `Reasoning::new(texte).sealed(Issuer::from(<nom du dialecte>))`, et `scope_reasoning` /
`replayable_to` ne garde que ceux dont l'émetteur est accepté (`replay_issuers`: `[dialect.name]` quand
`upstream_reasoning_issuer == false`, le cas de `LLAMACPP`). L'adaptateur peut donc : pour un message dont
`@replayed_reasoning` rend un texte, sceller avec l'émetteur du dialecte ; sinon ne rien mettre. L'émetteur décidé par
le domaine `agents` (« uniquement pour un modèle qui l'a émis ») devient côté adaptateur « le même nom de dialecte »
→ prévoir une correspondance (`modèle émetteur` ↔ sceau). Le contenu texte de l'assistant part sous forme de tableau
de parties (`[{"type":"text",...}]`), pas de chaîne ; non vérifié contre vLLM (l'essai `inference-probe` utilisait
des chaînes).

## (7) Erreurs HTTP — exécuté + code

`ProviderError` (`error.rs` l. 358-411). Pour une réponse non 2xx : `ProviderError::ProviderResponse(r)` avec
`provider_response_status() -> Option<StatusCode>` et `provider_response_body() -> Option<&str>` (corps verbatim).
Mesures **[exécuté]** :

| Cas | `provider_response_status()` | `is_retryable()` | Variante |
| --- | --- | --- | --- |
| 429 | 429 | oui | `ProviderResponse` |
| 500 | 500 | oui | `ProviderResponse` |
| 401 / 403 | 401 / 403 | non | `ProviderResponse` (pas `InvalidAuthentication` : réservé à `verify`) |
| 400 dépassement de contexte | 400 | non | `ProviderResponse`, corps verbatim |
| 200 avec enveloppe `{"error":…}` | 200 | non | `ProviderResponse` (statut 2xx) |
| connexion refusée / coupée | `None` | oui | `Http` |
| corps 200 illisible | — | non | `Json` (`kind() == Json`) |

- **Dépassement de contexte** : rig ne le détecte pas (aucune occurrence dans le crate). Il faut chercher dans
  `provider_response_body()` (vLLM : `"maximum context length"`, `code: context_length_exceeded`, ou le texte de
  `"Please reduce the length"`). Les motifs restent à fixer par l'adaptateur, sur le statut 400/413 + corps.
- **Fuite** : `Display` de `Http` contient l'**URL complète** (`error sending request for url
  (http://127.0.0.1:43925/v1/chat/completions)`) et le `Debug` aussi **[exécuté]** ; `Display` de `ProviderResponse`
  contient le corps. L'adaptateur ne doit **jamais** reprendre `to_string()`/`Debug` : construire son propre message
  à partir de la classe (statut, `kind()`), conformément à `rig.sdd` (« ni la requête, ni l'adresse, ni la clé »).
- La classe `Transient`/`Permanent` de `rig.sdd` correspond à `is_retryable()` pour tout sauf `Truncated`
  (retryable) ; le corps JSON illisible est bien `Permanent` (`is_retryable() == false`).

## (8) Rust minimal, features, arbre de dépendances — exécuté + code

- **Rust minimal : 1.95.0, édition 2024** (`rust-version` de `rig-core`, `rig-http`, `rig-reqwest`) : identique au
  `rust-version = "1.95.0"` de lymaille. Aucun écart.
- Features de `rig-core` 0.43 : `derive` et `rustls` par défaut ; `reqwest` (transport), `native-tls`, `websocket`,
  `tungstenite`, `audio`, `image`, `pdf`, `epub`, `test-utils`. **`default-features = false` compile et passe tous les
  essais** de la sonde. Sans `reqwest`, `OpenAIConfig::client()` n'existe pas : l'hôte devrait fournir son propre
  `DynHttpClient`. `rig-core/rustls` active `rig-reqwest/rustls` = `reqwest/rustls` (**aws-lc-rs**, `system-proxy`,
  `http2`, `charset`) : lymaille a choisi `rustls-no-provider` (l'hôte installe son fournisseur, cf. `rmcp`
  `reqwest-tls-no-provider`) → **ne pas** activer `rustls`.
- Sans TLS, `reqwest` (feature `reqwest` seule) ne sait que le `http://`. Pour `https://` (la passerelle `smart`),
  ajouter `reqwest = { version = "0.13", default-features = false, features = ["rustls-no-provider"] }` dans lymaille
  (même `reqwest` 0.13.5 que `rmcp`, par unification des features) ; avec la feature `mcp-http` activée (défaut) c'est
  déjà le cas. **Non exécuté** : aucune connexion HTTPS dans la sonde.
- Arbre (`cargo tree -e normal --prefix none | sort -u | wc -l`, copie du `Cargo.toml`/`Cargo.lock` de lymaille dans
  le scratchpad, lymaille intact) **[exécuté]** :

  | Configuration | Crates (lignes uniques) | Écart |
  | --- | --- | --- |
  | lymaille actuel (défauts) | 265 | |
  | + `rig-core` sans défaut, sans transport | 306 | +41 |
  | + `rig-core` `["reqwest"]` (avec `mcp-http`) | 311 | +46 |
  | + `rig-core` `["reqwest"]` + `reqwest/rustls-no-provider`, `--no-default-features` | 306 (base sans défauts : 209) | +97 |
  | + `rig-core` défauts + `reqwest` (derive + rustls/aws-lc) | 371 | +106 |

  Sonde seule (`rig-core` `["reqwest"]`, tokio, serde_json, futures) : 147 crates ; doublons : `base64` 0.22/0.23, `syn`.
- **Doublons dans lymaille** (`cargo tree -d`) : ajoute `base64` 0.22.1 (`rig-core`) face au 0.23.1 direct de lymaille
  → `clippy::multiple_crate_versions` refusera ; il faut ajouter `"base64"` à `allowed-duplicate-crates` de
  `clippy.toml` (décision du développeur principal). `hashbrown`/`indexmap` ne doublonnent qu'avec la feature `derive`
  (non retenue). Rien d'autre de nouveau.
- **Licences** : `cargo deny check licenses bans` sur ces copies (avec `deny.toml` de lymaille, `all-features`) :
  `bans ok, licenses ok` pour les cinq configurations ; `rig-core`, `rig-http`, `rig-reqwest` sont sous MIT. Les
  avis de sécurité (`advisories`) n'ont pas été vérifiés (pas de base d'avis hors ligne).
- `rig-core` ne lit pas l'environnement sauf via `from_env`/`credential_from_env` (non utilisés) ; le crate dépend de
  `tracing` (journalise le corps de requête au niveau `trace` : `trace_json(... "OpenAI Chat Completions request")` —
  à garder désactivé, le corps contient l'historique). `rig-reqwest` sans `rustls` n'active pas `system-proxy`
  (pas de lecture de `HTTP_PROXY`).

## Features recommandées

`rig-core = { version = "=0.43.0", default-features = false, features = ["reqwest"] }`, plus la feature de TLS de
`reqwest` déjà choisie par lymaille (`rustls-no-provider`). Ni `derive`, ni `rustls`, ni `websocket`, ni `image`/`audio`.

## Ce qui complique l'adaptateur

1. `additional_params` écrase `model`/`messages`/`temperature` (3) : filtrage obligatoire côté adaptateur.
2. `max_model_len` non reporté (5) : lecture directe de `/v1/models` (et `list_models` inutile).
3. Messages d'erreur de `rig` qui contiennent l'URL ou le corps (7) : ne jamais les relayer ; détection du
   dépassement de contexte à écrire sur le corps.
4. Arguments d'outil invalides : fatals pour `call()` (toute la réponse perdue, `Json`) et pour le flux
   (`MalformedToolInput`, flux clos). Pour tenir « jamais perdu en silence / rendu comme appel erroné », implémenter
   `complete` par `stream` et accumuler les `End` soi-même ; récupérer l'appel fautif dans
   `MalformedToolInput { name, id, raw }`.
5. Flux + réponse finale : résolu (itérer jusqu'à `None` puis `finish()`), mais l'erreur `MalformedToolInput` ferme
   le flux : la réponse complète n'existe plus, l'adaptateur doit reconstruire à partir des événements `End` déjà vus.
6. Clé optionnelle ⇒ dialecte `LLAMACPP` (ou dialecte maison) ; le nom du dialecte est l'émetteur du raisonnement.
7. Dépendance : `base64` en doublon (`clippy.toml`).
