#!/usr/bin/env bash
# In-container half of the exact-match pixel oracle (../README.md). Runs inside
# the pinned amd64 image built from ../Dockerfile, where `resvg` + ImageMagick +
# the pinned font live. Driven by ../../scripts/test-snapshots.sh.
#
#   run.sh            compare renders against committed baselines (the CI gate)
#   run.sh --update   re-bless: overwrite baselines with this run's renders
#
# Exit 0 = every scenario matched (or was re-blessed); non-zero = a mismatch or
# a missing baseline. Either way it writes a self-contained review bundle to
# e2e/review/ (expected | actual | diff + index.html) for a human to eyeball.
set -euo pipefail

UPDATE=0
[[ "${1:-}" == "--update" ]] && UPDATE=1

REPO=/work
BASELINES="$REPO/e2e/baselines"
REVIEW="$REPO/e2e/review"
SVGS=/tmp/svgs

# Render knobs — part of the pinned contract. Geometry lives in the SVG itself
# (tests/common); here we only pin the rasterizer + the exact font file. The
# font is pinned so resvg can't fall back to a different face.
FONT_FILE="${MUDPUPPY_FONT_FILE:-/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf}"

echo "==> Emitting scenario SVGs (builds the binary, drives it via PTY)"
rm -rf "$SVGS" && mkdir -p "$SVGS"
MUDPUPPY_SVG_DIR="$SVGS" cargo test --test image_diff -- --nocapture

rm -rf "$REVIEW"
mkdir -p "$BASELINES" "$REVIEW/expected" "$REVIEW/actual" "$REVIEW/diff"
# Manifest the host-side bundle script reads to know what changed (and so what
# bytes to publish under baseline/ for the approve flow). One row per scenario:
#   <name>\t<status>
STATUS="$REVIEW/status.tsv"
: > "$STATUS"

render() { # <in.svg> <out.png>
  # Lossless 24-bit raster; --skip-system-fonts + the pinned file make glyph
  # rasterization deterministic and identical to the committed baselines.
  resvg --skip-system-fonts --use-font-file "$FONT_FILE" \
        --font-family "DejaVu Sans Mono" "$1" "$2" >/dev/null
}

dims() { identify -format '%wx%h' "$1" 2>/dev/null || echo "none"; }

fail=0
declare -a ROWS_HTML=()

for svg in "$SVGS"/*.svg; do
  name="$(basename "$svg" .svg)"
  actual="$REVIEW/actual/$name.png"
  baseline="$BASELINES/$name.png"
  render "$svg" "$actual"

  if [[ "$UPDATE" == "1" ]]; then
    cp "$actual" "$baseline"
    echo "  blessed  $name ($(dims "$actual"))"
    status="blessed"; ae="—"
    cp "$actual" "$REVIEW/expected/$name.png"
    cp "$actual" "$REVIEW/diff/$name.png"
  elif [[ ! -f "$baseline" ]]; then
    echo "  NEW      $name — no committed baseline (needs blessing)"
    status="new"; ae="n/a"; fail=1
    cp "$actual" "$REVIEW/diff/$name.png"
  else
    cp "$baseline" "$REVIEW/expected/$name.png"
    if [[ "$(dims "$baseline")" != "$(dims "$actual")" ]]; then
      # Dimension change: pad both to a common canvas so the diff is viewable.
      echo "  DIMS     $name — $(dims "$baseline") -> $(dims "$actual")"
      status="size-changed"; ae="size"; fail=1
      compare "$baseline" "$actual" "$REVIEW/diff/$name.png" 2>/dev/null || true
    else
      ae="$(compare -metric AE "$baseline" "$actual" "$REVIEW/diff/$name.png" 2>&1 || true)"
      ae="${ae%% *}"
      if [[ "$ae" == "0" ]]; then
        echo "  ok       $name (AE=0)"
        status="match"
      else
        echo "  CHANGED  $name (AE=$ae differing pixels)"
        status="changed"; fail=1
      fi
    fi
  fi

  printf '%s\t%s\n' "$name" "$status" >> "$STATUS"
  ROWS_HTML+=("<tr><th>$name<br><small>$status${ae:+ · AE=$ae}</small></th>\
<td><img src=\"expected/$name.png\"></td>\
<td><img src=\"actual/$name.png\"></td>\
<td><img src=\"diff/$name.png\"></td></tr>")
done

cat > "$REVIEW/index.html" <<HTML
<!doctype html><meta charset="utf-8"><title>mudpuppy snapshot review</title>
<style>
 body{background:#1e2128;color:#ddd;font:14px system-ui,sans-serif;margin:24px}
 h1{font-size:18px} table{border-collapse:collapse} img{display:block;border:1px solid #333;max-width:520px}
 th,td{padding:8px;vertical-align:top;text-align:left;border-bottom:1px solid #2c2f38}
 thead th{position:sticky;top:0;background:#1e2128} small{color:#8b93a7;font-weight:400}
</style>
<h1>mudpuppy snapshot review</h1>
<p>Renderer <code>resvg</code> (truecolor SVG→PNG) · DejaVu Sans Mono · zero pixel tolerance.
If the <b>actual</b> column is the intended look, re-bless with
<code>./scripts/test-snapshots.sh --update</code> and commit <code>e2e/baselines/</code>.</p>
<table><thead><tr><th>scenario</th><th>expected (committed)</th><th>actual (this run)</th><th>diff</th></tr></thead>
<tbody>
$(printf '%s\n' "${ROWS_HTML[@]}")
</tbody></table>
HTML

echo "==> Review bundle: e2e/review/index.html"
if [[ "$fail" == "1" ]]; then
  echo "==> MISMATCH — see the review bundle. Re-bless with --update if intended."
  exit 1
fi
echo "==> All snapshots matched."
