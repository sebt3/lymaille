# Essai de bout en bout de l'adaptateur `rig` (lot R2 de `src/agents/rig.sdd`)

Date : 2026-11-01. Serveur : vLLM du DGX du développeur principal, sans authentification, base `http://<adresse-du-dgx>:8000/v1`
(adresse passée par `LYMAILLE_E2E_BASE`, modèle par `LYMAILLE_E2E_MODEL`). Modèle : `qwen3.8-flash-next`. Programme hors
workspace (`lymaille = { path = "../.." }`, feature `rig` par défaut), `rustc 1.99.0`, `rustls` (feature `ring`) installé
par l'hôte avant tout client. Lancer : `CARGO_TARGET_DIR=~/.cache/cargo-target/rig-e2e LYMAILLE_E2E_BASE=... LYMAILLE_E2E_MODEL=... cargo run`
(`LYMAILLE_E2E_MOT=1` ajoute le second scénario de rejeu ; `LYMAILLE_E2E_ONLY_MOT=1` ne joue que lui).
Légende : **[exécuté]** = mesuré contre le serveur ; **[code]** = lu dans le code. Une dizaine de requêtes courtes en tout, en
séquence, plus 4 pour le second scénario de rejeu et 1 `curl` de contrôle du JSON brut (même hôte). Chaque mesure
est un seul essai (n = 1) : indicatif, pas statistique.

## API publique : suffisante pour construire une requête depuis l'extérieur

`ModelProfile::new(..).with_context_size(..).with_max_response(..).with_sampling(SamplingParams::default().with_extra(..)).with_replay(..).resolve(None)`
donne un `ResolvedProfile` ; `InferenceRequest::new(profil, système, messages, outils, raisonnement)` ; le rejeu se
calcule avec `replayed_reasoning` (exporté). Tout compile et tourne sans toucher à la crate **[exécuté]**. Défauts
d'API relevés plus bas (finish reason absent, etc.).

## Résultats

1. **`detect_models`** [exécuté] : un modèle annoncé, `qwen3.8-flash-next`, `context_size = Some(262144)` (attendu),
   `max_output = None`, `tools = None` (vLLM n'annonce ni l'un ni l'autre). Lève le TBD de la taille de contexte.
2. **`complete` simple** [exécuté] : « La capitale de la France est Paris. », usage `(prompt 75, completion 231)`, raisonnement
   décodé (869 car.), émetteur `qwen3.8-flash-next`, absent du texte.
3. **`stream`** [exécuté] : 4 fragments `"\n\nun"`, `", deux,"`, `" trois, quatre"`, `", cinq"` dans l'ordre ; la concaténation est
   identique au texte final ; usage final `(79, 43)` ; raisonnement décodé (119 car.) et non émis comme fragment.
4. **Raisonnement** [exécuté] : le serveur le renvoie dans le champ **`reasoning`** (contrôle `curl` : clés du message
   `content`, `reasoning`, `refusal`, …, pas de `reasoning_content` ; `completion_tokens_details.reasoning_tokens` = 41).
   L'adaptateur le décode, le rend dans `Message::Assistant.reasoning` et jamais dans le texte (vérifié sur 8 réponses).
   Le champ d'origine n'est pas visible côté adaptateur (c'est voulu). `reasoning_content` : non exercé ici (passerelle).
5. **Rejeu** [exécuté] : `chat_template_kwargs = {"preserve_thinking": true}` passé par la table libre de `SamplingParams`.
   - Scénario « nombre » : avec rejeu (`AllTurns`) le serveur accepte (prompt 389 jetons contre 123 sans rejeu : le raisonnement
     du tour 1 est bien parti), réponse `42` ; sans rejeu, `42` aussi. **Non discriminant** : 42 est la réponse par défaut du
     modèle, et le raisonnement du tour 1 avait justement choisi 42. Ne rien conclure de ce couple.
   - Scénario « mot rare de 8 lettres » (n = 1) : tour 1 « A » ; avec rejeu : `abstruse` (le mot envisagé dans le raisonnement
     du tour 1, cohérent avec la lettre annoncée), 144 jetons ; sans rejeu : texte vide, le modèle se perd dans son
     raisonnement jusqu'à la limite de 2048 jetons. Le rejeu est donc accepté, ne dégrade pas, et aide ici.
   - Non mesuré : l'effet de `preserve_thinking` seul sans rejeu côté adaptateur, et le backend Qwen3.6 de la passerelle (TBD
     de `context-window.md` : la passerelle n'était pas dans le périmètre de cet essai).
6. **Appel d'outil** [exécuté] : `rhai_pad_definition()` + consigne : 1 appel, `nom = "rhai_pad"`, `arguments = {"script":"6*7"}`
   (JSON valide), id `chatcmpl-tool-…`, texte vide, usage `(553, 61)`.
7. **`max_tokens`** [exécuté] : profil à 24 : `completion = 24` jetons (usage), texte vide, le raisonnement tronqué en pleine phrase.
   Respecté exactement. Le *finish reason* n'est pas visible (voir défauts). Corollaire : un modèle à raisonnement peut
   consommer tout `max_tokens` en raisonnement et rendre un texte vide (vu aussi à 2048 dans le scénario « mot »).
8. **Erreur** [exécuté] : modèle inexistant : `Permanent`, message « the server answered with HTTP status 404 » ; il ne contient pas
   l'adresse. Aussi : `Debug` de `RigConfig` = `RigConfig { .. }`.

## Écarts avec `rig.sdd` et défauts constatés

- Aucun écart de comportement avec la spec sur les 8 points mesurés.
- **Défaut d'API** : `InferenceResponse` n'expose pas le *finish reason* (`length` / `stop` / `tool_calls`). Une réponse tronquée
  par `max_tokens` n'est détectable que par `usage.completion == max_tokens` (heuristique). La boucle de tour ne peut pas
  distinguer une réponse tronquée d'une réponse finie, alors que le budget de contexte repose sur `max_tokens`. À décider pour 0.1.
- **Comportement** : le texte vLLM commence par `"\n\n"` (gabarit Qwen : le contenu est `"\n\nOui."`), rendu tel quel par l'adaptateur,
  y compris dans les fragments de flux. À décider : retirer l'espace de tête côté adaptateur ou côté hôte (la spec dit « conservé tel
  que reçu » pour l'historique, donc sans doute l'hôte).
- `ReportedUsage` ne porte pas les jetons de raisonnement, que le serveur rapporte (`reasoning_tokens`) : `TokenCounters` en a un
  troisième champ, d'où une perte possible entre les deux (non vérifié dans la boucle de tour).
- vLLM n'annonce ni `max_output_tokens` ni la prise en charge des outils : la sortie max doit venir du profil (sinon repli
  `contexte / 8` = 32768 ici, au-dessus des 25000 de la config opencode).

