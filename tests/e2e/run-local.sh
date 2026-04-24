#!/usr/bin/env bash
# Run the e2e skill-loader validators against local checkouts of the
# OpenClaw and Hermes repos. Builds inderes in release mode first.
#
# Env overrides:
#   OPENCLAW_DIR  — default: ~/dev/agents/openclaw
#   HERMES_DIR    — default: ~/dev/agents/hermes-agent
#   SKIP_OPENCLAW=1 — skip the OpenClaw job
#   SKIP_HERMES=1   — skip the Hermes job

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
e2e_dir="$repo_root/tests/e2e"

OPENCLAW_DIR="${OPENCLAW_DIR:-$HOME/dev/agents/openclaw}"
HERMES_DIR="${HERMES_DIR:-$HOME/dev/agents/hermes-agent}"

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m!!\033[0m  %s\n' "$*" >&2; exit 1; }

log "Building inderes (release)"
(cd "$repo_root" && cargo build --release --quiet)
export INDERES_BIN="$repo_root/target/release/inderes"
[[ -x "$INDERES_BIN" ]] || fail "inderes binary missing at $INDERES_BIN"

# ---- OpenClaw ---------------------------------------------------------------
if [[ "${SKIP_OPENCLAW:-0}" == "1" ]]; then
  log "Skipping OpenClaw (SKIP_OPENCLAW=1)"
else
  [[ -d "$OPENCLAW_DIR" ]] || fail "OPENCLAW_DIR not found: $OPENCLAW_DIR"
  log "OpenClaw validator (OPENCLAW_DIR=$OPENCLAW_DIR)"

  if [[ ! -d "$OPENCLAW_DIR/node_modules" ]]; then
    log "Installing OpenClaw deps (pnpm install --filter .) — this may take a minute"
    (cd "$OPENCLAW_DIR" && pnpm install --filter . --prefer-offline --ignore-scripts)
  fi

  if [[ ! -d "$e2e_dir/openclaw/node_modules" ]]; then
    log "Installing validator deps (tsx)"
    (cd "$e2e_dir/openclaw" && npm install --silent --no-audit --no-fund)
  fi

  (
    cd "$e2e_dir/openclaw"
    OPENCLAW_DIR="$OPENCLAW_DIR" \
      INDERES_BIN="$INDERES_BIN" \
      npx --no-install tsx validate.mts
  )
fi

# ---- Hermes -----------------------------------------------------------------
if [[ "${SKIP_HERMES:-0}" == "1" ]]; then
  log "Skipping Hermes (SKIP_HERMES=1)"
else
  [[ -d "$HERMES_DIR" ]] || fail "HERMES_DIR not found: $HERMES_DIR"
  log "Hermes validator (HERMES_DIR=$HERMES_DIR)"
  HERMES_DIR="$HERMES_DIR" \
    INDERES_BIN="$INDERES_BIN" \
    python3 "$e2e_dir/hermes/validate.py"
fi

log "All e2e checks passed"
