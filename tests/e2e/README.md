# E2E skill-loader tests

Validates that the two supported agent hosts can actually parse the `SKILL.md`
files shipped inside the `inderes` binary, and that the CLI itself is callable
without authentication.

## What each validator proves

For each host (OpenClaw, Hermes, ptrclaw):

1. Runs `inderes install-skill <host> --dest <tempdir>/inderes/SKILL.md --force`.
2. Feeds the resulting file to that host's own skill-loading code and asserts:
   - exactly one skill is discovered,
   - its `name` is `inderes`,
   - its `description` is a non-trivial string (≥20 chars).
3. Spawns `inderes --version` and `inderes whoami` and asserts both exit 0.
   `whoami` deliberately runs without auth — it prints `"Not signed in"` and
   exits 0, proving the CLI's no-auth code path is reachable.

No OAuth, no MCP calls, no LLM — the tests stay entirely offline from the
Inderes server.

## Running locally

```bash
./tests/e2e/run-local.sh
```

Defaults to:

- `OPENCLAW_DIR=~/dev/agents/openclaw`
- `HERMES_DIR=~/dev/agents/hermes-agent`
- `PTRCLAW_DIR=~/dev/agents/ptrclaw`

Override either with an env var, or skip a job:

```bash
OPENCLAW_DIR=/some/path ./tests/e2e/run-local.sh
SKIP_OPENCLAW=1 ./tests/e2e/run-local.sh
SKIP_HERMES=1   ./tests/e2e/run-local.sh
SKIP_PTRCLAW=1  ./tests/e2e/run-local.sh
```

The first OpenClaw run installs dependencies into
`~/dev/agents/openclaw/node_modules` and `tests/e2e/openclaw/node_modules` —
subsequent runs are fast. The ptrclaw validator is a small C++ program compiled
on demand against ptrclaw's own `skill.cpp` + `util.cpp`; it needs a working
`c++` (or `$CXX`) compiler on PATH.

## Running in CI

`.github/workflows/e2e.yml` runs both jobs in parallel on every push/PR that
touches `src/skill/**`, `src/skill.rs`, `src/commands.rs`, `src/main.rs`, or
`tests/e2e/**`, plus a weekly Monday cron.

Host repos are pinned to specific commit SHAs via workflow `env`:

- `OPENCLAW_SHA` → commit in `openclaw/openclaw`
- `HERMES_SHA`   → commit in `NousResearch/hermes-agent`

Bump them deliberately (e.g. quarterly) by editing the workflow. Unpinned
`main` would make CI fragile — an unrelated upstream change could break us.

`ptrclaw` is deliberately **not pinned**: it's the owner's own repo and the
goal is to catch skill-contract breakage between ptrclaw and inderes-cli
as soon as it appears. The ptrclaw job always clones `heikki-laitala/ptrclaw`
at `main`.

## Why not a full agent e2e?

Full-agent runs (scripted prompt → LLM → tool-call → `inderes` → MCP) need:

- an LLM API key (OpenAI / Anthropic / etc.),
- an Inderes Premium refresh token for the Bearer flow,
- ~minutes of wall time per run, with non-deterministic model output causing
  flakes.

None of those buy more safety than what these cheap validators already give
us: if our skill file stops parsing, or the CLI stops spawning, these tests
will catch it in seconds. A full agent run would catch the same breakages
plus model-interpretation drift — valuable, but not worth the ongoing
maintenance burden for a personal-use tool.

If that ever changes, add a manually-triggered `e2e-full.yml` gated on a
repository secret rather than expanding this workflow.
