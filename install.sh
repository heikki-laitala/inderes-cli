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
#   GH_TOKEN=<token>             forwarded as `Authorization: Bearer` on GitHub
#                                requests — needed when the repo is private.

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

# Optional: authenticated downloads for private releases. Curl's -L does not
# forward auth on redirect, so the S3-signed URL stays clean.
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
  VERSION="$(curl -fsSL "${curl_auth[@]}" "https://api.github.com/repos/$REPO/releases/latest" \
    | awk -F'"' '/"tag_name":/ {print $4; exit}')"
  [[ -n "$VERSION" ]] || die "could not determine latest tag"
fi

archive="inderes-${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/${VERSION}/${archive}"
sum_url="${url}.sha256"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

log "Downloading $archive ($VERSION)"
curl -fsSL "${curl_auth[@]}" -o "$tmp/$archive" "$url" || die "download failed: $url"

log "Verifying checksum"
if curl -fsSL "${curl_auth[@]}" -o "$tmp/$archive.sha256" "$sum_url" 2>/dev/null; then
  (cd "$tmp" && shasum -a 256 -c "$archive.sha256") \
    || die "checksum mismatch — refusing to install"
else
  warn "no .sha256 file found at $sum_url — skipping verification"
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
