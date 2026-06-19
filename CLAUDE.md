# Inderes-CLI

Unofficial Rust CLI wrapping the hosted MCP server at `https://mcp.inderes.com/`. The binary is installed as `inderes` and talks MCP to the server on the user's behalf after an OAuth 2.0 (auth code + PKCE) sign-in against the Inderes Keycloak realm at `https://sso.inderes.fi/auth/realms/Inderes`.

**Design constraint (do not revisit without discussion).** This is deliberately *not* registered as an MCP server with any agent host. The goal is to keep the agent's per-turn context small: a host that registers the Inderes MCP loads every one of its 16 tool schemas on every turn. Instead, agents invoke the binary via their terminal/bash/shell tool using one of the embedded `SKILL.md` files (`src/skill/{openclaw,hermes,ptrclaw}.md`) installed by `inderes install-skill <host>`. Friendly subcommands (`search`, `fundamentals`, `estimates`, `content`, `documents`) cover the common path; `inderes call <tool>` is the escape hatch for the remaining tools.

**Non-obvious invariants.**

- Tokens live as a JSON file at the platform config dir (`directories::ProjectDirs`), written atomically and `chmod 0600` on Unix. No OS keychain integration — we opted for file-only simplicity since this is a personal-use CLI. Never log, print, or include tokens in error messages.
- Versioning is **calver** (`YYYY.M.D`). Cargo strips leading zeros (`2026.4.24`, not `2026.04.24`); git tags mirror the Cargo version so the release workflow picks them up.
- Only the `inderes-mcp` Keycloak client ID is guaranteed to have localhost redirects whitelisted; do not hardcode alternates.
- The crate supports Linux, macOS, and Windows equally. Avoid platform-specific code outside `#[cfg]`-gated helpers (see `storage::set_file_perms_0600`).

## Engineering Principles

### KISS

Prefer straightforward control flow. Keep error paths obvious and localized.

### YAGNI

Do not add interfaces, config keys, or abstractions without a concrete caller. No speculative features.

### DRY (Rule of Three)

Duplicate small local logic when it preserves clarity. Extract shared helpers only after three repeated, stable patterns.

### TDD

Write tests first. Red → Green → Refactor. New features and bug fixes start with a failing test that defines the expected behavior before writing implementation code.

### Secure by Default

Never log secrets or tokens. Validate at system boundaries. Keep network/filesystem/shell scope narrow.

## Process rules

1. **Think before coding** — State assumptions. Ask if uncertain. Present alternatives when ambiguous. Stop when confused.
2. **Simplicity first** — Minimum code that solves the problem. No abstractions for single-use code. No features beyond what was asked.
3. **Surgical changes** — Touch only what you must. Don't "improve" adjacent code, comments, or formatting.
4. **Goal-driven, not step-driven** — Define success criteria, iterate until verified.
5. **Use the model only for judgment calls** — Classification, drafting, summarization, extraction. Not for routing, retries, or deterministic transforms. If code can answer, code answers.
6. **Surface conflicts, don't average them** — If two patterns contradict, pick one (more recent / more tested), explain why, flag the other.
7. **Tests verify intent** — Tests must encode *why* behavior matters, not just *what* it does. A test that can't fail when business logic changes is wrong.
8. **Checkpoint after every significant step** — Summarize what's done, verified, and left. Don't continue from a state you can't describe back.

## Before Committing

Always run lint and tests before creating commits or PRs:

```bash
cargo fmt                              # format code
cargo clippy --all-targets -- -D warnings   # lint (must pass clean)
cargo test --all-targets               # unit + integration tests
npx -y markdownlint-cli2@0.22.1        # markdown lint (uses .markdownlint-cli2.jsonc)
```

## Conventions

- **Git**: Conventional commits (`feat:`, `fix:`, `chore:`, `refactor:`, `test:`, `ci:`, `docs:`). No `Co-Authored-By` trailer. No "Generated with Claude Code" footer in PR descriptions.
- **Releases**: Calendar-versioned. Bump `Cargo.toml` `version = "YYYY.M.D"`, commit, then `git tag vYYYY.M.D && git push origin vYYYY.M.D` — the release workflow builds binaries for the five target triples and uploads `SHA256SUMS`.
- **Repo layout**: `src/main.rs` (clap dispatch) → `src/commands.rs` (subcommand bodies) → `src/mcp.rs` (Streamable-HTTP JSON-RPC) ← `src/auth.rs` ← `src/oauth.rs` + `src/storage.rs`. The skill text lives at `src/skill/SKILL.md` and is embedded into the binary via `include_str!`.
- **Code review**: Before opening a PR, run the local reviewer — `codex review --base main --title "<pr title>"` from the repo root — and fix its findings first. Then open the PR and post a comment `@codex review` to trigger the GitHub automated review (Codex does not review unless asked). When that review arrives, fix every finding and reply to the reviewer explaining each fix. Once the fixes are pushed and CI is green, merge the PR automatically — no further confirmation needed.
