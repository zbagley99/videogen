//! The layout & motion window: edits the project's preset live.

use super::{VgApp, place, theme};
use eframe::egui;
use vg_core::{Motion, Preset};

pub fn window(app: &mut VgApp, ctx: &egui::Context) {
    let mut open = app.settings_open;
    let mut dialog = None;
    let pos = if open { app.place.place(ctx, "settings", egui::Align2::LEFT_TOP, egui::vec2(370.0, 640.0)) } else { None };
    place::at(egui::Window::new(theme::caption("Layout · motion · captions").color(theme::WHITE)), "settings", pos).open(&mut open).resizable(false).collapsible(false).default_width(340.0).show(ctx, |ui| {
        let preset = &mut app.project.preset;
        ui.horizontal(|ui| {
            ui.label(theme::caption("Name"));
            ui.text_edit_singleline(&mut preset.name);
        });
        egui::CollapsingHeader::new(theme::caption("Canvas").color(theme::WHITE)).default_open(true).show(ui, |ui| {
            egui::Grid::new("canvas").num_columns(2).show(ui, |ui| {
                ui.label(theme::caption("Size"));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut preset.width).range(16..=7680).suffix(" px"));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut preset.height).range(16..=7680).suffix(" px"));
                });
                ui.end_row();
                ui.label(theme::caption("Frame rate"));
                ui.horizontal(|ui| {
                    for fps in [24, 30, 60] {
                        ui.selectable_value(&mut preset.fps, fps, fps.to_string());
                    }
                });
                ui.end_row();
            });
        });
        egui::CollapsingHeader::new(theme::caption("Avatar placement").color(theme::WHITE)).default_open(true).show(ui, |ui| {
            let a = &mut preset.avatar;
            theme::note(ui, "Each pose has its own spot: dragging or scrolling on the canvas moves and sizes the pose on screen only (Shift: all poses). These set the default spot, for poses you haven't placed.");
            egui::Grid::new("layout").num_columns(2).show(ui, |ui| {
                ui.label(theme::caption("Horizontal"));
                ui.add(egui::Slider::new(&mut a.x, 0.0..=1.0).fixed_decimals(3));
                ui.end_row();
                ui.label(theme::caption("Vertical")).on_hover_text("Where the bottom of the body sits (1 = bottom edge)");
                ui.add(egui::Slider::new(&mut a.y, 0.0..=1.3).fixed_decimals(3));
                ui.end_row();
                ui.label(theme::caption("Size")).on_hover_text("Pose image height as a fraction of the canvas height");
                ui.add(egui::Slider::new(&mut a.height, 0.05..=1.5).fixed_decimals(3));
                ui.end_row();
                ui.label(theme::caption("Mirror"));
                ui.checkbox(&mut a.flip, "flip horizontally");
                ui.end_row();
            });
            ui.horizontal(|ui| {
                theme::note(ui, "Snap to:");
                for (label, x) in [("left", 0.25), ("center", 0.5), ("right", 0.75)] {
                    if theme::tool_button(ui, label, None, false).clicked() {
                        a.x = x;
                        a.y = 1.0;
                    }
                }
            });
            if !a.poses.is_empty() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(theme::caption(format!("Placed on their own · {}", a.poses.len())));
                    if theme::tool_button(ui, "Reset all", None, false).clicked() {
                        a.poses.clear();
                    }
                });
                let mut reset = None;
                for (tag, s) in &a.poses {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tag.to_uppercase()).monospace().size(11.0).color(theme::MARK));
                        ui.label(egui::RichText::new(format!("x {:.2} · y {:.2} · size {:.2}", s.x, s.y, s.height)).size(11.0).color(theme::TEXT2));
                        if theme::tool_button(ui, "Reset", None, false).clicked() {
                            reset = Some(tag.clone());
                        }
                    });
                }
                if let Some(t) = reset {
                    a.poses.remove(&t);
                }
            }
        });
        egui::CollapsingHeader::new(theme::caption("Motion").color(theme::WHITE)).default_open(true).show(ui, |ui| {
            let m = &mut preset.motion;
            egui::Grid::new("motion").num_columns(2).show(ui, |ui| {
                ui.label(theme::caption("Pop on switch")).on_hover_text("Squash depth when the pose changes (0 = hard cut)");
                ui.add(egui::Slider::new(&mut m.pop_amount, 0.0..=0.4).fixed_decimals(2));
                ui.end_row();
                ui.label(theme::caption("Pop bounciness")).on_hover_text("Spring frequency, Hz");
                ui.add(egui::Slider::new(&mut m.pop_freq, 1.0..=15.0).fixed_decimals(1));
                ui.end_row();
                ui.label(theme::caption("Pop settle")).on_hover_text("Higher settles faster");
                ui.add(egui::Slider::new(&mut m.pop_damping, 4.0..=40.0).fixed_decimals(0));
                ui.end_row();
                ui.label(theme::caption("Idle bob"));
                ui.add(egui::Slider::new(&mut m.bob_amount, 0.0..=0.03).fixed_decimals(3));
                ui.end_row();
                ui.label(theme::caption("Breathing"));
                ui.add(egui::Slider::new(&mut m.breathe_amount, 0.0..=0.03).fixed_decimals(3));
                ui.end_row();
                ui.label(theme::caption("Idle period"));
                ui.add(egui::Slider::new(&mut m.bob_period, 0.5..=6.0).suffix(" s").fixed_decimals(1));
                ui.end_row();
                ui.label(theme::caption("Hide time"));
                ui.add(egui::Slider::new(&mut m.hide_time, 0.02..=0.6).suffix(" s").fixed_decimals(2));
                ui.end_row();
            });
            if theme::tool_button(ui, "Default motion", None, false).clicked() {
                *m = Motion::default();
            }
        });
        egui::CollapsingHeader::new(theme::caption("Captions").color(theme::WHITE)).default_open(false).show(ui, |ui| {
            let c = &mut preset.captions;
            ui.checkbox(&mut c.enabled, "Word-by-word captions (.ass next to the export; burned into the preview MP4)");
            ui.add_enabled_ui(c.enabled, |ui| {
                egui::Grid::new("captions").num_columns(2).show(ui, |ui| {
                    ui.label(theme::caption("Font"));
                    ui.text_edit_singleline(&mut c.font);
                    ui.end_row();
                    ui.label(theme::caption("Size"));
                    ui.add(egui::Slider::new(&mut c.size, 0.01..=0.2).fixed_decimals(3));
                    ui.end_row();
                    ui.label(theme::caption("Horizontal"));
                    ui.add(egui::Slider::new(&mut c.x, 0.0..=1.0).fixed_decimals(3));
                    ui.end_row();
                    ui.label(theme::caption("Vertical"));
                    ui.add(egui::Slider::new(&mut c.y, 0.0..=1.0).fixed_decimals(3));
                    ui.end_row();
                    ui.label(theme::caption("Wrap width"));
                    ui.add(egui::Slider::new(&mut c.width, 0.1..=1.0).fixed_decimals(2));
                    ui.end_row();
                    ui.label(theme::caption("Words at once"));
                    ui.add(egui::Slider::new(&mut c.max_words, 1..=12));
                    ui.end_row();
                    ui.label(theme::caption("Outline"));
                    ui.add(egui::Slider::new(&mut c.outline, 0.0..=0.25).fixed_decimals(2));
                    ui.end_row();
                    ui.label(theme::caption("Pop"));
                    ui.add(egui::Slider::new(&mut c.pop, 0.0..=0.4).fixed_decimals(2));
                    ui.end_row();
                    ui.label(theme::caption("Colours"));
                    ui.horizontal(|ui| {
                        theme::color_u32(ui, &mut c.color);
                        ui.label("text");
                        theme::color_u32(ui, &mut c.highlight);
                        ui.label("spoken word");
                    });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut c.uppercase, "UPPERCASE");
                    ui.end_row();
                });
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            if theme::tool_button(ui, "Reset to built-in", None, false).on_hover_text("Restore the built-in preset with this name (shorts/long)").clicked()
                && let Some(p) = Preset::builtin().into_iter().find(|p| p.name == preset.name) {
                    *preset = p;
                }
            if theme::tool_button(ui, "Save preset", None, false).clicked() {
                dialog = Some((super::dialog::Want::PresetSave, rfd::FileDialog::new().add_filter("preset", &["ron"]).set_file_name(format!("{}.ron", preset.name)), super::dialog::How::Save));
            }
            if theme::tool_button(ui, "Load preset", None, false).clicked() {
                dialog = Some((super::dialog::Want::PresetLoad, rfd::FileDialog::new().add_filter("preset", &["ron"]), super::dialog::How::File));
            }
        });
    });
    app.settings_open = open;
    if let Some((want, d, how)) = dialog {
        super::dialog::open(app, want, d, how);
    }
}
