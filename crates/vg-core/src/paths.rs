//! Path helpers shared by the project files, the pose library and the GUI.

use std::path::{Path, PathBuf};

/// `p` canonicalized, or as given when that fails (e.g. it doesn't exist).
pub fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `path` relative to `base` if it's inside it (or one level up), else absolute.
pub fn relative_path(path: &Path, base: &Path) -> PathBuf {
    // A file that doesn't exist yet (a new words file) resolves through its folder.
    let abs = |p: &Path| {
        std::fs::canonicalize(p)
            .ok()
            .or_else(|| {
                let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
                Some(std::fs::canonicalize(dir).ok()?.join(p.file_name()?))
            })
            .unwrap_or_else(|| p.to_path_buf())
    };
    let (p, b) = (abs(path), abs(base));
    if let Ok(r) = p.strip_prefix(&b) {
        return r.to_path_buf();
    }
    // One level up is common (avatar/poses.ron -> character-art/x.png).
    if let Some(parent) = b.parent()
        && let Ok(r) = p.strip_prefix(parent) {
            return Path::new("..").join(r);
        }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_handles_files_that_dont_exist_yet() {
        // Relative to the working directory (the crate's folder under `cargo test`).
        assert_eq!(relative_path(Path::new("src/new.words.json"), Path::new("src")), PathBuf::from("new.words.json"));
        assert_eq!(relative_path(Path::new("src/lib.rs"), Path::new("src")), PathBuf::from("lib.rs"));
        assert_eq!(relative_path(Path::new("Cargo.toml"), Path::new("src")), PathBuf::from("../Cargo.toml"));
    }
}
