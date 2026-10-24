#!/usr/bin/env bash
# Sonde un serveur d'inférence compatible OpenAI (vLLM, llama.cpp) : champ de raisonnement renvoyé, et effet du rejeu du
# raisonnement sur la taille du prompt. Requêtes minuscules (8 tokens de sortie).
# Usage : BASE_URL=http://hote:8000/v1 MODEL=nom-du-modele ./probe.sh   (API_KEY=... si le serveur en exige une,
#         TIMEOUT=secondes par requête, 120 par défaut ; un modèle froid peut demander plus)
set -euo pipefail
: "${BASE_URL:?}" "${MODEL:?}"
U="$BASE_URL/chat/completions"
AUTH=(); [ -n "${API_KEY:-}" ] && AUTH=(-H "Authorization: Bearer $API_KEY")
post() { curl -s -m "${TIMEOUT:-120}" "$U" -H 'Content-Type: application/json' "${AUTH[@]}" -d "$1" || echo '{"error":{"message":"echec curl (delai ou connexion)"}}'; }

echo "== 1) forme de la réponse"
post "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Calcule 17*23. Réponds seulement par le nombre.\"}],\"max_tokens\":600}" |
  jq -c 'if .error then {erreur: (.error|tostring|.[0:100])} else {champs_du_message: (.choices[0].message | keys), reasoning_tokens: .usage.completion_tokens_details.reasoning_tokens} end'

R=$(python3 -c "print('Je réfléchis étape par étape au calcul demandé, en décomposant 17*23 en 17*20 plus 17*3. ' * 30)")
probe() { # $1 libellé, $2 champ de raisonnement (vide = aucun), $3 chat_template_kwargs JSON (vide = aucun)
  local field="" kw=""
  [ -n "$2" ] && field=",\"$2\":$(jq -Rn --arg r "$R" '$r')"
  [ -n "$3" ] && kw=",\"chat_template_kwargs\":$3"
  local n; n=$(post "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Calcule 17*23.\"},{\"role\":\"assistant\",\"content\":\"391\"$field},{\"role\":\"user\",\"content\":\"Merci. Et 19*21 ?\"}],\"max_tokens\":8$kw}" |
    jq -c 'if .error then {erreur: .error.message[0:100]} else .usage.prompt_tokens end')
  printf '%-58s prompt_tokens=%s\n' "$1" "$n"
}
echo "== 2) effet du raisonnement rejoué (environ 600 tokens de raisonnement)"
probe "A. aucun raisonnement dans l'historique" "" ""
probe "B. reasoning_content, paramètres par défaut" "reasoning_content" ""
probe "C. reasoning_content + preserve_thinking=true" "reasoning_content" '{"preserve_thinking":true}'
probe "D. reasoning + preserve_thinking=true" "reasoning" '{"preserve_thinking":true}'
probe "E. reasoning_content + preserve_thinking=false" "reasoning_content" '{"preserve_thinking":false}'
probe "F. reasoning + preserve_thinking=false" "reasoning" '{"preserve_thinking":false}'
probe "G. reasoning + enable_thinking=false" "reasoning" '{"enable_thinking":false}'
