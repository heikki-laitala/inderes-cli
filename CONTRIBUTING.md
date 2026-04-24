# Contributing

Thanks for considering a contribution. This is a small single-maintainer
project — the bar for drive-by PRs is low, but a couple of conventions make
review quick and merges predictable.

## Requirements

- **Rust 1.82+** (stable toolchain; `rustfmt` and `clippy` are components)
- On Linux, the pre-built `libdbus-1-dev` requirement is *gone* since the
  keyring refactor — a plain `rustc` install is enough.

For the E2E skill-loader tests you also need:

- **OpenClaw checkout** (`git clone https://github.com/openclaw/openclaw`)
- **Hermes checkout** (`git clone https://github.com/NousResearch/hermes-agent`)
- **ptrclaw checkout** (`git clone https://github.com/heikki-laitala/ptrclaw`)
- `pnpm` (OpenClaw), `python3.11+` (Hermes), a C++17 compiler (ptrclaw)

Paths default to `~/dev/agents/{openclaw,hermes-agent,ptrclaw}`; override
per-host via env vars as documented in `tests/e2e/README.md`.

## Pre-commit gates

Run the full set before opening a PR — CI runs the same commands on
Linux / macOS / Windows and fails the PR if any of them don't:

```bash
cargo fmt --all                            # format (in-place)
cargo fmt --all -- --check                 # verify formatted
cargo clippy --all-targets -- -D warnings  # lint, warnings-as-errors
cargo test --all-targets                   # 92 unit + integration tests
npx -y markdownlint-cli2@0.22.1            # Markdown (uses .markdownlint-cli2.jsonc)
```

If you've changed anything under `src/skill/**`, `src/skill.rs`,
`src/commands.rs`, `src/main.rs`, or `tests/e2e/**`, also run the E2E
validators locally:

```bash
./tests/e2e/run-local.sh
```

## Conventions

- **Conventional commits** (`feat:`, `fix:`, `refactor:`, `test:`, `ci:`,
  `docs:`, `chore:`). Keep subjects under ~72 chars.
- **No `Co-Authored-By` trailers** and **no "Generated with Claude Code"
  footers** in commit messages or PR descriptions.
- **Calendar-versioned releases** (`YYYY.M.D`). Cargo rejects leading
  zeros, so keep `Cargo.toml` in sync with the tag form
  (`2026.4.24` → `v2026.4.24`).
- **Don't add dependencies casually.** Each new `[dependencies]` entry
  should justify itself in the PR description. Dev-deps are more forgiving
  but still prefer the standard library.

## Scope notes

This CLI deliberately does *not* try to be feature-complete across every
Inderes MCP tool. Friendly subcommands cover the 80% path; `inderes call
<tool>` is the escape hatch for everything else. If you're adding a new
friendly subcommand, consider whether `inderes call` already serves the
use case.

Skill files (`src/skill/{openclaw,hermes,ptrclaw}.md`) should stay small
and token-efficient — they're loaded into model context at runtime. Don't
pad them with duplicate examples.

## Release process

Releases are cut by bumping `Cargo.toml` version to `YYYY.M.D`, committing
with `release: vYYYY.M.D`, tagging `vYYYY.M.D`, and pushing the tag. The
release workflow builds all five target triples and uploads to a GitHub
Release; the smoke-install workflow then verifies the install scripts
work on every platform.

## Reporting bugs

- **Security issues**: see [SECURITY.md](./SECURITY.md) — don't open a
  public issue.
- **Inderes data / API issues**: email `support@inderes.fi`; this repo
  has no influence over the server.
- **Everything else**: open a
  [GitHub issue](https://github.com/heikki-laitala/inderes-cli/issues/new).

## Project-agent conventions

If you're using an LLM agent to help with contributions, the agent-facing
conventions (design constraints, non-obvious invariants) live in
[CLAUDE.md](./CLAUDE.md) — worth a skim to avoid suggesting changes that
intentionally aren't there (e.g. keyring integration, persistent daemon).
