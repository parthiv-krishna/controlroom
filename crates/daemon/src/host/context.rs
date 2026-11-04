//! What a workspace tells the agent about itself: instruction files from `/` down to the root
//! (in each directory the first of `AGENTS.override.md`, `AGENTS.md`, `CLAUDE.md`, like pi)
//! and skills under the root (`.pi/skills/<name>/SKILL.md`, `.agents/skills/<name>/SKILL.md`).

use std::path::Path;

use controlroom_protocol::RemotePath;
use controlroom_protocol::host::{ContextFile, Skill};

/// Instruction file names, by precedence within one directory.
const INSTRUCTION_FILES: [&str; 3] = ["AGENTS.override.md", "AGENTS.md", "CLAUDE.md"];
/// Where skills live, relative to the root.
const SKILL_DIRS: [&str; 2] = [".pi/skills", ".agents/skills"];
/// Largest instruction file included (larger ones are cut).
const MAX_FILE_BYTES: usize = 256 * 1024;

/// The instruction files for `root`, outermost first.
pub fn context_files(root: &Path) -> Vec<ContextFile> {
    let mut dirs: Vec<&Path> = root.ancestors().collect();
    dirs.reverse();
    dirs.into_iter()
        .filter_map(|dir| {
            INSTRUCTION_FILES.iter().find_map(|name| {
                let path = dir.join(name);
                let bytes = std::fs::read(&path).ok()?;
                let cut = &bytes[..bytes.len().min(MAX_FILE_BYTES)];
                Some(ContextFile {
                    path: RemotePath::from(path.to_string_lossy().as_ref()),
                    content: String::from_utf8_lossy(cut).into_owned(),
                })
            })
        })
        .collect()
}

/// The skills under `root`, by name (the first found wins).
pub fn skills(root: &Path) -> Vec<Skill> {
    let mut found: Vec<Skill> = Vec::new();
    for dir in SKILL_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path().join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let (name, description) = front_matter(&text);
            let name =
                name.unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            if found.iter().any(|s| s.name == name) {
                continue;
            }
            found.push(Skill {
                name,
                description: description.unwrap_or_default(),
                path: RemotePath::from(file.to_string_lossy().as_ref()),
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// `name` and `description` from a `---` front matter block (single-line values).
fn front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).to_owned();
        match key.trim() {
            "name" if !value.is_empty() => name = Some(value),
            "description" if !value.is_empty() => description = Some(value),
            _ => {}
        }
    }
    (name, description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_files_outermost_first_one_per_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src/api");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "top").unwrap();
        std::fs::write(dir.path().join("src/CLAUDE.md"), "claude").unwrap();
        std::fs::write(root.join("AGENTS.md"), "plain").unwrap();
        std::fs::write(root.join("AGENTS.override.md"), "override").unwrap();
        let files = context_files(&root);
        let found: Vec<_> = files
            .iter()
            .filter(|f| f.path.as_str().starts_with(dir.path().to_str().unwrap()))
            .map(|f| f.content.as_str())
            .collect();
        assert_eq!(found, ["top", "claude", "override"]);
    }

    #[test]
    fn skills_from_front_matter() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join(".pi/skills/deploy");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: deploy-app\ndescription: \"Ship it\"\n---\nbody",
        )
        .unwrap();
        let other = dir.path().join(".agents/skills/lint");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("SKILL.md"), "no front matter").unwrap();
        let skills = skills(dir.path());
        let names: Vec<_> = skills
            .iter()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect();
        assert_eq!(names, [("deploy-app", "Ship it"), ("lint", "")]);
    }
}
