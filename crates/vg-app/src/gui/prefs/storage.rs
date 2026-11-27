//! Storage & caches: what videogen keeps in memory and on disk, and clearing it.

use super::super::{VgApp, theme};
use super::super::theme::weak;
use super::super::util::{human_bytes, short};
use super::{Action, Confirm, heading};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub struct State {
    pub rescan: bool,
    /// Sizes of the folders on disk, measured in the background.
    sizes: Vec<(Item, Option<u64>)>,
    rx: Option<mpsc::Receiver<(usize, u64)>>,
}

impl Default for State {
    fn default() -> Self {
        State { rescan: true, sizes: vec![], rx: None }
    }
}

#[derive(Clone)]
struct Item {
    name: String,
    what: String,
    path: PathBuf,
}

/// The Hugging Face cache Whisper downloads its models into.
fn hf_hub() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HF_HUB_CACHE") {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("HF_HOME") {
        return Some(PathBuf::from(p).join("hub"));
    }
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("huggingface/hub"))
}

fn items() -> Vec<Item> {
    let mut out = vec![];
    if let Some(hub) = hf_hub()
        && let Ok(rd) = std::fs::read_dir(&hub) {
            let mut models: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("whisper"))).collect();
            models.sort();
            for m in models {
                let name = m.file_name().unwrap().to_string_lossy().trim_start_matches("models--").replace("--", "/");
                out.push(Item { name: format!("Whisper model · {name}"), what: "Downloaded again the next time you transcribe with it.".into(), path: m });
            }
        }
    if let Ok(dir) = vg_project::find_sidecar() {
        out.push(Item { name: "Transcriber environment".into(), what: "Python and faster-whisper for transcribing. uv rebuilds it on the next transcription (a large download).".into(), path: dir.join(".venv") });
    }
    out
}

/// Every file under `p`, with symlinks resolved to what they point at (Hugging
/// Face keeps a model's files in a shared `blobs` folder and links to them).
fn files_under(p: &Path, out: &mut std::collections::BTreeSet<PathBuf>) {
    let Ok(meta) = std::fs::symlink_metadata(p) else { return };
    if meta.is_symlink() {
        if let Ok(t) = std::fs::canonicalize(p)
            && t.is_file() {
                out.insert(t);
            }
    } else if meta.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                files_under(&e.path(), out);
            }
        }
    } else if meta.is_file() {
        out.insert(p.to_path_buf());
    }
}

fn dir_size(p: &Path) -> u64 {
    let mut files = std::collections::BTreeSet::new();
    files_under(p, &mut files);
    files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum()
}

/// Delete a folder and the files its links point at (when they're in the same
/// cache, e.g. a Whisper model's blobs), so the space really comes back.
pub fn delete_cache_dir(dir: &Path) -> std::io::Result<()> {
    let mut files = std::collections::BTreeSet::new();
    files_under(dir, &mut files);
    let root = dir.parent().and_then(|p| std::fs::canonicalize(p).ok());
    // Files other entries of the cache also link to stay.
    let mut shared = std::collections::BTreeSet::new();
    if let Some(rd) = dir.parent().and_then(|p| std::fs::read_dir(p).ok()) {
        for e in rd.flatten() {
            let p = e.path();
            if p != dir && p.is_dir() && p.file_name().is_some_and(|n| n != "blobs") {
                files_under(&p, &mut shared);
            }
        }
    }
    std::fs::remove_dir_all(dir)?;
    for f in files {
        if root.as_ref().is_some_and(|r| f.starts_with(r)) && !shared.contains(&f) && f.exists() {
            std::fs::remove_file(&f)?;
        }
    }
    Ok(())
}

fn rescan(app: &mut VgApp, ctx: &egui::Context) {
    let st = &mut app.prefs_win.storage;
    let items = items();
    st.sizes = items.iter().map(|i| (i.clone(), None)).collect();
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        for (k, i) in items.iter().enumerate() {
            if tx.send((k, dir_size(&i.path))).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    st.rx = Some(rx);
    st.rescan = false;
}

pub fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    if app.prefs_win.storage.rescan {
        rescan(app, &ctx);
    }
    if let Some(rx) = &app.prefs_win.storage.rx {
        while let Ok((k, b)) = rx.try_recv() {
            if let Some(s) = app.prefs_win.storage.sizes.get_mut(k) {
                s.1 = Some(b);
            }
        }
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| body(app, ui, &ctx));
}

