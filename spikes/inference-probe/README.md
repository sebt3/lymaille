# Sonde du serveur d'inférence

Mesure, sur un serveur compatible OpenAI, ce dont `dsd/agent-sessions/context-window.md` (FR-9) dépend : le champ de
raisonnement renvoyé, les noms acceptés en entrée, et l'effet de `preserve_thinking` sur le prompt. Aucun secret n'est
versionné : `BASE_URL`, `MODEL` et `API_KEY` se passent par l'environnement.

## Résultats du 2026-10-24

Serveur vLLM, modèle `qwen3.8-flash-next`.

| Mesure | Résultat |
| --- | --- |
| Champ de raisonnement dans la réponse | `reasoning` (pas `reasoning_content`) ; `reasoning_tokens` fourni dans l'usage |
| Champ accepté en entrée | `reasoning` et `reasoning_content`, avec le même effet |
| Aucun raisonnement dans l'historique (A) | 89 tokens de prompt |
| Raisonnement rejoué, paramètres par défaut (B) | 1 140 tokens : ce modèle **conserve par défaut** |
| `preserve_thinking=true` (C, D) | 1 140 tokens |
| `preserve_thinking=false` (E, F) | 85 tokens : le paramètre est honoré par `chat_template_kwargs` |
| `enable_thinking=false` (G) | 1 100 tokens : sans effet sur l'historique |

Qwen3.6 via le provider `smart` n'est pas mesuré (clé d'API requise). Des sources communautaires donnent
`preserve_thinking` désactivé par défaut dans son gabarit, ce qui diffère de ce modèle.
