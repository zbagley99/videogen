//! User and local configuration (`videogen.ron`, `~/.config/videogen/config.ron`,
//! `$VG_CONFIG`), plus the safe built-in defaults.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_LLM: &str = "http://localhost:8080/v1";
/// Where the pose library is looked for when nothing names one.
pub const DEFAULT_POSES: &str = "avatar/poses.ron";
pub const DEFAULT_WHISPER_MODEL: &str = "large-v3-turbo";
pub const DEFAULT_FILLERS: &str = "um, uh, uhm, erm, er, ah, hmm, mm";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Default OpenAI-compatible server for `vg suggest` and new GUI profiles.
    pub llm_url: String,
    /// Server endpoints initially listed in the Suggest window.
    pub llm_endpoints: Vec<String>,
    /// Default pose library path when none is passed or set by a project.
    pub poses: String,
    /// Default Whisper model for transcription.
    pub whisper_model: String,
    /// Default comma-separated filler words for `vg transcripts fillers`.
    pub fillers: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            llm_url: DEFAULT_LLM.into(),
            llm_endpoints: vec![DEFAULT_LLM.into()],
            poses: DEFAULT_POSES.into(),
            whisper_model: DEFAULT_WHISPER_MODEL.into(),
            fillers: DEFAULT_FILLERS.into(),
        }
    }
}

impl Config {
    /// Load configuration from `$VG_CONFIG`, `./videogen.ron`, or
    /// `$XDG_CONFIG_HOME/videogen/config.ron` (`~/.config/videogen/config.ron`),
    /// falling back to defaults. `$VG_LLM` overrides `llm_url`.
    pub fn load() -> Self {
        let mut cfg = Self::candidates()
            .into_iter()
            .find(|p| p.is_file())
            .and_then(|p| match Self::from_file(&p) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("warning: ignoring {}: {e}", p.display());
                    None
                }
            })
            .unwrap_or_default();
        if let Ok(url) = std::env::var("VG_LLM") {
            let url = url.trim().to_string();
            if !url.is_empty() {
                cfg.llm_url = url;
            }
        }
        cfg.normalize();
        cfg
    }

    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut cfg: Config = ron::from_str(&text)?;
        cfg.normalize();
        Ok(cfg)
    }

    fn candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(p) = std::env::var("VG_CONFIG") {
            let p = p.trim();
            if !p.is_empty() {
                out.push(PathBuf::from(p));
            }
        }
        out.push(PathBuf::from("videogen.ron"));
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        {
            out.push(dir.join("videogen/config.ron"));
        }
        out
    }

    fn normalize(&mut self) {
        self.llm_url = self.llm_url.trim().trim_end_matches('/').to_string();
        if self.llm_url.is_empty() {
            self.llm_url = DEFAULT_LLM.into();
        }
        let mut endpoints = Vec::new();
        for u in std::mem::take(&mut self.llm_endpoints) {
            let u = u.trim().trim_end_matches('/').to_string();
            if !u.is_empty() && !endpoints.contains(&u) {
                endpoints.push(u);
            }
        }
        if !endpoints.contains(&self.llm_url) {
            endpoints.insert(0, self.llm_url.clone());
        }
        self.llm_endpoints = endpoints;
        if self.poses.trim().is_empty() {
            self.poses = DEFAULT_POSES.into();
        }
        if self.whisper_model.trim().is_empty() {
            self.whisper_model = DEFAULT_WHISPER_MODEL.into();
        }
        if self.fillers.trim().is_empty() {
            self.fillers = DEFAULT_FILLERS.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_ron_fills_defaults_and_normalizes_endpoints() {
        let mut cfg: Config = ron::from_str(
            r#"(
                llm_url: "http://example.local:9000/v1/",
                llm_endpoints: ["http://example.local:9001/v1/"],
            )"#,
        )
        .unwrap();
        cfg.normalize();
        assert_eq!(cfg.llm_url, "http://example.local:9000/v1");
        assert_eq!(
            cfg.llm_endpoints,
            vec![
                "http://example.local:9000/v1".to_string(),
                "http://example.local:9001/v1".to_string(),
            ]
        );
        assert_eq!(cfg.poses, DEFAULT_POSES);
        assert_eq!(cfg.whisper_model, DEFAULT_WHISPER_MODEL);
        assert_eq!(cfg.fillers, DEFAULT_FILLERS);
    }
}
