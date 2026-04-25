#!/usr/bin/env bash
# One-liner installer for inderes-cli release binaries.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/heikki-laitala/inderes-cli/main/install.sh | bash
#
# Env overrides:
#   INDERES_VERSION=v0.1.0       install a specific tag (default: latest)
#   INDERES_INSTALL_DIR=~/bin    install directory (default: ~/.local/bin)
#   INDERES_REPO=owner/repo      release source (default: heikki-laitala/inderes-cli)
#   GH_TOKEN=<token>             optional — when set, forwarded as
#                                `Authorization: Bearer` so GitHub's API calls
#                                use the 5000/hr authenticated rate limit
#                                (vs 60/hr anonymous) and private-repo
#                                mirrors work.

set -euo pipefail

REPO="${INDERES_REPO:-heikki-laitala/inderes-cli}"
VERSION="${INDERES_VERSION:-latest}"
INSTALL_DIR="${INDERES_INSTALL_DIR:-$HOME/.local/bin}"

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*" >&2; }
die()  { printf '\033[1;31m!!\033[0m  %s\n' "$*" >&2; exit 1; }

require() {
  command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"
}

require curl
require tar

# Optional: authenticated downloads for private releases or rate-limit boost.
# Curl's -L does not forward auth on redirect, so the S3-signed URL stays clean.
#
# NOTE on the `${curl_auth[@]+"${curl_auth[@]}"}` expansion used below: macOS
# ships bash 3.2 where `"${empty_array[@]}"` triggers `set -u`'s unbound-
# variable error. The `${arr[@]+…}` form expands to nothing when the array
# is empty, bypassing the quirk. Required for macOS runners; harmless on
# bash 4+.
curl_auth=()
if [[ -n "${GH_TOKEN:-}" ]]; then
  curl_auth+=(-H "Authorization: Bearer $GH_TOKEN")
fi

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

case "$os-$arch" in
  linux-x86_64)   target="x86_64-unknown-linux-gnu" ;;
  linux-aarch64 | linux-arm64) target="aarch64-unknown-linux-gnu" ;;
  darwin-x86_64)  target="x86_64-apple-darwin" ;;
  darwin-arm64)   target="aarch64-apple-darwin" ;;
  *)
    die "unsupported platform: $os-$arch (Windows users: download from https://github.com/$REPO/releases)"
    ;;
esac

if [[ "$VERSION" == "latest" ]]; then
  log "Resolving latest release for $REPO"
  # Fetch first, parse second. Piping curl into `awk '... {exit}'` closes
  # the pipe before curl finishes writing the body, which under
  # `set -o pipefail` propagates curl's SIGPIPE (exit 23) and aborts the
  # whole installer — even though the tag was extracted successfully.
  # See https://github.com/heikki-laitala/inderes-cli/issues/9.
  release_json="$(curl -fsSL ${curl_auth[@]+"${curl_auth[@]}"} \
    "https://api.github.com/repos/$REPO/releases/latest")" \
    || die "could not query latest release"
  VERSION="$(printf '%s\n' "$release_json" | awk -F'"' '/"tag_name":/ {print $4; exit}')"
  [[ -n "$VERSION" ]] || die "could not determine latest tag"
fi

archive="inderes-${target}.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Choose asset URLs. On private repos the user-facing github.com/.../download/
# URL 404s even with a Bearer token — the only reliable authenticated path is
# via the API with the asset's numeric id. For public repos the github.com
# URL works without auth. We switch on whether GH_TOKEN is set.
asset_accept=()
if [[ -n "${GH_TOKEN:-}" ]]; then
  log "Resolving asset IDs for $VERSION"
  release_meta="$(curl -fsSL ${curl_auth[@]+"${curl_auth[@]}"} \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/$REPO/releases/tags/$VERSION")" \
    || die "could not read release metadata for $VERSION"

  # Find the numeric "id" of the asset whose "name" equals the given arg.
  # Works on pretty-printed GitHub API JSON where each key is on its own line.
  lookup_asset_id() {
    printf '%s\n' "$release_meta" | awk -v want="$1" '
      /"id":[[:space:]]*[0-9]+/ {
        last_id = $0
        gsub(/[^0-9]/, "", last_id)
      }
      /"name":[[:space:]]*"/ {
        line = $0
        sub(/.*"name":[[:space:]]*"/, "", line)
        sub(/".*/, "", line)
        if (line == want) { print last_id; exit }
      }
    '
  }
  archive_id="$(lookup_asset_id "$archive")"
  sums_id="$(lookup_asset_id "SHA256SUMS")"
  [[ -n "$archive_id" ]] || die "asset $archive not found in release $VERSION"
  archive_url="https://api.github.com/repos/$REPO/releases/assets/$archive_id"
  sums_url=""
  [[ -n "$sums_id" ]] && sums_url="https://api.github.com/repos/$REPO/releases/assets/$sums_id"
  asset_accept+=(-H "Accept: application/octet-stream")
else
  archive_url="https://github.com/${REPO}/releases/download/${VERSION}/${archive}"
  sums_url="https://github.com/${REPO}/releases/download/${VERSION}/SHA256SUMS"
fi

log "Downloading $archive ($VERSION)"
curl -fsSL ${curl_auth[@]+"${curl_auth[@]}"} ${asset_accept[@]+"${asset_accept[@]}"} -o "$tmp/$archive" "$archive_url" \
  || die "download failed: $archive_url"

log "Verifying checksum"
if [[ -n "$sums_url" ]] && \
   curl -fsSL ${curl_auth[@]+"${curl_auth[@]}"} ${asset_accept[@]+"${asset_accept[@]}"} -o "$tmp/SHA256SUMS" "$sums_url" 2>/dev/null; then
  sum_line="$(grep -E "[[:space:]]${archive}\$" "$tmp/SHA256SUMS" || true)"
  if [[ -n "$sum_line" ]]; then
    (cd "$tmp" && printf '%s\n' "$sum_line" | shasum -a 256 -c -) \
      || die "checksum mismatch — refusing to install"
  else
    warn "$archive not listed in SHA256SUMS — skipping verification"
  fi
else
  warn "SHA256SUMS not available — skipping verification"
fi

log "Extracting"
tar -xzf "$tmp/$archive" -C "$tmp"

src_dir="$tmp/inderes-${target}"
[[ -f "$src_dir/inderes" ]] || die "archive layout unexpected; binary not at $src_dir/inderes"

mkdir -p "$INSTALL_DIR"
install -m 0755 "$src_dir/inderes" "$INSTALL_DIR/inderes"

log "Installed inderes $VERSION -> $INSTALL_DIR/inderes"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    warn "$INSTALL_DIR is not on your PATH."
    warn "Add this to your shell rc:"
    warn "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac

log "Next steps:"
log "  inderes login                 # sign in via your Inderes account"
log "  inderes install-skill         # drop SKILL.md into ~/.openclaw/skills/"
