//! The export window: pick a format and output, render on a worker thread.

use super::util::{file_name, plural};
use super::{VgApp, place, theme};
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use vg_media::OutputFormat;
use vg_project::{ExportJob, resolve};

#[derive(Default)]
pub struct ExportPanel {
    pub open: bool,
    task: Option<Task>,
    last_output: Option<PathBuf>,
}

struct Task {
    handle: std::thread::JoinHandle<anyhow::Result<Option<PathBuf>>>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    started: Instant,
}

pub fn window(app: &mut VgApp, ctx: &egui::Context) {
    finish_task(app);
    if !app.export.open {
        return;
    }
    let mut open = app.export.open;
    let mut choose = None;
    let pos = app.place.place(ctx, "export", egui::Align2::LEFT_BOTTOM, egui::vec2(430.0, 360.0));
    place::at(egui::Window::new(theme::caption("Export").color(theme::WHITE)), "export", pos).open(&mut open).resizable(false).collapsible(false).default_width(400.0).show(ctx, |ui| {
        let busy = app.export.task.is_some();
        let base = app.base();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("export").num_columns(2).show(ui, |ui| {
                ui.label(theme::caption("Format"));
                let ex = &mut app.project.export;
                egui::ComboBox::from_id_salt("format").selected_text(ex.format.label()).show_ui(ui, |ui| {
                    for f in OutputFormat::ALL {
                        if ui.selectable_label(ex.format == f, f.label()).clicked() && ex.format != f {
                            // Keep the folder, swap in the new format's default name.
                            let dir = ex.output.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("out"));
                            ex.format = f;
                            ex.output = dir.join(f.default_name());
                        }
                    }
                });
                ui.end_row();
                ui.label(theme::caption("Output"));
                ui.horizontal(|ui| {
                    ui.set_max_width(300.0);
                    let mut s = ex.output.to_string_lossy().to_string();
                    if ui.add(egui::TextEdit::singleline(&mut s).desired_width(220.0)).changed() {
                        ex.output = PathBuf::from(s);
                    }
                    if theme::tool_button(ui, "Choose", None, false).clicked() {
                        let dialog = rfd::FileDialog::new().set_directory(&base);
                        choose = Some(if ex.format == OutputFormat::PngSequence { (dialog, super::dialog::How::Folder) } else { (dialog.set_file_name(ex.format.default_name()), super::dialog::How::Save) });
                    }
                });
                ui.end_row();
                if ex.format == OutputFormat::PreviewMp4 {
                    ui.label(theme::caption("Background"));
                    theme::color_u32(ui, &mut ex.background);
                    ui.end_row();
                }
            });
            let p = &app.project.preset;
            theme::note(ui, format!("{}×{} · {} fps · {}", p.width, p.height, p.fps, vg_core::format_time(app.duration())));
            let clips = app.project.video.clips.len();
            theme::note(ui, match (app.project.export.format, clips > 0) {
                (OutputFormat::ProRes4444, _) => "Transparent. Drop it on a track above your footage in Kdenlive.",
                (OutputFormat::WebmAlpha, _) => "Transparent and small, but slow to encode.",
                (OutputFormat::PngSequence, _) => "Transparent PNG per frame.",
                (OutputFormat::PreviewMp4, false) => "Avatar over a solid colour, with the narration, for a quick review.",
                (OutputFormat::PreviewMp4, true) => "Avatar over the video track (the colour between clips), with the narration and the clips' sound, for a quick review.",
            });
            if clips > 0 && app.project.export.format != OutputFormat::PreviewMp4 {
                theme::note(ui, format!("Video track · not included ({clips} clip{}): this is the overlay alone", plural(clips)));
            }
            if app.project.preset.captions.enabled {
                let ass = resolve(&app.base(), &app.project.export.output).with_extension("ass");
                theme::note(ui, format!("Captions · {} (burned into the preview MP4)", file_name(&ass)));
            } else {
                theme::note(ui, "Captions · off (Layout › Captions)");
            }
        });
        ui.separator();
        match &app.export.task {
            Some(task) => {
                let (done, total) = (task.done.load(Ordering::Relaxed), task.total.load(Ordering::Relaxed).max(1));
                let frac = done as f32 / total as f32;
                let secs = task.started.elapsed().as_secs_f32();
                let eta = if done > 0 { secs / frac - secs } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).show_percentage().text(format!("frame {done}/{total} · {eta:.0}s left")));
                if theme::tool_button(ui, "Cancel", None, false).clicked() {
                    task.cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            None => {
                ui.horizontal(|ui| {
                    if theme::tool_button(ui, "Render", None, true).clicked() {
                        start(app, ctx);
                    }
                    if let Some(out) = app.export.last_output.clone() {
                        if theme::tool_button(ui, "Show in folder", None, false).clicked() {
                            let dir = if out.is_dir() { out.clone() } else { out.parent().map(PathBuf::from).unwrap_or_default() };
                            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                        }
                        theme::note(ui, file_name(&out));
                    }
                });
            }
        }
    });
    app.export.open = open;
    if let Some((d, how)) = choose {
        super::dialog::open(app, super::dialog::Want::ExportOutput, d, how);
    }
}

fn start(app: &mut VgApp, ctx: &egui::Context) {
    let Some(timeline) = app.parsed.timeline.clone() else { return };
    if !app.parsed.errors.is_empty() {
        app.error("fix the script errors before exporting");
        return;
    }
    let job = ExportJob {
        timeline,
        preset: app.project.preset.clone(),
        poses: app.poses.clone(),
        format: app.project.export.format,
        output: resolve(&app.base(), &app.project.export.output),
        audio: app.parsed.audio_path.clone(),
        background: app.project.export.background,
        words: app.words.as_ref().map(|w| w.2.clone()).unwrap_or_default(),
        video: vg_project::resolve_track(&app.base(), &app.project.video),
    };
    let done = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(1));
    let cancel = Arc::new(AtomicBool::new(false));
    let (device, queue) = (app.rs.device.clone(), app.rs.queue.clone());
    let (d, t, c, ctx2) = (done.clone(), total.clone(), cancel.clone(), ctx.clone());
    let handle = std::thread::spawn(move || {
        let r = vg_project::export(&device, &queue, &job, &c, |n, of| {
            d.store(n, Ordering::Relaxed);
            t.store(of, Ordering::Relaxed);
        });
        ctx2.request_repaint();
        r
    });
    app.export.task = Some(Task { handle, done, total, cancel, started: Instant::now() });
}

fn finish_task(app: &mut VgApp) {
    if !app.export.task.as_ref().is_some_and(|t| t.handle.is_finished()) {
        return;
    }
    let task = app.export.task.take().unwrap();
    let secs = task.started.elapsed().as_secs_f32();
    match task.handle.join() {
        Ok(Ok(Some(out))) => {
            app.info(format!("exported {} in {secs:.1}s", out.display()));
            app.export.last_output = Some(out);
        }
        Ok(Ok(None)) => app.info("export cancelled"),
        Ok(Err(e)) => app.error(format!("export failed: {e:#}")),
        Err(_) => app.error("export crashed"),
    }
}
