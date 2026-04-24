# inderes-cli

Unofficial CLI for the [Inderes MCP server](https://mcp.inderes.com/). A thin terminal-friendly wrapper around the hosted MCP endpoint, designed to pair with an on-demand agent skill (OpenClaw or Hermes) so an agent can reach Inderes data without loading every MCP tool schema into its context on every turn.

> **Disclaimer.** This project is a community tool. It is **not affiliated with Inderes Oyj** and the authors have no relationship with Inderes beyond being subscribers. You need your own [Inderes Premium](https://www.inderes.fi/premium) subscription — the CLI never bypasses authentication.

## Why this exists

Inderes exposes 16 MCP tools. Registering its MCP server directly with an agent host loads all of those tool schemas into model context on every turn (~1.5–3k tokens, even when unused). This CLI inverts the relationship:

- The binary talks MCP to `mcp.inderes.com` privately.
- The agent sees a single small skill file (~500 tokens) that documents a handful of `inderes <subcommand>` invocations.
- The agent shells out to `inderes` when it needs Nordic-equity data — on demand, not preloaded.

## Install

### macOS / Linux — install script (recommended)

```bash
curl -sSL https://raw.githubusercontent.com/heikki-laitala/inderes-cli/main/install.sh | bash
```

Downloads the latest release binary for your OS+arch into `~/.local/bin/inderes`, verifies SHA-256, and prints a PATH reminder if needed.

### Any platform — cargo

```bash
cargo install --git https://github.com/heikki-laitala/inderes-cli
```

Requires Rust 1.82+.

### Windows — PowerShell (recommended)

```powershell
iwr -useb https://raw.githubusercontent.com/heikki-laitala/inderes-cli/main/install.ps1 | iex
```

Installs `inderes.exe` into `%LOCALAPPDATA%\Programs\inderes\bin` and prints a PATH reminder if needed. Verifies SHA-256 before installing.

Or download `inderes-x86_64-pc-windows-msvc.zip` from the [latest release](https://github.com/heikki-laitala/inderes-cli/releases/latest) manually.

## First run

```bash
inderes login
```

Opens your default browser and signs you in with your Inderes account via OAuth 2.0 (authorization code + PKCE) against Inderes's Keycloak (`sso.inderes.fi`). Tokens are stored as a JSON file in the platform config directory (`0600` on Unix, per-user `%APPDATA%` ACLs on Windows).

If the redirect fails with "Invalid redirect URI", follow the guidance in the [Inderes MCP setup docs](https://mcp.inderes.com/docs/setup) and email `support@inderes.fi` with the client name `inderes-mcp` and the exact error URL.

## Usage

```bash
inderes search "Nokia"
inderes fundamentals COMPANY:200 --field revenue --field ebitda --from-year 2020
inderes estimates COMPANY:200 --field revenue --field eps --quarters --years 3
inderes content list --company-id COMPANY:200 --type COMPANY_REPORT --first 10
inderes content get ANALYST_COMMENT:directus-1234
inderes documents list COMPANY:200
inderes documents read <documentId> -s 1,2,5
```

### The 16-tool escape hatch

Friendly subcommands cover ~half of Inderes's MCP surface. For the rest:

```bash
inderes call --list                                       # see all tools
inderes call list-transcripts --arg companyId=COMPANY:200
inderes call search-forum-topics --arg text=Nokia --arg order=relevancy
inderes call list-calendar-events --arg 'regions=["FINLAND","SWEDEN"]'
inderes call get-model-portfolio-content
```

`--arg KEY=VALUE` parses `VALUE` as JSON when possible (numbers, booleans, arrays, objects, quoted strings); otherwise it's a plain string. For a full object, use `--json-args '{"key":"value"}'`.

### Machine-readable output

Every tool-calling subcommand accepts `--json` to emit raw MCP output:

```bash
inderes --json search "Nokia" | jq '.content[0].text'
```

## Install the skill

The `inderes` binary is designed to be invoked by an agent through an on-demand skill — keeps per-turn context small. Two hosts are supported out of the box:

```bash
inderes install-skill openclaw   # -> ~/.openclaw/skills/inderes/SKILL.md
inderes install-skill hermes     # -> ~/.hermes/skills/inderes/SKILL.md
```

Pass `--force` to overwrite an existing skill, `--dest <path>` to write somewhere else. The skill content is shipped inside the binary, so reinstalling after a CLI upgrade always gives the agent up-to-date guidance.

Both skills teach the model to shell out to `inderes <subcommand>` via the host's terminal/bash tool — no MCP server registration, no tool-schema bloat.

## Shell completions

```bash
inderes completions bash       > /etc/bash_completion.d/inderes
inderes completions zsh        > "${fpath[1]}/_inderes"
inderes completions fish       > ~/.config/fish/completions/inderes.fish
inderes completions powershell | Out-String | Invoke-Expression
```

## Configuration

| Variable / flag | Purpose | Default |
|---|---|---|
| `--endpoint` / `INDERES_MCP_ENDPOINT` | Override the MCP HTTP endpoint | `https://mcp.inderes.com/` |
| `--json` | Emit raw JSON from MCP tool calls | off |
| `-v` / `-vv` | Increase logging verbosity (`warn` → `info` → `debug`) | `warn` |
| `INDERES_LOG` | `tracing` env filter (overrides `-v`) | unset |

Token file paths:

- **macOS:** `~/Library/Application Support/com.inderes.inderes-cli/tokens.json`
- **Linux:** `~/.config/inderes-cli/tokens.json`
- **Windows:** `%APPDATA%\inderes\inderes-cli\config\tokens.json`

Written atomically (tempfile + rename) so a crash can't leave a half-written file. Unix enforces `0600`; Windows relies on per-user AppData ACLs.

## Architecture

```
 agent ─┐                    ┌─ tokens.json (0600)
        │                    │
        ▼                    ▼
     SKILL.md ──shells──▶ inderes ──OAuth──▶ sso.inderes.fi (Keycloak)
                             │
                             │ Bearer + JSON-RPC
                             ▼
                        mcp.inderes.com  (Streamable-HTTP MCP)
```

- `src/oauth.rs` — PKCE S256, loopback redirect on ephemeral port, refresh grant.
- `src/storage.rs` — atomic-rename JSON file at the platform config dir (0600 on Unix).
- `src/mcp.rs` — MCP 2025-03-26 Streamable HTTP client, handles both `application/json` and `text/event-stream` responses.
- `src/commands.rs` — subcommand implementations and output formatting.
- `src/skill/SKILL.md` — embedded at compile time; `inderes install-skill` writes it to disk.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

CI runs the same checks on `ubuntu-latest`, `macos-latest`, and `windows-latest`.

## Releases

Versions use calendar-versioning (`YYYY.M.D`). A push of a `vYYYY.M.D` tag triggers the release workflow, which builds binaries for the five supported target triples, uploads them to a GitHub Release along with `SHA256SUMS`, and is what `install.sh` points at.

```bash
git tag v2026.4.24 && git push origin v2026.4.24
```

Cargo rejects leading zeros in semver components, so `Cargo.toml` uses the same zero-free form (`2026.4.24`) — keep them matched when bumping.

## License

MIT. See [LICENSE](./LICENSE).
