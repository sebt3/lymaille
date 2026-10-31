//! Sonde rig-core 0.43.0 : faux serveur HTTP local + client OpenAI-compatible (chat completions).
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use rig_core::completion::{CompletionRequest, Message};
use rig_core::message::{AssistantContent, Issuer, Reasoning};
use rig_core::providers::openai::wire::{LLAMACPP, OpenAIConfig};
use rig_core::streaming::{Item, StreamEvent};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, Vec<(String, String)>, String)>>>);

/// Serveur : répond `status` / `content_type` / `body` à toute requête, en journalisant (ligne, en-têtes, corps).
async fn serve(status: u16, ctype: &'static str, body: String) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Seen::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let log = log.clone();
            let body = body.clone();
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
                log.0.lock().unwrap().push((first, hdrs, rbody));
                let resp = format!("HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/v1"), seen)
}

fn cfg(url: &str, key: &str) -> OpenAIConfig {
    OpenAIConfig::with_key(&LLAMACPP, key).with_base_url(url)
}

fn unary(msg: Value, usage: Value) -> String {
    let mut v = json!({"id":"c1","object":"chat.completion","model":"m","choices":[{"index":0,"finish_reason":"stop","message":msg}]});
    if !usage.is_null() { v["usage"] = usage; }
    v.to_string()
}

fn sse(chunks: &[Value]) -> String {
    let mut s = String::new();
    for c in chunks { s.push_str(&format!("data: {c}\n\n")); }
    s.push_str("data: [DONE]\n\n");
    s
}

fn show_content(c: &[AssistantContent]) -> String {
    c.iter().map(|x| match x {
        AssistantContent::Text(t) => format!("Text({:?})", t.text),
        AssistantContent::Reasoning(s) => format!("Reasoning(issuer={}, {:?})", s.issuer(), s.open_for(&[s.issuer().clone()]).map(|r| r.display_text())),
        AssistantContent::ToolCall(t) => format!("ToolCall({:?})", t),
        other => format!("{other:?}"),
    }).collect::<Vec<_>>().join(", ")
}

