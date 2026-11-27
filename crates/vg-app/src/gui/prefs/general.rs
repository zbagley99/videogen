//! General preferences: recording, transcription, suggestions, playback, editing.

use super::super::{Prefs, VgApp, theme};
use super::super::theme::weak;
use super::super::util::short;
use super::{Action, Confirm, heading};
use eframe::egui;

#[derive(Default)]
pub struct State {
    devices: Option<Vec<String>>,
}

pub fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let grid = |ui: &mut egui::Ui, id: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Grid::new(id).num_columns(2).spacing([16.0, 8.0]).min_col_width(150.0).show(ui, |ui| add(ui));
        };
        let p = &mut app.prefs;

        heading(ui, "Recording");
        let devices = app.prefs_win.general.devices.get_or_insert_with(|| crate::recorder::input_names().unwrap_or_default());
        grid(ui, "g-rec", &mut |ui| {
            ui.label("Input");
            let current = p.record_device.clone().unwrap_or_else(|| "System default".into());
            egui::ComboBox::from_id_salt("prefs-input").width(300.0).selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut p.record_device, None, "System default");
                for d in devices.iter() {
                    ui.selectable_value(&mut p.record_device, Some(d.clone()), d);
                }
            });
            ui.end_row();
            ui.label("Countdown");
            theme::segmented(ui, &mut p.record_countdown, &[(false, "Off"), (true, "3 s")]);
            ui.end_row();
            ui.label("After a take");
            ui.checkbox(&mut p.record_transcribe, "transcribe it into a new script");
            ui.end_row();
        });

        heading(ui, "Transcription");
        grid(ui, "g-tr", &mut |ui| {
            ui.label("Whisper model");
            egui::ComboBox::from_id_salt("prefs-model").width(300.0).selected_text(&p.transcribe_model).show_ui(ui, |ui| {
                for (m, note) in super::super::transcribe::MODELS {
                    ui.selectable_value(&mut p.transcribe_model, m.to_string(), format!("{m}  ({note})"));
                }
            });
            ui.end_row();
            ui.label("Language");
            ui.add(egui::TextEdit::singleline(&mut p.transcribe_language).hint_text("auto (or en, de, …)").desired_width(140.0));
            ui.end_row();
            ui.label("Pose tags");
            let mut mode = match (p.transcribe_autotag, p.transcribe_llm) {
                (false, _) => 0,
                (true, false) => 1,
                (true, true) => 2,
            };
            if theme::segmented(ui, &mut mode, &[(0, "None"), (1, "Rules"), (2, "LLM")]) {
                p.transcribe_autotag = mode > 0;
                p.transcribe_llm = mode == 2;
            }
            ui.end_row();
        });

        heading(ui, "Pose suggestions");
        grid(ui, "g-llm", &mut |ui| {
            ui.label("Server");
            ui.vertical(|ui| {
                for url in p.llm_endpoints.clone() {
                    ui.radio_value(&mut p.llm_url, url.clone(), egui::RichText::new(&url).monospace().size(11.5));
                }
            });
            ui.end_row();
        });
        weak(ui, "Add or remove servers in the Suggest window (Ctrl+G).");

        heading(ui, "Playback");
        grid(ui, "g-play", &mut |ui| {
            ui.label("Volume");
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut p.volume, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0} %", v * 100.0)));
                ui.checkbox(&mut p.muted, "muted");
            });
            ui.end_row();
        });

        heading(ui, "Editing");
        grid(ui, "g-edit", &mut |ui| {
            ui.label("Timeline");
            ui.checkbox(&mut p.snap_timeline, "snap drags to words, lines, poses and the playhead");
            ui.end_row();
            ui.label("Canvas");
            ui.horizontal(|ui| {
                ui.checkbox(&mut p.snap_canvas, "show the grid and snap to it");
                ui.add(egui::DragValue::new(&mut p.grid_divs).range(2..=48).suffix(" cells"));
            });
            ui.end_row();
            ui.label("Shorts");
            ui.checkbox(&mut p.show_guides, "shade the areas YouTube's Shorts UI covers");
            ui.end_row();
        });

        ui.add_space(14.0);
        ui.separator();
        ui.horizontal(|ui| {
            if theme::tool_button(ui, "Reset preferences", None, false).clicked() {
                app.prefs_win.confirm = Some(Confirm {
                    title: "Reset preferences".into(),
                    body: "Everything on this page goes back to its default. Recent projects, folders, pinned transcripts, the transcript limit and your LLM servers are kept.".into(),
                    files: vec![],
                    ok: "Reset".into(),
                    action: Action::ResetPrefs,
                });
            }
            if let Some(dir) = eframe::storage_dir("videogen") {
                weak(ui, format!("Saved in {}", short(&dir.join("app.ron"))));
            }
        });
    });
}

pub fn reset(app: &mut VgApp) {
    let old = std::mem::take(&mut app.prefs);
    app.prefs = Prefs {
        recent_projects: old.recent_projects,
        projects_dir: old.projects_dir,
        transcript_dirs: old.transcript_dirs,
        transcript_keep: old.transcript_keep,
        transcript_limit: old.transcript_limit,
        llm_endpoints: old.llm_endpoints,
        llm_url: old.llm_url,
        view: old.view,
        ..Prefs::default()
    };
    app.info("preferences reset");
}
