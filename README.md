# inderes-cli

Unofficial CLI for the [Inderes MCP server](https://mcp.inderes.com/). A thin terminal-friendly wrapper around the hosted MCP endpoint, designed to pair with an on-demand agent skill (OpenClaw, Hermes, or ptrclaw) so an agent can reach Inderes data without loading every MCP tool schema into its context on every turn.

> **Disclaimer.** This project is a community tool. It is **not affiliated with Inderes Oyj** and the authors have no relationship with Inderes beyond being subscribers. You need your own [Inderes Premium](https://www.inderes.fi/premium) subscription — the CLI never bypasses authentication.

## Why this exists

MCP is a clean protocol for "give an agent a data source" and it's the right answer for most cases. But when a server exposes many tools that an agent uses only occasionally, the default integration has real costs worth surfacing before you adopt it.

**1. Tool schemas are always resident in model context.**
Inderes exposes 16 tools. Registering its MCP server directly with an agent host means roughly 1.5–3k tokens of tool descriptions sit in the system prompt of every single turn, regardless of what the user is asking. Over a 40-turn conversation that's ~80k tokens of dead weight — paid both in input cost and in the model's attention budget. The MCP spec has no "lazy load" for schemas; they come down at session start and stay.

**2. Most MCP client hosts don't handle OAuth.**
They accept a static bearer token you paste into config. Inderes's Keycloak issues access tokens that expire in minutes, so the status-quo workflow is "copy fresh token → paste → restart agent → repeat." This CLI does a full PKCE browser login once, stores a refresh token in the OS-appropriate config dir with 0600 perms, and auto-rotates. The agent host never sees credentials.

**3. MCP is agent-only.**
Useful for agents. Not useful if you want to grep analyst comments from a shell pipeline, cron-schedule earnings-transcript downloads, or pipe results through `jq`. This crate is the missing CLI; every subcommand has `--json` so agent-adjacent scripting works too.

**4. On-demand skills are cheaper than always-on schemas.**
OpenClaw, Hermes, and ptrclaw all load skill markdown lazily — only when the model decides the skill is relevant to the current turn. A ~400-token skill that teaches the model to shell out to `inderes <subcommand>` is roughly 4–8× cheaper per conversation than keeping 16 full tool schemas permanently resident, and the skill doesn't re-read on every turn the way a system prompt does.

### When you should NOT use this

If your agent is dedicated to Inderes (you use its tools on most turns) or you're running a headless service that talks to one specific MCP server, direct MCP registration is simpler — one line in the host config, and the per-turn context cost is paid on turns where you'd want the schemas loaded anyway. This CLI's win is the occasional-use case: Inderes queries interspersed with other work.

## Install

### macOS / Linux — install script (recommended)

```bash
curl -sSL https://raw.githubusercontent.com/heikki-laitala/inderes-cli/main/install.sh | bash
```

Downloads the latest release binary for your OS+arch into `~/.local/bin/inderes`, verifies SHA-256 against the release's `SHAxxxxSUMS` sidecar, and prints a PATH reminder if needed. No authentication needed — release assets are public.

If you hit GitHub's anonymous rate limit (60/hr) on a shared network, export `GH_TOKEN=$(gh auth token)` before running to use your authenticated 5000/hr quota.

### Any platform — cargo

```bash
cargo install --git https://github.com/heikki-laitala/inderes-cli
```

Requires Rust 1.82+.

### Windows — PowerShell (recommended)

```powershell
iwr -useb https://raw.githubusercontent.com/heikki-laitala/inderes-cli/main/install.ps1 | iex
```

Installs `inderes.exe` into `%LOCALAPPDATA%\Programs\inderes\bin` and prints a PATH reminder if needed. Verifies SHA-256 before installing. Set `$env:GH_TOKEN` first only if you're hitting GitHub's anonymous rate limit.

Or download `inderes-x86_64-pc-windows-msvc.zip` from the [latest release](https://github.com/heikki-laitala/inderes-cli/releases/latest) manually.

## First run

```bash
inderes login
```

Opens your default browser and signs you in with your Inderes account via OAuth 2.0 (authorization code + PKCE) against Inderes's Keycloak (`sso.inderes.fi`). Tokens are stored as a JSON file in the platform config directory (`0600` on Unix, per-user `%APPDATA%` ACLs on Windows).

If the redirect fails with "Invalid redirect URI", follow the guidance in the [Inderes MCP setup docs](https://mcp.inderes.com/docs/setup) and email `support@inderes.fi` with the client name `inderes-mcp` and the exact error URL.

### Headless / SSH / agent flow

The default `inderes login` binds a loopback HTTP listener for the OAuth callback. That fails when you're on a machine without a browser and the actual user is elsewhere — typical scenarios:

- SSH'd into a remote box; browser is on your laptop
- Running inside Docker without port forwarding
- Driven by an agent (OpenClaw, Hermes, ptrclaw) that doesn't have a graphical session

Use `--paste-callback` for those:

```bash
inderes login --paste-callback
```

The CLI prints the auth URL, you open it in any browser on any machine, sign in, and your browser tries to redirect to a localhost URL — which will show "unable to connect" because no listener is running. **That's expected.** Copy the full URL from your browser's address bar (it looks like `http://127.0.0.1:46233/callback?state=…&code=…`) and paste it back to the CLI's prompt. The CLI extracts the `code`, validates `state`, and exchanges it for tokens just like the loopback flow.

For agent integrations: the CLI prints the auth URL to stderr and reads the pasted URL from stdin, so an agent driving `inderes login --paste-callback` as a subprocess can:

1. Read stderr to surface the auth URL to the user.
2. Capture the user's pasted URL through whatever UI the agent uses.
3. Write that URL plus a newline to the subprocess's stdin.
4. Wait for the subprocess to exit successfully — tokens are now stored.

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

## Upgrade

```bash
inderes upgrade --check-only   # just print current vs latest
inderes upgrade                # install the latest release in place
inderes upgrade --force        # re-install even if already on latest
```

The CLI queries GitHub for the latest tag, compares to the running version, and (if newer or `--force`) shells out to the same `install.sh` / `install.ps1` you'd `curl | bash`. The new binary lands in the same directory the running binary was launched from, so an upgrade preserves the install location regardless of where you originally put it.

`inderes upgrade --check-only` is safe to run from cron or an agent.

## Uninstall

```bash
inderes uninstall                            # confirms, clears tokens, prints rm hint
inderes uninstall --yes                      # skip the confirmation prompt
inderes uninstall --yes --remove-skills      # also delete ~/.<host>/skills/inderes/
```

`uninstall` clears stored tokens, optionally removes installed skill files, and prints the platform-appropriate command to delete the binary itself. The CLI doesn't self-delete its running executable — that step is left to you because it's the only sane cross-platform answer (Unix permits it; Windows requires a workaround that has its own footguns).

## Install the skill

The `inderes` binary is designed to be invoked by an agent through an on-demand skill — keeps per-turn context small. Three hosts are supported out of the box:

```bash
inderes install-skill openclaw   # -> ~/.openclaw/skills/inderes/SKILL.md
inderes install-skill hermes     # -> ~/.hermes/skills/inderes/SKILL.md
inderes install-skill ptrclaw    # -> ~/.ptrclaw/skills/inderes/SKILL.md
```

Pass `--force` to overwrite an existing skill, `--dest <path>` to write somewhere else. The skill content is shipped inside the binary, so reinstalling after a CLI upgrade always gives the agent up-to-date guidance.

All three skills teach the model to shell out to `inderes <subcommand>` via the host's terminal/bash/shell tool — no MCP server registration, no tool-schema bloat.

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
- `src/skill/{openclaw,hermes,ptrclaw}.md` — embedded at compile time; `inderes install-skill <host>` writes the right one to disk.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

CI runs the same checks on `ubuntu-latest`, `macos-latest`, and `windows-latest`.

## License

MIT. See [LICENSE](./LICENSE).
