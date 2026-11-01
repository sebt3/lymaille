//! Essai de bout en bout de l'adaptateur `rig` de lymaille contre un vrai serveur (lot R2). Lit LYMAILLE_E2E_BASE et
//! LYMAILLE_E2E_MODEL. Quelques requêtes courtes, séquentielles.
use lymaille::{
    ErrorClass, InferenceProvider, InferenceRequest, InferenceResponse, Message, ModelProfile, Reasoning, ReplayMode,
    ResolvedProfile, RigConfig, RigProvider, SamplingParams, detect_models, replayed_reasoning, rhai_pad_definition,
};
use serde_json::json;

fn profile(model: &str, ctx: u64, max: u64, extra: bool, replay: ReplayMode) -> ResolvedProfile {
    let mut s = SamplingParams::default().with_temperature(0.6);
    if extra {
        s = s.with_extra("chat_template_kwargs".into(), json!({"preserve_thinking": true}));
    }
    ModelProfile::new("e2e".into(), "dgx".into(), model.into())
        .with_context_size(ctx)
        .with_max_response(max)
        .with_sampling(s)
        .with_replay(replay)
        .resolve(None)
        .expect("resolve")
}

fn request(p: ResolvedProfile, msgs: Vec<Message>, tools: Vec<serde_json::Value>) -> InferenceRequest {
    let rs: Vec<Option<Reasoning>> = replayed_reasoning(&msgs, p.replay()).into_iter().map(|r| r.cloned()).collect();
    InferenceRequest::new(p, "Tu es un assistant concis.".into(), msgs, tools, rs)
}

fn show(r: &InferenceResponse) -> (String, Option<Reasoning>, usize) {
    match r.message() {
        Message::Assistant { content, reasoning, tool_calls } => (content.clone(), reasoning.clone(), tool_calls.len()),
        _ => (String::new(), None, 0),
    }
}

fn cut(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{t}...") } else { t }
}

fn report(tag: &str, r: &InferenceResponse) -> (String, Option<Reasoning>) {
    let (text, reasoning, nt) = show(r);
    println!("[{tag}] texte ({} car.): {:?}", text.chars().count(), cut(&text, 300));
    match &reasoning {
        Some(x) => println!("[{tag}] raisonnement ({} car., émetteur {:?}): {:?}", x.text().chars().count(), x.emitter(), cut(x.text(), 160)),
        None => println!("[{tag}] raisonnement: aucun"),
    }
    println!("[{tag}] appels d'outil: {nt}; usage: {:?}", r.usage().map(|u| (u.prompt(), u.completion())));
    (text, reasoning)
}

