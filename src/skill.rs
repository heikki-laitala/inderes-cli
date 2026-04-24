//! Skill bodies shipped with the binary. `inderes install-skill <host>`
//! writes a copy into the matching agent's skills directory.

use std::path::PathBuf;

use clap::ValueEnum;

const OPENCLAW_SKILL: &str = include_str!("skill/openclaw.md");
const HERMES_SKILL: &str = include_str!("skill/hermes.md");

/// Supported agent hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Host {
    /// OpenClaw — skill at `~/.openclaw/skills/inderes/SKILL.md`.
    Openclaw,
    /// Hermes Agent — skill at `~/.hermes/skills/inderes/SKILL.md`.
    Hermes,
}

impl Host {
    /// The embedded SKILL.md body for this host.
    pub fn body(self) -> &'static str {
        match self {
            Host::Openclaw => OPENCLAW_SKILL,
            Host::Hermes => HERMES_SKILL,
        }
    }

    /// Per-host conventional install path: `<home>/.<host>/skills/inderes/SKILL.md`.
    pub fn default_install_path(self) -> PathBuf {
        let dir = match self {
            Host::Openclaw => ".openclaw",
            Host::Hermes => ".hermes",
        };
        home_dir()
            .join(dir)
            .join("skills")
            .join("inderes")
            .join("SKILL.md")
    }
}

fn home_dir() -> PathBuf {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_skills_are_non_empty() {
        assert!(Host::Openclaw.body().contains("name: inderes"));
        assert!(Host::Hermes.body().contains("name: inderes"));
    }

    #[test]
    fn openclaw_path_ends_with_openclaw_skills() {
        let p = Host::Openclaw.default_install_path();
        let tail: PathBuf = [".openclaw", "skills", "inderes", "SKILL.md"]
            .iter()
            .collect();
        assert!(p.ends_with(&tail), "got {}", p.display());
    }

    #[test]
    fn hermes_path_ends_with_hermes_skills() {
        let p = Host::Hermes.default_install_path();
        let tail: PathBuf = [".hermes", "skills", "inderes", "SKILL.md"]
            .iter()
            .collect();
        assert!(p.ends_with(&tail), "got {}", p.display());
    }

    #[test]
    fn hermes_skill_has_hermes_metadata() {
        assert!(Host::Hermes.body().contains("metadata:"));
        assert!(Host::Hermes.body().contains("hermes:"));
    }

    #[test]
    fn openclaw_skill_has_openclaw_metadata() {
        assert!(Host::Openclaw.body().contains("openclaw"));
    }
}