#[tokio::main]
async fn main() {
    // ---- (1)+(2) : base configurable, clé optionnelle, reasoning / reasoning_content
    for field in ["reasoning", "reasoning_content"] {
        let msg = json!({"role":"assistant","content":"Bonjour", field:"je réfléchis"});
        let (url, seen) = serve(200, "application/json", unary(msg, json!({"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"completion_tokens_details":{"reasoning_tokens":3}}))).await;
        let model = cfg(&url, "").client().chat("m");
        let r = model.call(CompletionRequest::new("salut")).await.unwrap();
        println!("[1/2] champ `{field}` -> choice=[{}] usage={:?}", show_content(&r.choice), r.usage);
        let s = seen.0.lock().unwrap();
        println!("[1] requête: {} ; authorization={:?}", s[0].0, s[0].1.iter().find(|h| h.0 == "authorization"));
    }
    // les deux champs à la fois
    {
        let msg = json!({"role":"assistant","content":"x","reasoning":"A","reasoning_content":"B"});
        let (url, _) = serve(200, "application/json", unary(msg, Value::Null)).await;
        let r = cfg(&url, "").client().chat("m").call(CompletionRequest::new("salut")).await.unwrap();
        println!("[2] les deux champs présents -> {}", show_content(&r.choice));
    }
    // avec clé
    {
        let (url, seen) = serve(200, "application/json", unary(json!({"role":"assistant","content":"ok"}), Value::Null)).await;
        cfg(&url, "sk-secret").client().chat("m").call(CompletionRequest::new("salut")).await.unwrap();
        let s = seen.0.lock().unwrap();
        println!("[1] avec clé: authorization={:?}", s[0].1.iter().find(|h| h.0 == "authorization"));
    }

    // ---- (3) additional_params
    {
        let (url, seen) = serve(200, "application/json", unary(json!({"role":"assistant","content":"ok"}), Value::Null)).await;
        let mut req = CompletionRequest::new("vrai prompt");
        req.temperature = Some(0.3);
        req.additional_params = Some(json!({
            "chat_template_kwargs": {"preserve_thinking": true}, "top_k": 20, "top_p": 0.9,
            "model": "PIRATE", "messages": [{"role":"user","content":"PIRATE"}], "tools": [], "stream": true, "temperature": 0.99
        }));
        let r = cfg(&url, "").client().chat("m").call(req).await;
        println!("[3] unary avec clés hostiles -> ok={}", r.is_ok());
        let s = seen.0.lock().unwrap();
        println!("[3] corps unary: {}", s[0].2);
    }
    {
        let (url, seen) = serve(200, "text/event-stream", sse(&[json!({"id":"1","choices":[{"index":0,"delta":{"content":"ok"}}]})])).await;
        let mut req = CompletionRequest::new("vrai prompt");
        req.additional_params = Some(json!({"model":"PIRATE","messages":[],"stream":false,"stream_options":{"include_usage":false},"top_k":5}));
        let mut st = cfg(&url, "").client().chat("m").stream(req).unwrap();
        while st.next().await.is_some() {}
        let s = seen.0.lock().unwrap();
        println!("[3] corps stream: {}", s[0].2);
    }

    // ---- (4) flux
    {
        let chunks = [
            json!({"id":"1","model":"m","choices":[{"index":0,"delta":{"role":"assistant","reasoning":"pen"}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"reasoning_content":"se"}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"content":"Bon"}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"content":"jour"}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"rhai_pad","arguments":"{\"a\":"}}]}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_2","type":"function","function":{"name":"skill","arguments":"{pas json"}}]}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"id":"1","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18,"completion_tokens_details":{"reasoning_tokens":2}}}),
        ];
        let (url, seen) = serve(200, "text/event-stream", sse(&chunks)).await;
        let mut st = cfg(&url, "").client().chat("m").stream(CompletionRequest::new("salut")).unwrap();
        while let Some(item) = st.next().await {
            match item {
                Ok(Item::Event(e @ StreamEvent::Text { .. })) => println!("[4] event {:?}", e),
                Ok(Item::Event(e @ StreamEvent::Reasoning { .. })) => println!("[4] event {:?}", e),
                Ok(Item::Event(e @ StreamEvent::Arguments { .. })) => println!("[4] event {:?}", e),
                Ok(Item::Event(e)) => println!("[4] event {}", e.name()),
                Ok(Item::Unknown(u)) => println!("[4] unknown {u:?}"),
                Err(rig_core::error::ProviderError::MalformedToolInput(m)) => println!("[4] ERREUR MalformedToolInput name={} id={} raw={:?}", m.name, m.id, m.raw),
                Err(e) => println!("[4] ERREUR {e}"),
            }
        }
        // finish() consomme: refaire un flux complet pour la réponse finale
        let r = cfg(&url, "").client().chat("m").stream(CompletionRequest::new("salut")).unwrap().finish().await;
        match r {
            Ok(r) => println!("[4] finish: choice=[{}] usage={:?} finish_reason={:?}", show_content(&r.choice), r.usage, r.finish_reason()),
            Err(e) => println!("[4] finish ERREUR {e}"),
        }
        println!("[4] corps envoyé en flux: {}", seen.0.lock().unwrap()[0].2);
    }


    // ---- (4b) appels d'outil unary, arguments invalides ; flux valide avec usage final
    {
        let msg = json!({"role":"assistant","content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":"rhai_pad","arguments":"{\"a\":1}"}},
            {"id":"c2","type":"function","function":{"name":"skill","arguments":"{pas json"}}]});
        let (url, _) = serve(200, "application/json", unary(msg, Value::Null)).await;
        let r = cfg(&url, "").client().chat("m").call(CompletionRequest::new("x")).await;
        match r { Ok(r) => println!("[4b] unary ok: {}", show_content(&r.choice)), Err(e) => println!("[4b] unary ERREUR (toute la réponse perdue): variante Json={} {e}", matches!(e, rig_core::error::ProviderError::Json(_))) }
        let chunks = [
            json!({"id":"1","choices":[{"index":0,"delta":{"reasoning":"pen"}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"content":"Bon"}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"content":"jour"}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"rhai_pad","arguments":"{\"a\":1}"}}]}}]}),
            json!({"id":"1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"id":"1","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18,"completion_tokens_details":{"reasoning_tokens":2}}}),
        ];
        let (url, _) = serve(200, "text/event-stream", sse(&chunks)).await;
        let r = cfg(&url, "").client().chat("m").stream(CompletionRequest::new("x")).unwrap().finish().await;
        match r { Ok(r) => println!("[4b] stream finish: choice=[{}] usage={:?} reason={:?}", show_content(&r.choice), r.usage, r.finish_reason()), Err(e) => println!("[4b] stream ERREUR {e}") }
        // deltas puis réponse : itérer jusqu'à None, puis finish() sur le même flux
        let (url, _) = serve(200, "text/event-stream", sse(&chunks)).await;
        let mut st = cfg(&url, "").client().chat("m").stream(CompletionRequest::new("x")).unwrap();
        let mut textes = Vec::new();
        while let Some(item) = st.next().await { if let Ok(Item::Event(StreamEvent::Text { text, .. })) = item { textes.push(text); } }
        match st.finish().await { Ok(r) => println!("[4c] deltas={textes:?} puis finish: choice=[{}] usage={:?}", show_content(&r.choice), r.usage), Err(e) => println!("[4c] ERREUR {e}") }
        // flux sans usage final
        let (url, _) = serve(200, "text/event-stream", sse(&chunks[..5])).await;
        let r = cfg(&url, "").client().chat("m").stream(CompletionRequest::new("x")).unwrap().finish().await;
        match r { Ok(r) => println!("[4b] stream sans usage: usage_reported={} ", r.usage.is_reported()), Err(e) => println!("[4b] stream sans usage ERREUR {e}") }
    }

    // ---- (5) list_models
    {
        let body = json!({"object":"list","data":[
            {"id":"vllm-model","object":"model","owned_by":"vllm","max_model_len":262144},
            {"id":"sans-info","object":"model"},
            {"id":"gateway","object":"model","context_length":8192}]}).to_string();
        let (url, seen) = serve(200, "application/json", body).await;
        let l = cfg(&url, "").client().list_models().await.unwrap();
        for m in &l { println!("[5] {} context_length={:?} max_output_tokens={:?}", m.id, m.context_length, m.max_output_tokens); }
        println!("[5] requête: {}", seen.0.lock().unwrap()[0].0);
    }

    // ---- (6) rejeu du raisonnement
    {
        let issuer = Issuer::from("llamacpp");
        let hist = |iss: Issuer| vec![
            Message::user("q1"),
            Message::Assistant { id: None, content: vec![
                AssistantContent::Reasoning(Reasoning::new("ma pensée").sealed(iss)),
                AssistantContent::text("r1")] },
            Message::user("q2"),
        ];
        for (nom, iss) in [("même émetteur", issuer.clone()), ("autre émetteur", Issuer::from("autre-modele"))] {
            let (url, seen) = serve(200, "application/json", unary(json!({"role":"assistant","content":"ok"}), Value::Null)).await;
            let h = hist(iss);
            let mut it = h.into_iter();
            let _ = &mut it;
            let req = CompletionRequest::new(Message::user("q2")).messages(vec![]).clone();
            let _ = req;
            let mut hh = hist(Issuer::from(if nom == "même émetteur" { "llamacpp" } else { "autre-modele" }));
            let prompt = hh.pop().unwrap();
            let req = CompletionRequest::new(prompt).messages(hh);
            let r = cfg(&url, "").client().chat("m").call(req).await;
            println!("[6] {nom}: ok={:?}", r.as_ref().map(|_| ()).map_err(|e| e.to_string()));
            println!("[6] {nom}: corps messages = {}", serde_json::from_str::<Value>(&seen.0.lock().unwrap()[0].2).map(|v| v["messages"].to_string()).unwrap_or_default());
        }
    }

    // ---- (7) erreurs
    for (status, body) in [
        (429, json!({"error":{"message":"slow down"}}).to_string()),
        (401, json!({"error":{"message":"bad key"}}).to_string()),
        (403, "nope".to_owned()),
        (400, json!({"error":{"message":"This model's maximum context length is 8192 tokens. However, you requested 9000 tokens","type":"BadRequestError","code":"context_length_exceeded"}}).to_string()),
        (500, "boom".to_owned()),
        (200, json!({"error":{"message":"enveloppe 2xx"}}).to_string()),
    ] {
        let (url, _) = serve(status, "application/json", body).await;
        let r = cfg(&url, "sk-secret").client().chat("m").call(CompletionRequest::new("x")).await;
        match r {
            Ok(_) => println!("[7] {status}: Ok ?!"),
            Err(e) => println!("[7] {status}: variante={:?} status={:?} retryable={} body={:?}\n     Display={:?}", std::mem::discriminant(&e) == std::mem::discriminant(&rig_core::error::ProviderError::Truncated), e.provider_response_status(), e.is_retryable(), e.provider_response_body(), e.to_string()),
        }
    }
    // connexion refusée
    {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        let r = cfg(&format!("http://{a}/v1"), "sk-secret").client().chat("m").call(CompletionRequest::new("x")).await;
        if let Err(e) = r { println!("[7] connexion refusée: status={:?} retryable={} Display={:?} Debug-contains-addr={}", e.provider_response_status(), e.is_retryable(), e.to_string(), format!("{e:?}").contains(&a.to_string())); }
    }
    // connexion coupée sans réponse
    {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(async move { loop { let (s, _) = l.accept().await.unwrap(); drop(s); } });
        let r = cfg(&format!("http://{a}/v1"), "sk-secret").client().chat("m").call(CompletionRequest::new("x")).await;
        if let Err(e) = r { println!("[7] connexion coupée: retryable={} Display={:?}", e.is_retryable(), e.to_string()); }
    }
    // JSON de réponse illisible
    {
        let (url, _) = serve(200, "application/json", "pas du json".to_owned()).await;
        let r = cfg(&url, "").client().chat("m").call(CompletionRequest::new("x")).await;
        if let Err(e) = r { println!("[7] corps illisible: kind={:?} retryable={} Display={:?}", e.kind(), e.is_retryable(), e.to_string()); }
    }
    println!("{:?}", cfg("http://secret-host/v1", "sk-secret"));
}
