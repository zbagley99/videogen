//! Offscreen GUI screenshots (`vg screenshot`): runs the real UI on an egui
//! context with no window and renders it with egui-wgpu into a PNG. Used for
//! docs and for checking the UI without opening windows.

use super::{Prefs, VgApp};
use anyhow::{Context, Result};
use eframe::egui;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct ShotArgs {
    /// Project to open.
    project: Option<PathBuf>,
    #[arg(long, short, default_value = "docs/screenshot.png")]
    output: PathBuf,
    #[arg(long, default_value_t = 1600)]
    width: u32,
    #[arg(long, default_value_t = 1000)]
    height: u32,
    /// Playhead time in seconds.
    #[arg(long, default_value_t = 0.0)]
    time: f64,
    /// Show the layout & motion window.
    #[arg(long)]
    settings: bool,
    /// Select a pose (index) in the palette to show its inspector.
    #[arg(long)]
    pose: Option<usize>,
    /// Open windows, comma-separated: export, transcribe, record, recording
    /// (records a short take from the default input and discards it), recorded
    /// (keeps that take, stops it the way Stop does and prints the state), suggest,
    /// suggesting (also asks the LLM), preferences, projects, transcripts, storage
    /// (Settings pages), media (the sidebar's Media tab).
    #[arg(long, value_delimiter = ',')]
    open: Vec<String>,
    /// Turn the canvas grid on.
    #[arg(long)]
    grid: bool,
    /// Replay a mouse drag x0,y0,x1,y1 (window pixels) before capturing; prints resulting state.
    #[arg(long, value_delimiter = ',')]
    drag: Option<Vec<f32>>,
    /// Capture while the drag is still held.
    #[arg(long)]
    hold: bool,
    /// Hold Alt during the drag.
    #[arg(long)]
    alt: bool,
    /// Hold Ctrl during the drag.
    #[arg(long)]
    ctrl: bool,
    /// Hold Shift during the drag.
    #[arg(long)]
    shift: bool,
    /// Show a view: video (default) or audio.
    #[arg(long)]
    view: Option<String>,
    /// Answer every file dialog with these paths (comma-separated) instead of showing one.
    #[arg(long, value_delimiter = ',')]
    pick: Vec<PathBuf>,
    /// Steps after the drag, comma-separated: keys (`Delete`, `ctrl+z`, `b`),
    /// `click@x:y`, `rclick@x:y`, `type:text`, `wait` (for background renders),
    /// `scroll@x:y:dy`, `drag@x0:y0:x1:y1` (prefix `shift+` or `alt+` to hold a
    /// modifier; `holddrag@` captures before letting go), `media@PATH@x:y` (drops a
    /// Media card there), `hover-media@PATH@x:y` (holds it there) and `play:N`
    /// (plays silently for N frames at 60 Hz).
    #[arg(long, value_delimiter = ',')]
    keys: Vec<String>,
}

/// `ctrl+shift+z` → the key and its modifiers.
fn parse_key(spec: &str) -> Option<(egui::Key, egui::Modifiers)> {
    let mut mods = egui::Modifiers::NONE;
    let mut key = None;
    for part in spec.split('+') {
        match part.to_lowercase().as_str() {
            "ctrl" | "cmd" => mods |= egui::Modifiers::COMMAND,
            "shift" => mods |= egui::Modifiers::SHIFT,
            "alt" => mods |= egui::Modifiers::ALT,
            k => key = egui::Key::from_name(&if k.len() == 1 { k.to_uppercase() } else { part.to_string() }),
        }
    }
    Some((key?, mods))
}

/// A primary-button drag from `a` to `b` in 12 moves, let go of unless `hold`.
fn push_drag(script: &mut Vec<Vec<egui::Event>>, a: egui::Pos2, b: egui::Pos2, modifiers: egui::Modifiers, hold: bool) {
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers };
    script.push(vec![egui::Event::PointerMoved(a)]);
    script.push(vec![button(a, true)]);
    for k in 1..=12 {
        script.push(vec![egui::Event::PointerMoved(a + (b - a) * (k as f32 / 12.0))]);
    }
    if !hold {
        script.push(vec![button(b, false)]);
    }
}

