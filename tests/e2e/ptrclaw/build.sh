#!/usr/bin/env bash
# Compile tests/e2e/ptrclaw/validate.cpp against ptrclaw's own source tree.
# Required env:
#   PTRCLAW_DIR — path to a checked-out ptrclaw repo.
# Optional env:
#   CXX         — C++ compiler (default: c++).
# Output:
#   tests/e2e/ptrclaw/validate  — the compiled validator binary.

set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
: "${PTRCLAW_DIR:?PTRCLAW_DIR env var required (path to ptrclaw repo)}"
: "${CXX:=c++}"

if [[ ! -f "$PTRCLAW_DIR/src/skill.cpp" || ! -f "$PTRCLAW_DIR/src/util.cpp" ]]; then
  echo "ptrclaw sources not found under $PTRCLAW_DIR/src" >&2
  exit 1
fi

"$CXX" -std=c++17 -O1 -Wall -Wextra \
  -I"$PTRCLAW_DIR/src" \
  "$here/validate.cpp" \
  "$PTRCLAW_DIR/src/skill.cpp" \
  "$PTRCLAW_DIR/src/util.cpp" \
  -o "$here/validate"

echo "built $here/validate"
