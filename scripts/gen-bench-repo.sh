#!/usr/bin/env bash
# Generate a throwaway git repo with very large files and a large uncommitted
# diff, for profiling the diff viewer (e.g. under `samply`). Not a test fixture —
# it builds realistic, multi-thousand-line files from this repo's own Rust source
# so syntax highlighting has real tokens to chew on.
#
#   scripts/gen-bench-repo.sh [target_dir] [lines_per_file] [num_files]
#
# Then profile the viewer against it:
#   samply record -- "$PWD/target/release/mudpuppy" -C <target_dir>
# (or cd into <target_dir> and run mudpuppy there).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="${1:-/tmp/mudpuppy-bench-repo}"
lines_per_file="${2:-4000}"
num_files="${3:-4}"

# Pool of real source to concatenate into each big file.
pool=(
  "src/tui/app.rs"
  "src/tui/render.rs"
  "src/diff.rs"
  "src/anchor.rs"
  "src/lua/api.rs"
  "src/highlight.rs"
)

rm -rf "$target"
mkdir -p "$target/src"
git -C "$target" init -q
git -C "$target" config user.email bench@example.com
git -C "$target" config user.name bench

# Build one big file by repeating the pool until it reaches lines_per_file.
build_file() {
  local out="$1" want="$2" have=0 i=0
  : > "$out"
  while [ "$have" -lt "$want" ]; do
    local src="$repo_root/${pool[$((i % ${#pool[@]}))]}"
    cat "$src" >> "$out"
    have=$(wc -l < "$out")
    i=$((i + 1))
  done
}

echo "Generating $num_files file(s) of ~$lines_per_file lines in $target ..."
for n in $(seq 1 "$num_files"); do
  build_file "$target/src/big_$n.rs" "$lines_per_file"
done

# Baseline commit: the files as-is.
git -C "$target" add -A
git -C "$target" commit -qm "baseline"

# Now perturb each file throughout so the diff has hunks spanning the whole
# file (not one tidy block) — every ~25th line edited, plus scattered
# insertions and deletions.
for n in $(seq 1 "$num_files"); do
  f="$target/src/big_$n.rs"
  awk '
    { ln++ }
    ln % 25 == 0  { print "// edited line " ln " for benchmark diff"; next }   # change
    ln % 60 == 0  { print; print "let __bench_inserted = " ln ";"; next }      # insert
    ln % 90 == 0  { next }                                                     # delete
    { print }
  ' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
done

added=$(git -C "$target" diff --numstat | awk '{a+=$1} END{print a+0}')
deleted=$(git -C "$target" diff --numstat | awk '{d+=$2} END{print d+0}')
echo "Done. Uncommitted diff: +$added / -$deleted across $num_files file(s)."
echo
echo "Profile it with:"
echo "  samply record -- \"$repo_root/target/release/mudpuppy\" -C \"$target\""
