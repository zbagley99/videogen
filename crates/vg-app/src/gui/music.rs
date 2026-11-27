//! Background music for the Audio tab: from a file, or downloaded from a link
//! (YouTube and the many sites yt-dlp knows) into the project's `music/` folder.
//! It becomes a music track: looped to the narration's end, faded and ducked,
//! and editable like any other track.

use super::{VgApp, theme};
use eframe::egui;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct MusicPanel {
    link: String,
    task: Option<Download>,
}

impl MusicPanel {
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
}

struct Download {
    handle: std::thread::JoinHandle<anyhow::Result<Option<PathBuf>>>,
    /// Percent done and the latest status line.
    progress: Arc<Mutex<(f32, String)>>,
    cancel: Arc<AtomicBool>,
}

/// Where downloaded music goes.
fn music_dir(app: &VgApp) -> PathBuf {
    app.cue_dir().join("music")
}

/// Download the audio of `url` into `dir` with yt-dlp. Returns the file.
fn download(url: &str, dir: &Path, progress: &Mutex<(f32, String)>, cancel: &AtomicBool) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context;
    std::fs::create_dir_all(dir)?;
    let mut child = std::process::Command::new("yt-dlp")
        .args(["-f", "bestaudio/best", "-x", "--no-playlist", "--restrict-filenames", "--newline", "--progress", "--progress-template", "download:%(progress._percent_str)s", "--print", "after_move:filepath", "-o"])
        .arg(dir.join("%(title).80s.%(ext)s"))
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("running yt-dlp (install it, e.g. `pacman -S yt-dlp`)")?;
    let stderr = child.stderr.take().unwrap();
    let err_tail = std::thread::spawn(move || std::io::BufReader::new(stderr).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).last().unwrap_or_default());
    let mut file = None;
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(p) = l.strip_suffix('%').and_then(|v| v.trim().parse::<f32>().ok()) {
            *progress.lock().unwrap() = (p, "downloading".into());
        } else if Path::new(l).is_file() {
            file = Some(PathBuf::from(l));
        } else {
            progress.lock().unwrap().1 = l.chars().take(80).collect();
        }
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("yt-dlp failed: {}", err_tail.join().unwrap_or_default());
    }
    file.map(Some).context("yt-dlp finished but didn't say where the file went")
}

/// Add `path` as a music track at the start of the project.
pub fn add(app: &mut VgApp, path: &Path) {
    let s = &mut app.studio;
    if s.mix.tracks.is_empty() {
        return app.error("open or record the narration first; music goes under it");
    }
    let rel = super::relative_path(path, &s.base());
    match vg_media::probe_duration(path) {
        Ok(dur) => {
            let name = path.file_stem().map(|n| n.to_string_lossy().replace('_', " ")).unwrap_or_else(|| "music".into());
            let mut t = vg_audio::Track::new(&name, rel, dur);
            t.music = Some(vg_audio::edit::Music::default());
            t.gain_db = -14.0;
            t.level = true;
            s.mix.tracks.push(t);
            s.track = s.mix.tracks.len() - 1;
            s.changed();
            app.info(format!("added {name} as background music (it loops to the end of the narration and ducks under your voice)"));
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}

fn poll(app: &mut VgApp) {
    if !app.music.task.as_ref().is_some_and(|t| t.handle.is_finished()) {
        return;
    }
    let t = app.music.task.take().unwrap();
    match t.handle.join() {
        Ok(Ok(Some(p))) => {
            app.music.link.clear();
            add(app, &p);
        }
        Ok(Ok(None)) => app.info("download cancelled"),
        Ok(Err(e)) => app.error(format!("{e:#}")),
        Err(_) => app.error("the download crashed"),
    }
}

/// The sidebar section.
pub fn section(app: &mut VgApp, ui: &mut egui::Ui) {
    poll(app);
    let has_mix = !app.studio.mix.tracks.is_empty();
    ui.horizontal(|ui| {
        if theme::tool_button_enabled(ui, "From a file", None, false, has_mix).clicked() {
            let d = rfd::FileDialog::new().add_filter("audio", &["wav", "flac", "mp3", "ogg", "m4a", "opus", "aac", "webm", "mp4"]).set_directory(app.cue_dir());
            super::dialog::open(app, super::dialog::Want::MusicFile, d, super::dialog::How::File);
        }
    });
    let dir = music_dir(app);
    match &app.music.task {
        Some(t) => {
            let (pct, status) = t.progress.lock().unwrap().clone();
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(pct / 100.0).desired_width(170.0).text(format!("{pct:.0} % · {status}")));
                if theme::tool_button(ui, "Cancel", None, false).clicked() {
                    t.cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        None => {
            ui.horizontal(|ui| {
                ui.add_enabled(has_mix, egui::TextEdit::singleline(&mut app.music.link).hint_text("YouTube link").desired_width(ui.available_width() - 96.0));
                let url = app.music.link.trim().to_string();
                if theme::tool_button_enabled(ui, "Download", None, false, has_mix && url.starts_with("http")).clicked() {
                    let progress = Arc::new(Mutex::new((0.0, "starting".to_string())));
                    let cancel = Arc::new(AtomicBool::new(false));
                    let (p, c, d, ctx) = (progress.clone(), cancel.clone(), dir.clone(), ui.ctx().clone());
                    let handle = std::thread::spawn(move || {
                        let r = download(&url, &d, &p, &c);
                        ctx.request_repaint();
                        r
                    });
                    app.music.task = Some(Download { handle, progress, cancel });
                }
            });
        }
    }
    theme::weak(ui, format!("Music goes under the narration from the start, loops to its end, fades, and dips while you talk. Links download into {}/. Use music you have the rights to.", super::util::short(&dir)));
}

/// The settings card for a music track (in the effects panel).
pub fn settings(m: &mut vg_audio::edit::Music, gain_db: &mut f32, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    egui::Grid::new("music-settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(theme::caption("Volume"));
        changed |= ui.add(egui::Slider::new(gain_db, -40.0..=0.0).suffix(" dB").fixed_decimals(1)).changed();
        ui.end_row();
        ui.label(theme::caption("Duck under voice"));
        changed |= ui.add(egui::Slider::new(&mut m.duck_db, 0.0..=24.0).suffix(" dB").fixed_decimals(1)).changed();
        ui.end_row();
        ui.label(theme::caption("Fade in"));
        changed |= ui.add(egui::Slider::new(&mut m.fade_in, 0.0..=10.0).suffix(" s").fixed_decimals(1)).changed();
        ui.end_row();
        ui.label(theme::caption("Fade out"));
        changed |= ui.add(egui::Slider::new(&mut m.fade_out, 0.0..=10.0).suffix(" s").fixed_decimals(1)).changed();
        ui.end_row();
        ui.label(theme::caption("Length"));
        changed |= theme::segmented(ui, &mut m.loop_it, &[(true, "Loop to the end"), (false, "Play once")]);
        ui.end_row();
    });
    changed
}
