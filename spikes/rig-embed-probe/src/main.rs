//! Sonde rig-core 0.43.0 : faux serveur HTTP local + client OpenAI-compatible (embeddings),
//! et modèle d'embedding factice sans HTTP (driver::Local, Transport maison).
use std::sync::{Arc, Mutex};

use futures::stream;
use rig_core::driver::{Exchange, Local, Model, Opened, Opening, Step, Transport};
use rig_core::embeddings::{Embedding, EmbeddingResponse, EmbeddingsBuilder};
use rig_core::error::ProviderError;
use rig_core::http_client::{HeaderMap, StatusCode};
use rig_core::operation::Embedding as EmbeddingOp;
use rig_core::providers::openai::embedding::EncodingFormat;
use rig_core::providers::openai::wire::{Embeddings, LLAMACPP, OpenAIConfig};
use rig_core::vector_store::in_memory_store::InMemoryVectorStore;
use rig_core::vector_store::{VectorSearchRequest, VectorStoreIndex};
use rig_core::wire::{Body, Capabilities, Encoded, WireFrame};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, Vec<(String, String)>, String)>>>);

/// Serveur : répond selon `handler(corps)` à toute requête, en journalisant (ligne, en-têtes, corps).
async fn serve_fn<F>(handler: F) -> (String, Seen)
where
    F: Fn(String) -> (u16, String) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Seen::default();
    let log = seen.clone();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let log = log.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let (head_end, clen) = loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let clen = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                        break (p + 4, clen);
                    }
                };
                while buf.len() < head_end + clen {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { break; }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.lines();
                let first = lines.next().unwrap_or("").to_owned();
                let hdrs = lines.filter_map(|l| l.split_once(": ")).map(|(a, b)| (a.to_lowercase(), b.to_owned())).collect();
                let rbody = String::from_utf8_lossy(&buf[head_end..]).to_string();
                log.0.lock().unwrap().push((first, hdrs, rbody.clone()));
                let (status, body) = handler(rbody);
                let resp = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/v1"), seen)
}

async fn serve(status: u16, body: String) -> (String, Seen) {
    serve_fn(move |_| (status, body.clone())).await
}

fn cfg(url: &str, key: &str) -> OpenAIConfig {
    OpenAIConfig::with_key(&LLAMACPP, key).with_base_url(url)
}

/// Réponse d'echo : un vecteur [position, 0, 0] par texte reçu, dans l'ordre de l'entrée.
fn echo(body: String) -> (u16, String) {
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let inputs = v["input"].as_array().cloned().unwrap_or_default();
    let data: Vec<Value> = inputs.iter().enumerate()
        .map(|(k, _)| json!({"object":"embedding","index":k,"embedding":[k as f64, 0.0, 0.0]}))
        .collect();
    let n = inputs.len() as u64;
    // completion_tokens volontairement absent : to_normalized doit le dériver du total.
    (200, json!({"object":"list","model":"srv-model","data":data,"usage":{"prompt_tokens":3*n,"total_tokens":4*n}}).to_string())
}

