# Sonde `rig-core` 0.43.0 — embeddings : `Model<Embeddings>`, lots, erreurs, `DynModel<Embedding>` factice

Programme autonome (hors workspace de lymaille, `CARGO_TARGET_DIR=~/.cache/cargo-target/rig-embed-probe`,
`CARGO_BUILD_JOBS=2`, `CARGO_PROFILE_DEV_DEBUG=0`) qui interroge `rig-core` **=0.43.0** (rustc stable de la chaîne
lymaille) contre un faux serveur HTTP local (`tokio::net`), sans modèle réel. Lancer : `cargo run` dans ce dossier.
Légende : **[exécuté]** = mesuré par la sonde ou par `cargo` ; **[code]** = lu dans
`~/.cargo/registry/src/*/rig-core-0.43.0/src/`. Modèle : même serveur et même style que `spikes/rig-probe` (chat).

## Dépendance et API retenue

```toml
rig-core = { version = "=0.43.0", default-features = false, features = ["reqwest"] }
```

## (1) Construire un `Model<Embeddings>` (wire OpenAI-compatible) — exécuté + code

Le code exact, calqué sur le chat de `rig-probe` (base arbitraire, clé optionnelle par le dialecte `LLAMACPP`,
`Auth::OptionalBearer`) :

```rust
use rig_core::providers::openai::wire::{LLAMACPP, OpenAIConfig};

let cfg = OpenAIConfig::with_key(&LLAMACPP, key).with_base_url(url); // key = "" : pas d'en-tête Authorization
let model = cfg.client().embedding("nom-du-modele", None);           // Model<Embeddings> ; None = ndims non déclaré
let rep   = model.call(vec!["texte un".into(), "texte deux".into()]).await?; // EmbeddingResponse
```

- `OpenAI::embedding(model, ndims) -> Model<Embeddings>` (`client/openai.rs` l. 72-74) ; le moyen public le plus
  direct passe par le client. Alternative sans client : `Model::new(Embeddings::new(cfg, nom, ndims), transport)` —
  `Embeddings::new` et `Model::new` sont `pub` (`wire/modality.rs` l. 114, `driver.rs` l. 61) **[code]**.
- Chemin : le dialecte `LLAMACPP` hérite de `Quirks::openai()` → `embeddings_path = "/embeddings"` ; avec une base
  `http://h:p/v1` la requête mesurée est `POST /v1/embeddings` **[exécuté]**.
- Corps mesuré : `{"input":["alpha","beta"],"model":"probe-model"}` — tableau de chaînes brut, pas d'objets par
  entrée ; `model` présent (`sends_model_field: true`) ; **aucun champ de largeur** avec `LLAMACPP`
  (`DimensionsField::Ignored`, `requested_width()` rend `None`) **[exécuté]**.
- Clé vide : aucun en-tête `Authorization` ; clé `sk-secret` : `Authorization: Bearer sk-secret` **[exécuté]** —
  comme pour le chat, la clé optionnelle exige le dialecte `LLAMACPP` (ou un dialecte maison avec
  `Auth::OptionalBearer`).
- `EmbeddingResponse` : `embeddings: Vec<Embedding{document, vec: Vec<f64>}>`, `usage`, `provider` (= nom du
  dialecte, `"llamacpp"`), `model` (= nom signalé par le serveur, repli sur le nom du wire s'il n'en signale aucun :
  `Some("probe-model")` mesuré sur une réponse sans champ `model` **[exécuté]**), `response_id`,
  `provider_request_id`, `raw`.

## (2) Lot de N textes : ordre, `index`, taille maximale — exécuté + code

- **Ordre préservé** : 5 entrées, réponse en ordre avec `vec[0]` marquant la position d'entrée →
  `t0->[0,..] t1->[1,..] … t4->[4,..]` **[exécuté]**. Le couplage est **positionnel** :
  `Embedded::zipped` fait `documents.zip(vectors)` (`operation/modality.rs` l. 235-256) ; le champ `index` de la
  réponse est **lu par personne** — `EmbeddingData.index` existe (`providers/openai/embedding.rs` l. 46) mais le
  décodeur ne le consulte pas (`wire/modality.rs` l. 259-270) **[code]**.
- **`index` ignoré, mesuré** : même lot, `data` renvoyé **à l'envers** avec `index` = position vraie →
  `t0->[2,..] t1->[1,..] t2->[0,..]` : chaque document reçoit le vecteur de sa **position dans `data`**, pas celle
  qu'annonce `index` **[exécuté]**. Conclusion : la réponse **doit arriver dans l'ordre de la requête** ; un serveur
  qui répond en désordre avec des `index` corrects est silencieusement mal couplé.
- **Taille de lot** : `call(2000)` part en **une seule requête** de 2000 entrées, rig n'impose rien sur ce chemin
  (aucune vérification de `max_documents` dans le driver — occurrence seulement dans `EmbeddingsBuilder`)
  **[exécuté + code]**. `EmbeddingsBuilder` débite en tranches de `capabilities().max_documents` : 2 requêtes
  [1024, 976] mesurées avec le dialecte `LLAMACPP` (`EmbeddingQuirks::openai().max_documents = 1024`), ordre des
  documents reconstitué par emplacements (l. 96-186) **[exécuté + code]**.
