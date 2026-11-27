//! Projects on disk: finding `.vgproj` files, and creating, duplicating and
//! renaming them together with their scripts.

use crate::{ProjectFile, resolve, words_path};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use vg_core::paths::relative_path;

#[derive(Clone, Debug)]
pub struct Info {
    pub path: PathBuf,
    /// The script, resolved (it may not exist yet).
    pub script: Option<PathBuf>,
    pub modified: Option<SystemTime>,
    pub error: Option<String>,
}

impl Info {
    pub fn name(&self) -> String {
        self.path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }
}

pub fn info(path: &Path) -> Info {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    match ProjectFile::load(path) {
        Ok(pf) => Info { path: path.to_path_buf(), script: Some(resolve(path.parent().unwrap_or(Path::new(".")), &pf.cues)), modified, error: None },
        Err(e) => Info { path: path.to_path_buf(), script: None, modified, error: Some(format!("{e:#}")) },
    }
}

/// `.vgproj` files in `dir` and up to `depth` folders below it (skipping
/// hidden folders and build output).
pub fn scan(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if depth > 0 && !name.starts_with('.') && !matches!(name.as_str(), "target" | "node_modules") {
                out.extend(scan(&p, depth - 1));
            }
        } else if name.ends_with(".vgproj") {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn check_name(name: &str) -> Result<&str> {
    let n = name.trim().trim_end_matches(".vgproj");
    if n.is_empty() || n.contains(['/', '\\']) || n.starts_with('.') {
        bail!("give it a plain name, without folders");
    }
    Ok(n)
}

/// A new project in `dir/<name>/`: `<name>.vgproj` and the script `<name>.md`.
pub fn create(dir: &Path, name: &str, poses: &Path, preset: vg_core::Preset, script: &str) -> Result<PathBuf> {
    let n = check_name(name)?;
    let folder = dir.join(n);
    let path = folder.join(format!("{n}.vgproj"));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let cues = PathBuf::from(format!("{n}.md"));
    if !folder.join(&cues).exists() {
        std::fs::write(folder.join(&cues), script)?;
    }
    let pf = ProjectFile { poses: relative_path(poses, &folder), cues, preset, export: Default::default(), media: vec![], video: Default::default() };
    pf.save(&path)?;
    Ok(path)
}

/// A copy next to the original, named `new_name`. The script is copied too
/// (as `<new_name>.md`, next to the original script); narration and transcripts
/// are shared.
pub fn duplicate(path: &Path, new_name: &str) -> Result<PathBuf> {
    let n = check_name(new_name)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let new_path = dir.join(format!("{n}.vgproj"));
    if new_path.exists() {
        bail!("{} already exists", new_path.display());
    }
    let mut pf = ProjectFile::load(path)?;
    let script = resolve(dir, &pf.cues);
    if script.exists() {
        let new_script = script.with_file_name(format!("{n}.md"));
        if new_script.exists() {
            bail!("{} already exists", new_script.display());
        }
        std::fs::copy(&script, &new_script)?;
        pf.cues = relative_path(&new_script, dir);
    }
    pf.save(&new_path)?;
    Ok(new_path)
}

/// Rename a project file; with `script`, its script is renamed to match (and
/// the project pointed at it).
pub fn rename(path: &Path, new_name: &str, script: bool) -> Result<PathBuf> {
    let n = check_name(new_name)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let new_path = dir.join(format!("{n}.vgproj"));
    if new_path == path {
        return Ok(new_path);
    }
    if new_path.exists() {
        bail!("{} already exists", new_path.display());
    }
    let mut pf = ProjectFile::load(path)?;
    if script {
        let old = resolve(dir, &pf.cues);
        let new = old.with_file_name(format!("{n}.md"));
        if old.exists() && new != old {
            if new.exists() {
                bail!("{} already exists", new.display());
            }
            std::fs::rename(&old, &new)?;
            pf.cues = relative_path(&new, dir);
        }
    }
    pf.save(&new_path)?;
    std::fs::remove_file(path)?;
    Ok(new_path)
}

/// The files that belong to a project, for deleting it: the project, its
/// script and the script's words file (the ones that exist).
pub fn files(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if let Some(script) = info(path).script.filter(|s| s.exists()) {
        let dir = script.parent().unwrap_or(Path::new(".")).to_path_buf();
        let words = std::fs::read_to_string(&script).ok().and_then(|t| words_path(&t, &dir)).filter(|w| w.exists());
        out.push(script);
        out.extend(words);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_duplicate_rename() {
        let root = std::env::temp_dir().join(format!("vg-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("avatar")).unwrap();
        std::fs::write(root.join("avatar/poses.ron"), "(poses: [])").unwrap();
        let p = create(&root, "chess", &root.join("avatar/poses.ron"), vg_core::Preset::shorts(), "[0:00] hi\n").unwrap();
        assert_eq!(p, root.join("chess/chess.vgproj"));
        assert_eq!(ProjectFile::load(&p).unwrap().poses, PathBuf::from("../avatar/poses.ron"));
        assert!(create(&root, "chess", &root.join("avatar/poses.ron"), vg_core::Preset::shorts(), "").is_err());

        let d = duplicate(&p, "chess 2").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("chess/chess 2.md")).unwrap(), "[0:00] hi\n");
        assert_eq!(ProjectFile::load(&d).unwrap().cues, PathBuf::from("chess 2.md"));

        let r = rename(&d, "openings", true).unwrap();
        assert!(!d.exists() && r.exists() && root.join("chess/openings.md").exists() && !root.join("chess/chess 2.md").exists());
        assert_eq!(scan(&root, 2).len(), 2);
        assert_eq!(files(&r), vec![r.clone(), root.join("chess/openings.md")]);
        assert!(rename(&r, "a/b", false).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
