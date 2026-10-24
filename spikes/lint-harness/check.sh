#!/usr/bin/env bash
# Self-test of the harness: the clean base must produce no diagnostic, and every expected lint must fire on the
# canaries. A lint that is renamed, removed or misconfigured makes this script fail instead of silently switching off.
set -euo pipefail
cd "$(dirname "$0")"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target/lymaille-lints}"

# clippy exits non-zero when a denied lint fires, which is the whole point here: only the diagnostics matter.
codes() { { cargo clippy --all-targets "$@" --message-format=json 2>/dev/null || true; } |
  jq -r 'select(.reason=="compiler-message") | .message.code.code // empty' | sort -u; }

echo "== 1) clean base: no diagnostic expected"
base=$(codes)
if [ -n "$base" ]; then echo "FAILED, diagnostics on the clean base:"; echo "$base"; exit 1; fi
echo "ok"

echo "== 2) canaries: every expected lint must fire"
got=$(codes --features canaries)
missing=0
while read -r want; do
  [ -z "$want" ] && continue
  if ! grep -qxF "$want" <<<"$got"; then echo "MISSING $want"; missing=1; fi
done < expected_codes.txt
if grep -qE '^(unknown_lints|renamed_and_removed_lints)$' <<<"$got"; then
  echo "FAILED: a lint name is unknown or renamed"; missing=1
fi
[ "$missing" = 0 ] && echo "ok: $(grep -c . expected_codes.txt) lints verified"
exit "$missing"