- `usage` dérivé : réponse avec `prompt_tokens:15, total_tokens:20` et `completion_tokens` absent →
  `Usage{input:15, output:5, total:20}` (l'absence est comblée par `total - prompt`) **[exécuté]**.

## (3) Comportements en erreur — exécuté + code

Le trait commun à tout est `rig_core::error::ProviderError` ; `is_retryable()` est la seule classification
structurelle disponible (≈ la classe `Transient` de lymaille ; tout le reste est `Permanent`).

| Cas | Variante | `kind()` | `is_retryable()` | Détail mesuré |
| --- | --- | --- | --- | --- |
| connexion refusée / coupée | `Http` | `Http` | **oui** | `Display` contient **l'URL complète** (fuite, comme en chat) |
| 200, corps non JSON | `Json` | `Json` | non | `expected value at line 1 column 1` |
| 200, enveloppe `{"error":…}` | `Json` | `Json` | non | **l'enveloppe d'erreur 2xx n'est PAS reconnue en embeddings** (`missing field data`) — contrairement au chat (`ProviderResponse`) |
| 200 sans `usage` | `Response` | `Response` | non | `llamacpp embedding response omitted required usage` — le dialecte `LLAMACPP` **exige** `usage` (`requires_usage: true`) |
| 200 `"usage":{}` | `Json` | `Json` | non | `missing field prompt_tokens` — le wire exige `prompt_tokens` et `total_tokens` |
| 2 vecteurs pour 3 entrées (et 3 pour 2) | `Response` | `Response` | non | `provider returned 2 embeddings for 3 documents` — **comptage strict dans les deux sens**, mais en texte seulement (variantes `Response(String)`, pas de champs structurés) |
| dimension inattendue, `ndims` non déclaré | — | — | — | **aucune vérification** : modèle inconnu ET modèle connu (table 1536) laissent passer des vecteurs de dimension 4 ; `honour_declaration` ne regarde que `declared`, jamais `capabilities.ndims` |
| dimension inattendue, `ndims=Some(8)` | `MismatchedDimensions{provider, requested:8, returned:4}` | `Response` | non | le corps envoyé ne porte aucun champ largeur (`Ignored`) : la déclaration n'agit que comme contrôle du retour |
| `encoding_format = base64` | `RequestError` | `Request` | non | refusé **avant tout socket** (« Rig cannot decode … encoded as `base64` »), 0 requête partie |
| HTTP 400 (entrée trop longue, simulé) | `ProviderResponse` | `ProviderResponse` | non | statut 400 + corps verbatim dans `Display` |
| HTTP 429 / 500 | `ProviderResponse` | `ProviderResponse` | **oui** | — |
| HTTP 401 | `ProviderResponse` | `ProviderResponse` | non | pas `InvalidAuthentication` (réservé à `verify`) |

Classable : **oui**, par `is_retryable()` + `kind()` + `provider_response_status()` — la table complète est ci-dessus.
Pièges : le comptage et l'absence d'`usage` ne sont classables que comme `Response` non rejouable (le détail est dans
la chaîne `Display`, sans champs structurés) ; `Http` et `ProviderResponse` **fuient l'URL et le corps dans
`Display`** — ne jamais les relayer tels quels, construire son propre message comme en chat.

## (4) Nom du modèle et dimension (`ndims`) côté rig — exécuté + code

```rust
let m = cfg.client().embedding("text-embedding-3-small", None);
m.name();          // "llamacpp"  — nom du dialecte (descripteur), pas le nom du modèle
m.id();            // Some("text-embedding-3-small") — le nom passé à embedding()
m.capabilities();  // Capabilities { ndims, declared, max_documents, .. }
```

`ndims` résolu par `Capabilities` : l'argument `ndims` du donneur d'ordre, sinon la table de largeurs du dialecte
(vide pour `LLAMACPP`), sinon la table OpenAI partagée (`model_dimensions_from_identifier` : 3072/1536/1536 pour les
`text-embedding-3-large/small` et `ada-002`), sinon **0 = inconnu** **[code]**. Mesuré **[exécuté]** :

| modèle | argument ndims | capabilities.ndims | declared | max_documents |
| --- | --- | --- | --- | --- |
| `probe-model` (inconnu) | `None` | 0 | `None` | 1024 |
| `text-embedding-3-small` | `None` | 1536 | `None` | 1024 |
| `probe-model` | `Some(8)` | 8 | `Some(8)` | 1024 |

Le nom côté **réponse** vient de `EmbeddingResponse.model` (serveur, sinon wire) **[exécuté]**. La dimension effective
des vecteurs rendus nulle part d'autre que `response.embeddings[*].vec.len()` — `capabilities().ndims` est une
déclaration, pas une mesure, et n'est comparée à la réponse que via `declared`.

## (5) `DynModel<Embedding>` sans HTTP et `InMemoryVectorIndex` — exécuté

**Possible avec une struct maison ; pas avec une closure.** Il n'existe aucune fabrique publique à partir d'une
fermeture (`from_fn` et assimilés : aucune occurrence **[code]**) ; la porte d'entrée est le trait public
`Transport<W>` (`driver.rs` l. 72-78) et `Model::new(wire, transport).erase()`. Deux chemins mesurés :

**5a — wire `driver::Local<Embedding>` (conçue pour ça), zéro HTTP, déterminisme complet :**

```rust
use rig_core::driver::{Exchange, Local, Model, Opened, Opening, Step, Transport};
use rig_core::embeddings::{Embedding, EmbeddingResponse};
use rig_core::operation::Embedding as EmbeddingOp;

#[derive(Clone)]
struct FakeEmbed;

impl Transport<Local<EmbeddingOp>> for FakeEmbed {
    fn send(&self, texts: Vec<String>, _: Exchange) -> Opening<Step<EmbeddingOp>> {
        let response = EmbeddingResponse::new(
            texts.iter().map(|t| Embedding { document: String::new(), vec: fake_vec(t) }).collect());
        Opening::ready(Opened::new(futures::stream::once(async move { Ok(Step::End(response)) })))
    }
}

fn fake_vec(text: &str) -> Vec<f64> {
    (0..8).map(|i| text.matches(char::from(b'a' + i as u8)).count() as f64 + 0.01).collect()
}

let fmodel = Model::new(
    Local::<EmbeddingOp>::new("fake").with_id("fake-embed-1")
        .with_capabilities(Capabilities::embedding(usize::MAX, 8)),
    FakeEmbed,
).erase();                                  // DynModel<EmbeddingOp>

let store = InMemoryVectorStore::from_documents(pairs);  // pairs : (document, vec![Embedding]) pré-calculés
let index = store.index(fmodel.clone());                 // InMemoryVectorIndex
let hits = index.top_n::<String>(
    VectorSearchRequest::builder().query("alpha").samples(2).build()).await?;
```

Mesuré **[exécuté]** : `call` remplit `document` (le fold `Embedded` zipe les entrées même en local) ; deux appels
identiques → vecteurs identiques ; `embed_text`, `top_n` (`0.905 doc0 "alpha beta" ; 0.778 doc1 …`, cosinus
décroissant, identifiants `doc{n}`) et `top_n_ids` fonctionnent ; `name()="fake"`, `id()=Some("fake-embed-1")`,
`capabilities().ndims=8` (le faux modèle peut donc se faire passer pour un modèle de dimension connue).

**5b — Transport maison sur la wire réelle `Embeddings`** : `Model::new(Embeddings::new(cfg, "wire-fake", None),
WireFake).erase()` où `WireFake::send` lit `payload.request.body()` (`wire::Body::Bytes`) — le corps vu est
exactement `{"input":["x","y"],"model":"wire-fake"}` — et répond un `WireFrame::Bytes(json)` avec
`Opened::with_http(StatusCode::OK, ..)` **[exécuté]**. Utile pour tester l'adaptateur en gardant l'encodage réel de
la wire, sans serveur.

Sans HTTP sur la vraie wire : NON MESURÉ sans `with_http` (la variante 5b l'a toujours fourni).

## Non mesuré

- Serveur réel (vLLM, llama-server) : ordre réellement rendu, présence de `usage`, 400 pour entrée trop longue en
  embeddings — tout est simulé par le faux serveur.
- HTTPS, `encoding_format` autre que la valeur refusée, tranches `EmbeddingsBuilder` en parallèle réel
  (`buffer_unordered` : la doc dit l'ordre reconstruit, le cas « lots finis en désordre » n'a pas été déclenché).
- `Transport` maison sans `with_http` sur la wire HTTP.

## Conclusion — coût de l'adaptateur `Embedder` (trait lymaille) → `DynModel` de rig

1. **Faible** : une `OpenAIConfig` par profil + `client.embedding(nom, ndims).erase()` donne le
   `DynModel<Embedding>` ; `embed(textes)` = `call(Vec<String>)` puis le champ `embeddings` (documents déjà
   remplis) — une vingtaine de lignes de passage.
2. Deux obligations côté adaptateur : **passer le `ndims` du profil de façon explicite** (rig ne vérifie la
   dimension que déclarée) et **ne jamais relayer `Display`** (URL et corps qui fuient) — classer par
   `is_retryable()`/`kind()`/statut comme pour le chat.
3. **Contrat d'ordre à vérifier contre un serveur réel** : rig couple positionnellement et ignore `index` ;
   un serveur qui répond en désordre corrompt silencieusement le couplage document↔vecteur.
4. `usage` est **exigé** par le dialecte `LLAMACPP` (une réponse sans `usage` est une erreur `Response`) et rig
   n'impose **aucune** taille de lot sur `call` — l'adaptateur débite lui-même si le serveur impose sa limite.
5. Pour les tests lymaille, un faux modèle déterministe coûte ~15 lignes (`driver::Local` + `Transport` maison,
   zéro HTTP) et s'insère tel quel dans `InMemoryVectorIndex` via `store.index(model)` — l'extension « connaissance »
   (`dsd/agent-sessions/modules.md`) est testable sans réseau.