pub fn screenshot(opts: &ShotArgs) -> Result<()> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .context("no GPU adapter")?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).context("no GPU device")?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
    let rs = egui_wgpu::RenderState {
        adapter,
        available_adapters: vec![],
        device: device.clone(),
        queue: queue.clone(),
        target_format: format,
        renderer: std::sync::Arc::new(egui::mutex::RwLock::new(renderer)),
    };

    let ctx = egui::Context::default();
    let prefs = Prefs::default();
    let mut app = VgApp::with_state(&ctx, rs.clone(), prefs, opts.project.clone());
    app.time = opts.time;
    // Never show a real file dialog from a capture.
    app.dialogs.fake = Some(opts.pick.clone());
    app.settings_open = opts.settings;
    app.palette_select(opts.pose);
    let open = |w: &str| opts.open.iter().any(|o| o == w);
    app.export.open = open("export");
    app.transcribe.open = open("transcribe");
    // `record` opens the window; `recording` also records a short real take
    // (from the default input) and throws it away after the capture; `recorded`
    // keeps it (stopping it the way the Stop button does) and captures the result.
    let keep = open("recorded");
    let recording = keep || open("recording");
    app.record.open = recording || open("record");
    app.prefs.snap_canvas = opts.grid;
    // `suggest` opens the pose suggestions window; `suggesting` also asks the LLM.
    app.assist.open = open("suggest") || open("suggesting");
    for (key, page) in [("preferences", super::prefs::Page::General), ("projects", super::prefs::Page::Projects), ("transcripts", super::prefs::Page::Transcripts), ("storage", super::prefs::Page::Storage)] {
        if open(key) {
            app.prefs_win.show_page(page);
        }
    }
    if open("suggesting") {
        super::assist::start(&mut app, &ctx, vg_llm::Mode::Untagged);
    }
    if open("media") {
        app.prefs.sidebar = super::media::Sidebar::Media;
    }
    if opts.view.as_deref() == Some("audio") {
        app.prefs.view = super::View::Audio;
    }

    let (w, h) = (opts.width, opts.height);
    let modifiers = egui::Modifiers { alt: opts.alt, ctrl: opts.ctrl, command: opts.ctrl, shift: opts.shift, ..Default::default() };
    let frame = std::cell::Cell::new(0u32);
    let raw = |events: Vec<egui::Event>, mods: Option<egui::Modifiers>| {
        frame.set(frame.get() + 1);
        egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32))),
            time: Some(frame.get() as f64 / 60.0),
            modifiers: mods.unwrap_or(modifiers),
            ..Default::default()
        }
    };
    // Frames: a few to settle layout (and let background renders finish), then
    // the drag and keys (if any), then a few more.
    let settle_frames = 4;
    let mut script: Vec<Vec<egui::Event>> = vec![vec![]; settle_frames];
    let drag: Option<[f32; 4]> = opts.drag.as_deref().and_then(|d| d.try_into().ok());
    if let Some([x0, y0, x1, y1]) = drag {
        push_drag(&mut script, egui::pos2(x0, y0), egui::pos2(x1, y1), modifiers, opts.hold);
        script.extend(vec![vec![]; 2]);
    }
    let mut waits = std::collections::HashSet::new();
    // Modifiers held and a drag-and-drop payload set, by frame.
    let mut frame_mods: std::collections::HashMap<usize, egui::Modifiers> = Default::default();
    let mut payloads: std::collections::HashMap<usize, PathBuf> = Default::default();
    let mut naps = std::collections::HashSet::new();
    let xy = |at: &str| -> Result<egui::Pos2> {
        let (x, y) = at.split_once(':').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))).context("x:y")?;
        Ok(egui::pos2(x, y))
    };
    // `play:N` plays (silently) for N real-time frames at 60 Hz and keeps playing.
    let mut plays = std::collections::HashMap::new();
    for k in &opts.keys {
        // `scroll@x:y:dy`: the wheel over x, y (the timeline zooms; smoothed over a few frames).
        if let Some(at) = k.strip_prefix("scroll@") {
            let v: Vec<f32> = at.split(':').filter_map(|n| n.parse().ok()).collect();
            anyhow::ensure!(v.len() == 3, "scroll@x:y:dy");
            let pos = egui::pos2(v[0], v[1]);
            script.push(vec![egui::Event::PointerMoved(pos)]);
            script.push(vec![egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, v[2]), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE }]);
            script.extend(vec![vec![]; 20]);
            continue;
        }
        let click = k.strip_prefix("click@").map(|r| (r, egui::PointerButton::Primary)).or(k.strip_prefix("rclick@").map(|r| (r, egui::PointerButton::Secondary)));
        if let Some((at, button)) = click {
            let pos = xy(at)?;
            let press = |pressed| egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE };
            script.push(vec![egui::Event::PointerMoved(pos)]);
            script.push(vec![press(true)]);
            script.push(vec![press(false)]);
            script.push(vec![]);
            continue;
        }
        // `[shift+|alt+]drag@…`, or `holddrag@…` to capture before letting go (last step).
        if let Some((pre, rest)) = k.split_once("drag@") {
            let hold = pre.ends_with("hold");
            let m = pre.trim_end_matches("hold").trim_end_matches('+');
            let held = if m.is_empty() { egui::Modifiers::NONE } else { parse_key(&format!("{m}+a")).map(|(_, m)| m).context("drag modifiers")? };
            let v: Vec<f32> = rest.split(':').filter_map(|n| n.parse().ok()).collect();
            anyhow::ensure!(v.len() == 4, "drag@x0:y0:x1:y1");
            let first = script.len();
            push_drag(&mut script, egui::pos2(v[0], v[1]), egui::pos2(v[2], v[3]), held, hold);
            for f in first..script.len() {
                frame_mods.insert(f, held);
            }
            script.push(vec![]);
            continue;
        }
        let media = k.strip_prefix("media@").map(|r| (r, true)).or(k.strip_prefix("hover-media@").map(|r| (r, false)));
        if let Some((rest, release)) = media {
            let (path, at) = rest.rsplit_once('@').context("media@PATH@x:y")?;
            let pos = xy(at)?;
            for _ in 0..3 {
                payloads.insert(script.len(), PathBuf::from(path));
                script.push(vec![egui::Event::PointerMoved(pos)]);
            }
            // Give background work (probing the file) a moment.
            naps.insert(script.len() - 1);
            if release {
                payloads.insert(script.len(), PathBuf::from(path));
                script.push(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
                script.push(vec![]);
            }
            continue;
        }
        if k == "wait" {
            waits.insert(script.len());
            script.push(vec![]);
            continue;
        }
        if let Some(n) = k.strip_prefix("play:") {
            plays.insert(script.len(), n.parse::<u32>().context("play:N")?);
            script.push(vec![]);
            continue;
        }
        if let Some(text) = k.strip_prefix("type:") {
            script.push(vec![egui::Event::Text(text.to_string())]);
            script.push(vec![]);
            continue;
        }
        let Some((key, mods)) = parse_key(k) else { anyhow::bail!("unknown key {k:?}") };
        let ev = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: mods };
        script.push(vec![ev(true)]);
        script.push(vec![ev(false)]);
        script.push(vec![]);
    }
    if recording {
        app.prefs.record_countdown = false;
        app.prefs.record_transcribe = false;
        super::record::start(&mut app);
        script.extend(vec![vec![]; 30]);
    }
    let settle = script.len() - 3;
    // Every frame's texture changes must reach the renderer (font atlas growth).
    let apply = |out: &egui::FullOutput| {
        let mut r = rs.renderer.write();
        for (id, delta) in &out.textures_delta.set {
            r.update_texture(&device, &queue, *id, delta);
        }
    };
    // Runs at least one pass, so it always returns an output.
    let wait_renders = |app: &mut VgApp| {
        let waited = std::time::Instant::now();
        let mut quiet = 0;
        let mut out = None;
        while quiet < 3 && waited.elapsed().as_secs() < 240 {
            if ((app.prefs.view == super::View::Audio || app.studio.pending_sync) && app.studio.busy()) || app.assist.busy() || app.music.busy() || app.preview.frames.scrubbing() {
                quiet = 0;
                std::thread::sleep(std::time::Duration::from_millis(40));
            } else {
                quiet += 1;
            }
            let o = ctx.run_ui(raw(vec![], None), |ui| app.ui_root(ui));
            apply(&o);
            out = Some(o);
        }
        out
    };
    for (i, events) in script.into_iter().enumerate() {
        if i == settle_frames || waits.contains(&i) {
            wait_renders(&mut app);
            app.media.wait();
            app.preview.frames.wait();
        }
        if let Some(&n) = plays.get(&i) {
            // Playback without sound: the clock runs, the audio player isn't started.
            app.playing = true;
            app.last_tick = Some(std::time::Instant::now());
            let (from, began, shown) = (app.time, std::time::Instant::now(), app.preview.frames.stats.shown);
            for _ in 0..n {
                let o = ctx.run_ui(raw(vec![], None), |ui| app.ui_root(ui));
                apply(&o);
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            let secs = began.elapsed().as_secs_f64();
            let frames = app.preview.frames.stats.shown - shown;
            println!("played {:.2}s → {:.2}s in {secs:.2}s: {frames} video frames ({:.1} fps)", from, app.time, frames as f64 / secs);
        }
        if recording {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if naps.contains(&i) {
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        if let Some(p) = payloads.get(&i) {
            egui::DragAndDrop::set_payload(&ctx, super::video::MediaDrop(p.clone()));
        }
        if keep && i == settle {
            super::record::stop(&mut app, &ctx);
        }
        let out = ctx.run_ui(raw(events, frame_mods.get(&i).copied()), |ui| app.ui_root(ui));
        apply(&out);
    }
    // Let background audio renders finish (the Audio tab), then settle.
    let out = wait_renders(&mut app).context("no frame rendered")?;
    if recording && !keep {
        super::record::discard(&mut app);
    }
    if keep {
        println!("{}", app.debug_report());
    }
    if drag.is_some() || !opts.keys.is_empty() {
        println!("{}", app.debug_report());
    }
    if !app.project.video.is_empty() {
        println!("{}", app.preview.frames.stats.report());
    }
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shot"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let jobs = ctx.tessellate(out.shapes, 1.0);
    let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [w, h], pixels_per_point: 1.0 };
    let mut enc = device.create_command_encoder(&Default::default());
    let extra = rs.renderer.write().update_buffers(&device, &queue, &mut enc, &jobs, &screen);
    {
        let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rs.renderer.read().render(&mut pass.forget_lifetime(), &jobs, &screen);
    }
    queue.submit(extra.into_iter().chain(Some(enc.finish())));
    save_texture(&device, &queue, &target, w, h, &opts.output)
}

fn save_texture(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32, path: &Path) -> Result<()> {
    let bpr = (4 * w).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shot readback"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let mut img = image::RgbaImage::new(w, h);
    {
        let data = buf.slice(..).get_mapped_range();
        for y in 0..h {
            let row = &data[(y * bpr) as usize..(y * bpr + 4 * w) as usize];
            img.as_mut()[(y * 4 * w) as usize..((y + 1) * 4 * w) as usize].copy_from_slice(row);
        }
    }
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    img.save(path).with_context(|| format!("writing {}", path.display()))
}
