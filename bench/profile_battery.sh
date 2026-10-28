#!/usr/bin/env bash
# Profile battery: run every candidate model through the three micro-probes, one model at a
# time, for the per-model profiles (bead boubou-65d.5). Unattended: wrap in `caffeinate -is`
# on a laptop so it does not sleep. Results land in bench/results/profiles/<model_tag>/ with
# one log per model; a final summary block is printed per model as it finishes.
#
#   caffeinate -is bash bench/profile_battery.sh                 # default model list
#   caffeinate -is bash bench/profile_battery.sh qwen3.5:9b      # one model
set -u
cd "$(dirname "$0")/.."
MODELS="${*:-qwen3.5:9b gemma4:12b gemma4:e4b gemma4:e2b}"
OUT="$PWD/bench/results/profiles"
mkdir -p "$OUT"
echo "### battery start $(date '+%Y-%m-%d %H:%M:%S') models: $MODELS"
for m in $MODELS; do
  tag=$(printf '%s' "$m" | tr ':/' '__')
  echo "### $m start $(date +%H:%M:%S)"
  python3 bench/probes.py --model "$m" --n 10 --results "$OUT/$tag" > "$OUT/$tag.log" 2>&1
  rc=$?
  echo "### $m done $(date +%H:%M:%S) exit $rc"
  python3 bench/probes.py --summary --results "$OUT/$tag" 2>/dev/null | grep '^\*\*'
done
echo "### battery done $(date '+%Y-%m-%d %H:%M:%S')"