async fn replay(prov: &RigProvider, model: &str, ctx: u64, mot: bool) {
    println!("\n== (4)/(5) raisonnement et rejeu");
    let (q1, q2) = if mot {
        ("Choisis mentalement un mot français rare de 8 lettres, sans me le dire. Dis-moi seulement sa première lettre.", "Quel mot avais-tu choisi ? Réponds par le mot seul.")
    } else {
        ("Pense à un nombre entre 1 et 100 sans me le dire, puis dis-moi seulement s'il est pair ou impair.", "Quel nombre avais-tu choisi ? Réponds par le nombre seul.")
    };

    let p1 = profile(model, ctx, 2048, true, ReplayMode::AllTurns);
    let first = prov.complete(request(p1, vec![Message::user(q1)], vec![])).await;
    match first {
        Err(e) => println!("ERREUR tour 1 {:?}: {}", e.class(), e.message()),
        Ok(r1) => {
            let (t1, r1r) = report("tour1", &r1);
            let a1 = r1.message().clone();
            let hist = vec![Message::user(q1), a1, Message::user(q2)];
            println!("raisonnement du tour 1 dans le texte ? {}", r1r.as_ref().is_some_and(|x| !x.text().is_empty() && t1.contains(&cut(x.text(), 40))));
            for (tag, mode, extra) in [
                ("tour2 AVEC rejeu (AllTurns + preserve_thinking)", ReplayMode::AllTurns, true),
                ("tour2 SANS rejeu (None)", ReplayMode::None, true),
            ] {
                let p = profile(model, ctx, 2048, extra, mode);
                match prov.complete(request(p, hist.clone(), vec![])).await {
                    Ok(r) => { let _ = report(tag, &r); }
                    Err(e) => println!("[{tag}] ERREUR {:?}: {}", e.class(), e.message()),
                }
            }
            // choix du tour 1 : le raisonnement le dit-il ?
            if let Some(x) = r1r { println!("[tour1] raisonnement complet (pour comparer au nombre rendu au tour 2): {:?}", cut(x.text(), 700)); }
        }
    }

}

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider().install_default().expect("rustls");
    let base = std::env::var("LYMAILLE_E2E_BASE").expect("LYMAILLE_E2E_BASE");
    let model = std::env::var("LYMAILLE_E2E_MODEL").expect("LYMAILLE_E2E_MODEL");
    // The key, when the server needs one, only comes from the environment and is never printed.
    let key = std::env::var("LYMAILLE_E2E_KEY").ok().filter(|k| !k.is_empty());
    let cfg = RigConfig::new(base.clone(), key);
    println!("Debug RigConfig: {cfg:?}");
    let prov = RigProvider::new(&cfg);
    let ctx = 262_144;
    if std::env::var("LYMAILLE_E2E_ONLY_MOT").is_ok() {
        replay(&prov, &model, ctx, true).await;
        return;
    }

    println!("\n== (1) detect_models");
    let mut detected_ctx = None;
    match detect_models(&cfg).await {
        Ok(v) => {
            for (id, d) in &v {
                println!("modèle {id}: contexte {:?}, sortie max {:?}, tools {:?}", d.context_size(), d.max_output(), d.tools());
                if *id == model { detected_ctx = d.context_size(); }
            }
            println!("=> {model} contexte détecté: {detected_ctx:?}");
        }
        Err(e) => println!("ERREUR {:?}: {}", e.class(), e.message()),
    }

    println!("\n== (2) complete simple");
    let p = profile(&model, ctx, 1024, false, ReplayMode::None);
    match prov.complete(request(p, vec![Message::user("Quelle est la capitale de la France ? Réponds en une phrase.")], vec![])).await {
        Ok(r) => { let _ = report("complete", &r); }
        Err(e) => println!("ERREUR {:?}: {}", e.class(), e.message()),
    }

    println!("\n== (3) stream");
    let p = profile(&model, ctx, 1024, false, ReplayMode::None);
    let mut frags: Vec<String> = Vec::new();
    match prov.stream(request(p, vec![Message::user("Compte de 1 à 5 en lettres, séparés par des virgules.")], vec![]), |f| frags.push(f.to_string())).await {
        Ok(r) => {
            let (text, _) = report("stream", &r);
            let concat: String = frags.concat();
            println!("[stream] {} fragments; premiers: {:?}", frags.len(), frags.iter().take(5).collect::<Vec<_>>());
            println!("[stream] concaténation == texte final: {}", concat == text);
            if concat != text { println!("[stream] concat={:?}", cut(&concat, 200)); }
        }
        Err(e) => println!("ERREUR {:?}: {}", e.class(), e.message()),
    }

    replay(&prov, &model, ctx, false).await;
    if std::env::var("LYMAILLE_E2E_MOT").is_ok() { replay(&prov, &model, ctx, true).await; }

    println!("\n== (6) appel d'outil");
    let p = profile(&model, ctx, 1024, false, ReplayMode::None);
    let msg = Message::user("Utilise l'outil rhai_pad pour calculer 6*7 avec le script `6*7`. Tu dois appeler l'outil.");
    match prov.complete(request(p, vec![msg], vec![rhai_pad_definition()])).await {
        Ok(r) => {
            let _ = report("outil", &r);
            if let Message::Assistant { tool_calls, .. } = r.message() {
                for c in tool_calls { println!("[outil] id={:?} nom={:?} arguments={}", c.id(), c.name(), c.arguments()); }
            }
        }
        Err(e) => println!("ERREUR {:?}: {}", e.class(), e.message()),
    }

    println!("\n== (7) max_tokens très bas (24)");
    let p = profile(&model, ctx, 24, false, ReplayMode::None);
    match prov.complete(request(p, vec![Message::user("Écris un long poème de vingt vers sur la mer.")], vec![])).await {
        Ok(r) => { let _ = report("max24", &r); }
        Err(e) => println!("ERREUR {:?}: {}", e.class(), e.message()),
    }

    println!("\n== (8) modèle inexistant");
    let p = profile("modele-qui-n-existe-pas", ctx, 64, false, ReplayMode::None);
    match prov.complete(request(p, vec![Message::user("bonjour")], vec![])).await {
        Ok(_) => println!("inattendu: succès"),
        Err(e) => {
            let host = base.split("//").nth(1).unwrap_or("").split('/').next().unwrap_or("");
            println!("classe {:?} (Permanent attendu: {}); message: {:?}", e.class(), e.class() == ErrorClass::Permanent, e.message());
            println!("le message contient l'adresse ({host}) ? {}", e.message().contains(host));
        }
    }
}
