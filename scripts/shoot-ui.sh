#!/usr/bin/env bash
# Photograph a component in all of its states.
#
# Much of this interface was written without anyone looking at it, and
# the bugs that get found are layout ones no assertion catches: a
# column shifted left, a button wrapping, text running under an icon.
# This mounts a component via its `*.shot.test.ts`, renders the markup
# against the real built stylesheet at a realistic width, and takes a
# picture in both themes.
#
#   ./scripts/shoot-ui.sh sync/BillingPanel [width]
#
# Needs `npm run build` to have produced dist/assets/*.css at least
# once, and google-chrome on PATH.
set -euo pipefail

TARGET="${1:?usage: shoot-ui.sh <feature/Component> [panel-width]}"
WIDTH="${2:-320}"
NAME="$(basename "$TARGET")"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${RELAY_SHOTS_OUT:-/tmp/relay-shots}/$NAME"

SPEC="apps/desktop/src/features/${TARGET}.shot.test.ts"
if [ ! -f "$ROOT/$SPEC" ]; then
  echo "no camera for $TARGET — write $SPEC first" >&2
  exit 1
fi

rm -rf "$OUT"
mkdir -p "$OUT"

cd "$ROOT"
RELAY_SHOTS=1 RELAY_SHOTS_DIR="$OUT" \
  npx vitest run --root apps/desktop "src/features/${TARGET}.shot.test.ts" >/dev/null

python3 - "$OUT" "$NAME" "$WIDTH" <<'PY'
import html, json, pathlib, sys

out, name, width = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
css = sorted(pathlib.Path("apps/desktop/dist/assets").glob("*.css"))[0].read_text()
states = json.loads((out / "states.json").read_text())

panels = "".join(
    f'<section><h2>{html.escape(s)}</h2>'
    f'<div class="frame">{(out / f"{s}.html").read_text()}</div></section>'
    for s in states
)

for theme in ("dark", "light"):
    (out / f"page-{theme}.html").write_text(f"""<!doctype html>
<html lang="en" class="{theme}"><head><meta charset="utf-8"><style>{css}</style><style>
body{{margin:0;padding:24px;font-family:system-ui,sans-serif}}
h2{{font:600 11px ui-monospace,monospace;letter-spacing:.04em;opacity:.55;margin:0 0 6px}}
section{{margin-bottom:22px}}
.frame{{width:{width}px;border:1px solid rgba(128,128,128,.35);border-radius:6px;padding:12px}}
.wrap{{display:flex;flex-wrap:wrap;gap:28px;align-items:flex-start}}
</style></head><body><div class="wrap">{panels}</div></body></html>""")

print(f"{name}: {len(states)} states")
PY

for theme in dark light; do
  google-chrome --headless --disable-gpu --no-sandbox --hide-scrollbars \
    --window-size=1280,1600 \
    --screenshot="$OUT/$NAME-$theme.png" \
    "file://$OUT/page-$theme.html" >/dev/null 2>&1
done

echo "$OUT/$NAME-dark.png"
echo "$OUT/$NAME-light.png"