## Reste à décider

- Exposer le *finish reason* dans `InferenceResponse` (ou un indicateur « tronquée »).
- Où retirer le `"\n\n"` de tête.
- Rejeu : mesurer le backend Qwen3.6 de la passerelle (champ `reasoning_content`), HTTPS, un serveur avec authentification.
- Tâche #3 de `rig.sdd` : cocher une fois ces points tranchés ; le TBD de `context-window.md` reste ouvert pour la passerelle.

Nettoyage : `cargo clean` du répertoire `~/.cache/cargo-target/rig-e2e` fait en fin d'essai ; aucun processus en fond.

## Passerelle `smart` (Qwen3.6-35B-A3B), mesuré le 2026-11-01

Même programme, `LYMAILLE_E2E_BASE=http://<adresse-de-la-passerelle>:11434/v1`, `LYMAILLE_E2E_MODEL=Qwen3.6-35B-A3B`, clé dans `LYMAILLE_E2E_KEY` (lue de la configuration d'opencode, jamais écrite ni affichée : 0 occurrence dans les journaux). Deux exécutions, aucun blocage du proxy, code de sortie 0.

Exécuté :
- **Détection** : la passerelle annonce `context_length: 8192` pour presque tous ses modèles, `Qwen3.6-35B-A3B` compris (réel : 250 000) ; confirme `kydah/kydah-ai-proxy#113`. La surcharge du profil est donc nécessaire, pas seulement prévue.
- **`complete`, `stream`, appel d'outil** : conformes (texte, usage, raisonnement décodé et absent du texte, 9 fragments dont la concaténation égale le texte final, `rhai_pad` décodé avec `{"script":"6*7"}`). `max_tokens` 24 respecté (usage 24).
- **Rejeu** : le tour 2 AVEC rejeu (`AllTurns` + `chat_template_kwargs {"preserve_thinking": true}`) est accepté par le backend et le prompt grossit (601 jetons contre 124 sans rejeu), ce qui montre que le raisonnement du tour 1 arrive dans le prompt côté serveur. Les deux variantes répondent `47` : le scénario « nombre » ne discrimine pas (réponse par défaut du modèle). Le scénario « mot de 8 lettres » est inexploitable ici : avec `max_tokens` 2048 le modèle consomme tout le budget en raisonnement (texte vide aux deux tours, usage 2048), donc aucun message d'assistant avec texte à rejouer.
- **Modèle inexistant** : la passerelle répond **503**, classé `Transient` par l'adaptateur (le serveur ne distingue pas « modèle inconnu » d'une panne) ; la boucle de tour réessaierait avec backoff sur une erreur permanente. Le message ne contient pas l'adresse.

Conclusion honnête : le backend accepte `reasoning_content` et `chat_template_kwargs` et le raisonnement rejoué arrive dans le prompt. Que le rejeu améliore la réponse sur Qwen3.6 n'est pas démontré (le DGX l'a démontré). Pour le démontrer : un second tour dont la réponse dépend du raisonnement du premier avec un `max_tokens` assez grand (au moins 8000) ou sans mode de réflexion au tour 1.
