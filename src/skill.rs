//! SKILL.md shipped with the binary via `include_str!`. `inderes install-skill`
//! writes a copy to the OpenClaw skills directory.

use std::path::PathBuf;

pub const SKILL_MD: &str = include_str!("skill/SKILL.md");

/// Default install location: `<home>/.openclaw/skills/inderes/SKILL.md`.
pub fn default_install_path() -> PathBuf {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".openclaw")
        .join("skills")
        .join("inderes")
        .join("SKILL.md")
}
