"""E2E validator for Hermes Agent integration.

1. Asks the ``inderes`` binary to install its Hermes skill into a tempdir.
2. Calls Hermes's own ``iter_skill_index_files`` + ``_parse_frontmatter``
   against that tempdir and asserts the skill registered with the expected
   name + a non-trivial description.
3. Spawns ``inderes --version`` and ``inderes whoami`` to prove the CLI is
   callable (no auth required — whoami reports "Not signed in").

Required env:
    HERMES_DIR  — path to a checked-out hermes-agent repo that has been
                  installed with ``pip install -e .`` so ``agent`` and
                  ``tools`` are importable.

Optional env:
    INDERES_BIN — path to the inderes binary (default: "inderes" on PATH).
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path


def fatal(msg: str) -> None:
    print(f"FAIL  {msg}", file=sys.stderr)
    sys.exit(1)


hermes_dir = os.environ.get("HERMES_DIR")
if not hermes_dir:
    fatal("HERMES_DIR env var required (path to a checked-out hermes-agent repo)")

hermes_path = Path(hermes_dir)  # type: ignore[arg-type]
if not (hermes_path / "agent" / "skill_utils.py").exists():
    fatal(f"Hermes loader not found under {hermes_path} — is HERMES_DIR correct?")

# Prepend the repo so `agent.*` / `tools.*` imports resolve even without
# `pip install -e .` (useful for fast local runs). CI still runs pip install
# so both paths converge on the same API.
sys.path.insert(0, str(hermes_path))

try:
    from agent.skill_utils import iter_skill_index_files  # type: ignore
    from tools.skills_tool import _parse_frontmatter  # type: ignore
except ImportError as e:
    fatal(f"could not import Hermes skill loaders: {e}")

inderes_bin = os.environ.get("INDERES_BIN", "inderes")

with tempfile.TemporaryDirectory(prefix="inderes-e2e-hermes-") as tmp:
    skills_root = Path(tmp)
    skill_file = skills_root / "inderes" / "SKILL.md"
    skill_file.parent.mkdir(parents=True, exist_ok=True)

    # Install skill via the CLI, pointed at the tempdir.
    install = subprocess.run(
        [
            inderes_bin,
            "install-skill",
            "hermes",
            "--dest",
            str(skill_file),
            "--force",
        ]
    )
    if install.returncode != 0:
        fatal(f"inderes install-skill exited with {install.returncode}")
    if not skill_file.exists():
        fatal(f"install-skill returned 0 but {skill_file} is missing")

    # Load via Hermes's own scanner + frontmatter parser.
    found = list(iter_skill_index_files(skills_root, "SKILL.md"))
    if len(found) != 1:
        fatal(f"expected exactly 1 SKILL.md under {skills_root}, found {len(found)}")

    content = found[0].read_text(encoding="utf-8")
    frontmatter, _body = _parse_frontmatter(content)
    if frontmatter.get("name") != "inderes":
        fatal(f'expected name "inderes", got {frontmatter.get("name")!r}')
    desc = str(frontmatter.get("description") or "")
    if len(desc) < 20:
        fatal(f"description missing or too short (got {len(desc)} chars)")

    # Prove the CLI is callable.
    version = subprocess.run(
        [inderes_bin, "--version"], capture_output=True, text=True
    )
    if version.returncode != 0 or "inderes" not in (version.stdout or ""):
        fatal(
            f"inderes --version failed: status={version.returncode}, "
            f"stdout={version.stdout!r}, stderr={version.stderr!r}"
        )

    whoami = subprocess.run([inderes_bin, "whoami"], capture_output=True, text=True)
    if whoami.returncode != 0:
        fatal(
            f"inderes whoami failed: status={whoami.returncode}, "
            f"stdout={whoami.stdout!r}, stderr={whoami.stderr!r}"
        )

print("OK  hermes skill loader integration")
print(f'    skill.name        = {frontmatter["name"]}')
print(f"    skill.description = {len(desc)} chars")
print(f"    inderes --version = {version.stdout.strip()}")
print(f"    inderes whoami    = {whoami.stdout.strip()}")