fn data_body(dim: f64, count: usize) -> String {
    let data: Vec<Value> = (0..count).map(|k| json!({"object":"embedding","index":k,"embedding":vec![dim; 4]})).collect();
    json!({"object":"list","model":"srv-model","data":data,"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string()
}

fn variant(e: &ProviderError) -> &'static str {
    match e {
        ProviderError::Http(_) => "Http",
        ProviderError::Json(_) => "Json",
        ProviderError::Response(_) => "Response",
        ProviderError::ProviderResponse(_) => "ProviderResponse",
        ProviderError::MismatchedDimensions { .. } => "MismatchedDimensions",
        ProviderError::Truncated => "Truncated",
        _ => "Autre",
    }
}

/// Appelle `model.call(inputs)` sur `url` et rend le compte-rendu d'erreur (ou Ok).
async fn probe_err(label: &str, url: String, inputs: Vec<String>, ndims: Option<usize>) {
    match cfg(&url, "sk-secret").client().embedding("m", ndims).call(inputs).await {
        Ok(r) => println!("[3] {label}: Ok -> {}", show(&r.embeddings)),
        Err(e) => println!("[3] {label}: variante={} status={:?} kind={:?} retryable={} body={:?} Display={:?}",
            variant(&e), e.provider_response_status(), e.kind(), e.is_retryable(), e.provider_response_body(), e.to_string()),
    }
}

fn show(emb: &[Embedding]) -> String {
    emb.iter().map(|e| format!("{}->{:?}", e.document, e.vec)).collect::<Vec<_>>().join(" ")
}

#[tokio::main]
async fn main() {
    // ---- (1) construire Model<Embeddings> : base, clé, corps, chemin
    {
        let (url, seen) = serve(200, json!({"object":"list","model":"srv-model",
            "data":[{"object":"embedding","index":0,"embedding":[0,0,0]},{"object":"embedding","index":1,"embedding":[1,0,0]}],
            "usage":{"prompt_tokens":6,"total_tokens":8}}).to_string()).await;
        let model = cfg(&url, "").client().embedding("probe-model", None);
        let r = model.call(vec!["alpha".to_string(), "beta".to_string()]).await.unwrap();
        println!("[1] embeddings: {}", show(&r.embeddings));
        println!("[1] usage={:?} reported={} provider={} model={:?} response_id={:?}",
            r.usage, r.usage.is_reported(), r.provider, r.model, r.response_id);
        let s = seen.0.lock().unwrap();
        println!("[1] requête: {} ; authorization={:?}", s[0].0, s[0].1.iter().find(|h| h.0 == "authorization"));
        println!("[1] corps: {}", s[0].2);
        let (url, seen) = serve(200, data_body(0.0, 1)).await;
        cfg(&url, "sk-secret").client().embedding("m", None).call(vec!["x".to_string()]).await.unwrap();
        let s = seen.0.lock().unwrap();
        println!("[1] avec clé: authorization={:?}", s[0].1.iter().find(|h| h.0 == "authorization"));
    }

    // ---- (2) lot : ordre, index, taille
    {
        let (url, _) = serve_fn(echo).await;
        let inputs: Vec<String> = (0..5).map(|k| format!("t{k}")).collect();
        let r = cfg(&url, "").client().embedding("m", None).call(inputs).await.unwrap();
        println!("[2a] ordre (document->vecteur, vec[0] marque la position d'entrée) : {}", show(&r.embeddings));
        println!("[2a] usage dérivé (prompt_tokens=15, completion_tokens absent, total=20) : {:?}", r.usage);
    }
    {
        // data en ordre inverse, index = position VRAIE du texte ; vec[0] = position dans data.
        let (url, seen) = serve_fn(|body: String| -> (u16, String) {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let n = v["input"].as_array().map_or(0, |a| a.len());
            let data: Vec<Value> = (0..n).rev()
                .map(|k| json!({"object":"embedding","index":k,"embedding":[k as f64, 0.0, 0.0]}))
                .collect();
            (200, json!({"object":"list","model":"srv-model","data":data,"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string())
        }).await;
        let r = cfg(&url, "").client().embedding("m", None).call(vec!["t0".into(), "t1".into(), "t2".into()]).await.unwrap();
        println!("[2b] data renvoyée à l'envers (index=k partout, ordre=data inverse) : {}", show(&r.embeddings));
        let s = seen.0.lock().unwrap();
        println!("[2b] (une requête attendue: {})", s.len());
    }
    {
        // lot de 2 000 via call : rig impose-t-il une taille ?
        let (url, seen) = serve_fn(echo).await;
        let inputs: Vec<String> = (0..2000).map(|k| format!("t{k}")).collect();
        let r = cfg(&url, "").client().embedding("m", None).call(inputs).await.unwrap();
        let s = seen.0.lock().unwrap();
        let sizes: Vec<usize> = s.iter().map(|(_, _, b)| serde_json::from_str::<Value>(b).map_or(0, |v| v["input"].as_array().map_or(0, |a| a.len()))).collect();
        let ordered = r.embeddings.iter().enumerate().all(|(k, e)| e.document == format!("t{k}") && e.vec.first() == Some(&(k as f64)));
        println!("[2c] call(2000) : {} requête(s) de tailles {sizes:?} ; ordre intact: {}", s.len(), ordered);
    }
    {
        // lot de 2 000 via EmbeddingsBuilder
        let (url, seen) = serve_fn(echo).await;
        let inputs: Vec<String> = (0..2000).map(|k| format!("t{k}")).collect();
        let model = cfg(&url, "").client().embedding("m", None).erase();
        let built = EmbeddingsBuilder::new(model).documents(inputs).unwrap().build().await.unwrap();
        let s = seen.0.lock().unwrap();
        let sizes: Vec<usize> = s.iter().map(|(_, _, b)| serde_json::from_str::<Value>(b).map_or(0, |v| v["input"].as_array().map_or(0, |a| a.len()))).collect();
        let ordered = built.iter().enumerate().all(|(k, (d, v))| *d == format!("t{k}") && v[0].vec.first() == Some(&((k % 1024) as f64)));
        println!("[2d] builder(2000) : {} requête(s) de tailles {sizes:?} ; ordre intact: {}", s.len(), ordered);
    }

    // ---- (3) comportements en erreur
    {
        // serveur indisponible : port refermé
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        probe_err("connexion refusée", format!("http://{a}/v1"), vec!["x".into(), "y".into()], None).await;
        // connexion coupée sans réponse
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(async move { loop { let (s, _) = l.accept().await.unwrap(); drop(s); } });
        probe_err("connexion coupée", format!("http://{a}/v1"), vec!["x".into(), "y".into()], None).await;
    }
    {
        let (url, _) = serve(200, "pas du json".to_owned()).await;
        probe_err("200 corps non JSON", url, vec!["x".into()], None).await;
        let (url, _) = serve(200, json!({"error":{"message":"enveloppe 2xx"}}).to_string()).await;
        probe_err("200 enveloppe error", url, vec!["x".into()], None).await;
        let (url, _) = serve(200, json!({"object":"list","model":"m","data":[{"object":"embedding","index":0,"embedding":[1,2,3]}]}).to_string()).await;
        probe_err("200 sans usage", url, vec!["x".into()], None).await;
        let (url, _) = serve(200, json!({"object":"list","model":"m","data":[{"object":"embedding","index":0,"embedding":[1,2,3]}],"usage":{}}).to_string()).await;
        probe_err("200 usage vide", url, vec!["x".into()], None).await;
        let (url, _) = serve(200, data_body(7.0, 2)).await; // 2 vecteurs pour 3 entrées
        probe_err("2 vecteurs pour 3 entrées", url, vec!["x".into(), "y".into(), "z".into()], None).await;
        let (url, _) = serve(200, data_body(7.0, 3)).await; // 3 vecteurs pour 2 entrées
        probe_err("3 vecteurs pour 2 entrées", url, vec!["x".into(), "y".into()], None).await;
    }
    {
        // dimension : vérifiée seulement si le donneur d'ordre a déclaré ndims > 0
        let (url, _) = serve(200, data_body(4.0, 2)).await; // vecteurs de dimension 4
        probe_err("modèle inconnu, ndims=None, vecteurs dim 4", url, vec!["x".into(), "y".into()], None).await;
        let (url, seen) = serve(200, data_body(4.0, 2)).await;
        probe_err("ndims=Some(8), vecteurs dim 4", url, vec!["x".into(), "y".into()], Some(8)).await;
        let s = seen.0.lock().unwrap();
        println!("[3] corps envoyé avec ndims=Some(8) (llamacpp : champ dimensions ignoré) : {}", s[0].2);
        let (url, _) = serve(200, data_body(4.0, 2)).await;
        let r = cfg(&url, "").client().embedding("text-embedding-3-small", None).call(vec!["x".into(), "y".into()]).await;
        match r { Ok(r) => println!("[3] modèle connu (table 1536), ndims=None, vecteurs dim 4 : Ok, ndims déclaré={:?} capabilities.ndims={}", r.embeddings[0].vec.len(), cfg("http://x/v1", "").client().embedding("text-embedding-3-small", None).capabilities().ndims), Err(e) => println!("[3] modèle connu dim 4 : variante={} retryable={}", variant(&e), e.is_retryable()) }
        let (url, _) = serve(200, json!({"object":"list","model":"m","data":[{"index":0,"embedding":[1,2,3,4,5,6,7,8]},{"index":1,"embedding":[8,7,6,5,4,3,2,1]}],"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string()).await;
        probe_err("ndims=Some(8), vecteurs dim 8 (contrôle)", url, vec!["x".into(), "y".into()], Some(8)).await;
    }
    {
        for (status, body) in [
            (400, json!({"error":{"message":"maximum context length is 8192 tokens, requested 9000","type":"BadRequestError","code":"context_length_exceeded"}}).to_string()),
            (429, json!({"error":{"message":"slow down"}}).to_string()),
            (500, "boom".to_owned()),
            (401, json!({"error":{"message":"bad key"}}).to_string()),
        ] {
            let (url, _) = serve(status, body).await;
            probe_err(&format!("HTTP {status}"), url, vec!["x".into()], None).await;
        }
    }

    // une erreur d'encodage refuse la requête avant tout socket
    {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let m = Model::new(
            Embeddings::new(cfg("http://127.0.0.1:1/v1", ""), "m", None).with_encoding_format(EncodingFormat::Base64),
            WireFake(seen.clone()),
        );
        match m.call(vec!["x".into()]).await {
            Ok(_) => println!("[3] encoding_format=base64 : Ok ?!"),
            Err(e) => println!("[3] encoding_format=base64 : variante={} kind={:?} retryable={} Display={:?}", variant(&e), e.kind(), e.is_retryable(), e.to_string()),
        }
        println!("[3] requêtes parties avec base64 : {}", seen.lock().unwrap().len());
    }

    // ---- (4) nom du modèle et ndims côté rig
    {
        for (m, nd) in [("probe-model", None), ("text-embedding-3-small", None), ("probe-model", Some(8))] {
            let mm = cfg("http://127.0.0.1:1/v1", "").client().embedding(m, nd);
            println!("[4] {m:?} ndims_arg={nd:?} -> name()={:?} id()={:?} capabilities{{ndims: {}, declared: {:?}, max_documents: {}}}",
                mm.name(), mm.id(), mm.capabilities().ndims, mm.capabilities().declared, mm.capabilities().max_documents);
        }
        // nom côté réponse : signalé par le serveur, sinon repli sur le nom du wire
        let (url, _) = serve(200, json!({"object":"list","data":[{"index":0,"embedding":[1,2,3]}],"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string()).await;
        let r = cfg(&url, "").client().embedding("probe-model", None).call(vec!["x".into()]).await.unwrap();
        println!("[4] réponse sans champ model : model={:?} provider={}", r.model, r.provider);
    }

    // ---- (5a) DynModel<Embedding> factice sans HTTP : wire driver::Local + Transport maison
    {
        let fmodel = Model::new(
            Local::<EmbeddingOp>::new("fake").with_id("fake-embed-1").with_capabilities(Capabilities::embedding(usize::MAX, 8)),
            FakeEmbed,
        ).erase();
        println!("[5a] name()={:?} id()={:?} capabilities.ndims={}", fmodel.name(), fmodel.id(), fmodel.capabilities().ndims);
        let one = fmodel.embed_text("alpha").await.unwrap();
        println!("[5a] embed_text(\"alpha\") -> {:?}", one.vec);
        let docs = fmodel.call(vec!["alpha beta".to_string(), "gamma delta".to_string(), "omega zeta".to_string()]).await.unwrap().embeddings;
        println!("[5a] call 3 documents : {}", docs.iter().map(|e| format!("{} -> {:?}", e.document, e.vec)).collect::<Vec<_>>().join(" | "));
        let store = InMemoryVectorStore::from_documents(docs.iter().map(|e| (e.document.clone(), vec![e.clone()])).collect::<Vec<_>>());
        let index = store.index(fmodel.clone());
        let req = VectorSearchRequest::builder().query("alpha").samples(2).build();
        let hits = index.top_n::<String>(req).await.unwrap();
        println!("[5a] top_n(\"alpha\", 2) : {}", hits.iter().map(|(s, id, d)| format!("{s:.3} {id} {d:?}")).collect::<Vec<_>>().join(" ; "));
        let req = VectorSearchRequest::builder().query("omega").samples(3).build();
        let ids = index.top_n_ids(req).await.unwrap();
        println!("[5a] top_n_ids(\"omega\", 3) : {}", ids.iter().map(|(s, id)| format!("{s:.3} {id}")).collect::<Vec<_>>().join(" ; "));
        let again = fmodel.call(vec!["alpha beta".to_string(), "gamma delta".to_string(), "omega zeta".to_string()]).await.unwrap().embeddings;
        let same = docs.iter().zip(&again).all(|(a, b)| a.document == b.document && a.vec == b.vec);
        println!("[5a] déterminisme (2 appels, documents et vecteurs) : {same}");
    }

    // ---- (5b) même sans HTTP en gardant la wire réelle Embeddings : Transport maison sur Encoded
    {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let wmodel = Model::new(Embeddings::new(OpenAIConfig::with_key(&LLAMACPP, ""), "wire-fake", None), WireFake(seen.clone())).erase();
        let r = wmodel.call(vec!["x".to_string(), "y".to_string()]).await.unwrap();
        println!("[5b] call sans HTTP : {} ; model={:?}", show(&r.embeddings), r.model);
        println!("[5b] corps que la wire réelle a produit : {}", seen.lock().unwrap()[0]);
    }
}

/// Vecteur déterministe : 8 composantes = occurrences des lettres a..h (+0,01 pour éviter le zéro).
fn fake_vec(text: &str) -> Vec<f64> {
    (0..8).map(|i| text.matches(char::from(b'a' + i as u8)).count() as f64 + 0.01).collect()
}

/// Modèle d'embedding factice : la payload EST la requête, le pas End est la réponse.
#[derive(Clone)]
struct FakeEmbed;

impl Transport<Local<EmbeddingOp>> for FakeEmbed {
    fn send(&self, texts: Vec<String>, _exchange: Exchange) -> Opening<Step<EmbeddingOp>> {
        let response = EmbeddingResponse::new(
            texts.iter().map(|t| Embedding { document: String::new(), vec: fake_vec(t) }).collect(),
        );
        Opening::ready(Opened::new(stream::once(async move { Ok(Step::End(response)) })))
    }
}

/// Transport sans HTTP sur la wire OpenAI réelle : lit le corps Encoded, répond un WireFrame JSON.
#[derive(Clone, Default)]
struct WireFake(Arc<Mutex<Vec<String>>>);

impl Transport<Embeddings> for WireFake {
    fn send(&self, payload: Encoded, _exchange: Exchange) -> Opening<WireFrame> {
        let bytes = match payload.request.body() {
            Body::Bytes(b) => b.clone(),
            _ => Vec::new(),
        };
        let text = String::from_utf8_lossy(&bytes).to_string();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let inputs = v["input"].as_array().cloned().unwrap_or_default();
        let n = inputs.len() as u64;
        let body = json!({"object":"list","model":"wire-fake",
            "data": inputs.iter().enumerate().map(|(k, _)| json!({"index":k,"embedding":[k,0,0]})).collect::<Vec<_>>(),
            "usage":{"prompt_tokens":3*n,"total_tokens":4*n}}).to_string();
        self.0.lock().unwrap().push(text);
        Opening::ready(
            Opened::new(stream::once(async move { Ok(WireFrame::Bytes(body.into_bytes())) }))
                .with_http(StatusCode::OK, HeaderMap::new()),
        )
    }
}
