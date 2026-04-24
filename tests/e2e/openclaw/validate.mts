// E2E validator for OpenClaw integration.
//
// Runs in two steps:
//   1. Asks the `inderes` binary to install its OpenClaw skill into a tempdir.
//   2. Calls OpenClaw's own `loadSkillsFromDirSafe` against that tempdir and
//      asserts the skill registered with the expected name + a non-trivial
//      description.
//   3. Spawns `inderes --version` and `inderes whoami` to prove the CLI is
//      callable (no auth required — whoami reports "Not signed in").
//
// Required env:
//   OPENCLAW_DIR — path to a checked-out openclaw repo (with `pnpm install`
//                  already run so the loader's internal deps resolve).
//
// Optional env:
//   INDERES_BIN  — path to the inderes binary (default: "inderes" on PATH).

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const openclawDir = process.env.OPENCLAW_DIR;
if (!openclawDir) {
  fatal("OPENCLAW_DIR env var required (path to a checked-out openclaw repo)");
}
const loaderTs = resolve(openclawDir!, "src/agents/skills/local-loader.ts");
if (!existsSync(loaderTs)) {
  fatal(`OpenClaw loader not found at ${loaderTs} — is OPENCLAW_DIR correct?`);
}

const inderesBin = process.env.INDERES_BIN ?? "inderes";

// ---------------------------------------------------------------- install
const skillsRoot = mkdtempSync(join(tmpdir(), "inderes-e2e-openclaw-"));
const skillDir = join(skillsRoot, "inderes");
mkdirSync(skillDir, { recursive: true });
const skillFile = join(skillDir, "SKILL.md");

const install = spawnSync(
  inderesBin,
  ["install-skill", "openclaw", "--dest", skillFile, "--force"],
  { stdio: "inherit" },
);
if (install.status !== 0) {
  fatal(`inderes install-skill exited with ${install.status}`);
}
if (!existsSync(skillFile)) {
  fatal(`install-skill returned 0 but ${skillFile} is missing`);
}

// ------------------------------------------------------------------ load
// Dynamic import of OpenClaw's TypeScript loader. tsx handles the .ts →
// runtime transform; the file URL avoids CommonJS path resolution pitfalls.
const loaderMod = (await import(pathToFileURL(loaderTs).href)) as {
  loadSkillsFromDirSafe: (params: { dir: string; source: string }) => {
    skills: Array<{
      name: string;
      description?: string;
      filePath: string;
    }>;
  };
};

const { skills } = loaderMod.loadSkillsFromDirSafe({
  dir: skillsRoot,
  source: "inderes-cli-e2e",
});

if (skills.length !== 1) {
  fatal(`expected exactly 1 skill, got ${skills.length}`);
}
const [skill] = skills;
if (skill.name !== "inderes") {
  fatal(`expected skill name "inderes", got ${JSON.stringify(skill.name)}`);
}
if (!skill.description || skill.description.length < 20) {
  fatal(
    `description missing or too short (got ${skill.description?.length ?? 0} chars)`,
  );
}

// --------------------------------------------------------- cli executable
const version = spawnSync(inderesBin, ["--version"], { encoding: "utf8" });
if (version.status !== 0 || !version.stdout.includes("inderes")) {
  fatal(
    `inderes --version failed: status=${version.status}, stdout=${JSON.stringify(
      version.stdout,
    )}, stderr=${JSON.stringify(version.stderr)}`,
  );
}

const whoami = spawnSync(inderesBin, ["whoami"], { encoding: "utf8" });
if (whoami.status !== 0) {
  fatal(
    `inderes whoami failed: status=${whoami.status}, stdout=${JSON.stringify(
      whoami.stdout,
    )}, stderr=${JSON.stringify(whoami.stderr)}`,
  );
}

// ---------------------------------------------------------------- done
console.log("OK  openclaw skill loader integration");
console.log(`    skill.name        = ${skill.name}`);
console.log(`    skill.description = ${skill.description!.length} chars`);
console.log(`    inderes --version = ${version.stdout.trim()}`);
console.log(`    inderes whoami    = ${whoami.stdout.trim()}`);

function fatal(msg: string): never {
  console.error(`FAIL  ${msg}`);
  process.exit(1);
}