/// One row: name and path on the left, size and a button on the right, a note under.
fn row(ui: &mut egui::Ui, name: &str, path: Option<&Path>, size: &str, note: &str, button: Option<(&str, bool)>) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(name).color(theme::WHITE));
            if let Some(p) = path {
                ui.label(egui::RichText::new(short(p)).monospace().size(10.5).color(theme::MUTED));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some((label, enabled)) = button {
                clicked = theme::tool_button_enabled(ui, label, None, false, enabled).clicked();
            }
            ui.label(egui::RichText::new(size).monospace().size(11.5));
        });
    });
    weak(ui, note);
    ui.add_space(6.0);
    clicked
}

fn body(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    heading(ui, "Caches");
    let render = app.studio.cache_bytes();
    row(ui, "Audio render cache", None, &render.map(|b| human_bytes(b as u64)).unwrap_or_else(|| "in use".into()), "Decoded recordings and each effect's output, in memory, so tweaks in the Audio tab re-render quickly. Rebuilt as needed.", None);
    row(ui, "Pose images", None, &format!("{} poses", app.poses.poses.len()), "Thumbnails and preview textures, reloaded from the pose files.", None);
    ui.add_space(4.0);
    if theme::tool_button(ui, "Clear caches", None, false).clicked() {
        clear(app, ctx);
    }
    weak(ui, "Frees the memory above and reloads the pose images, narration and word timings from disk. Nothing you made is touched.");

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        heading(ui, "Downloads on disk");
        if theme::tool_button(ui, "Measure again", None, false).clicked() {
            app.prefs_win.storage.rescan = true;
        }
    });
    let mut ask = None;
    for (item, size) in &app.prefs_win.storage.sizes {
        let exists = item.path.exists();
        let s = if !exists { "not there".into() } else { size.map(human_bytes).unwrap_or_else(|| "measuring…".into()) };
        if row(ui, &item.name, Some(&item.path), &s, &item.what, Some(("Delete", exists))) {
            ask = Some((item.clone(), *size));
        }
    }
    if app.prefs_win.storage.sizes.is_empty() {
        weak(ui, "No Whisper models or transcriber environment found.");
    }
    if let Some((item, size)) = ask {
        app.prefs_win.confirm = Some(Confirm {
            title: format!("Delete {}", item.name),
            body: format!("{} ({}) is deleted for good, not moved to the trash. {}", short(&item.path), size.map(human_bytes).unwrap_or_else(|| "size unknown".into()), item.what),
            files: vec![],
            ok: "Delete".into(),
            action: Action::DeleteDir(item.path),
        });
    }
    if app.transcribe.busy() {
        weak(ui, "A transcription is running: don't delete its model or environment until it's done.");
    }
}

fn clear(app: &mut VgApp, ctx: &egui::Context) {
    let freed = app.studio.cache_bytes().unwrap_or(0);
    if !app.studio.clear_cache() {
        return app.error("the Audio tab is rendering; try again in a moment");
    }
    if let Some(p) = app.poses_path.clone()
        && !app.poses_dirty
            && let Err(e) = app.load_poses(ctx, &p) {
                app.error(format!("{e:#}"));
                return;
            }
    // Forget the loaded narration and word timings so they're read again.
    app.audio = None;
    app.player = None;
    if !app.words_dirty {
        app.words = None;
    }
    app.reparse();
    app.info(format!("caches cleared ({} freed)", human_bytes(freed as u64)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_a_model_frees_its_blobs_but_not_shared_ones() {
        let hub = std::env::temp_dir().join(format!("vg-hub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&hub);
        std::fs::create_dir_all(hub.join("blobs")).unwrap();
        for (b, n) in [("own", 1000), ("shared", 10), ("other", 5)] {
            std::fs::write(hub.join("blobs").join(b), vec![0u8; n]).unwrap();
        }
        for (m, blobs) in [("models--a--whisper", &["own", "shared"][..]), ("models--b--whisper", &["shared", "other"][..])] {
            let snap = hub.join(m).join("snapshots/x");
            std::fs::create_dir_all(&snap).unwrap();
            for b in blobs {
                std::os::unix::fs::symlink(format!("../../../blobs/{b}"), snap.join(b)).unwrap();
            }
        }
        assert_eq!(dir_size(&hub.join("models--a--whisper")), 1010);
        delete_cache_dir(&hub.join("models--a--whisper")).unwrap();
        assert!(!hub.join("blobs/own").exists() && hub.join("blobs/shared").exists() && hub.join("blobs/other").exists());
        assert_eq!(dir_size(&hub.join("models--b--whisper")), 15);
        std::fs::remove_dir_all(&hub).unwrap();
    }
}
