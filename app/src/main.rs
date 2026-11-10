#![cfg_attr(windows, windows_subsystem = "windows")]
//! Ferrous Commander — a fast, light multi-pane file manager with tiled panels, tabs and column navigation.

mod app;
mod archive;
mod chrome;
mod compress;
mod drives;
mod fsx;
#[cfg(windows)]
mod file_transfer;
mod icons;
mod install;
#[cfg(target_os = "linux")]
mod linux_fs;
#[cfg(target_os = "linux")]
mod linux_properties;
#[cfg(target_os = "linux")]
mod linux_drag;
mod keymap;
mod lua_config;
mod omarchy;
mod openwith;
mod ops;
mod palette;
mod preview;
mod defaults;
mod settings;
mod theme;
mod tiles;
mod ui;
mod ui_keys;
mod ui_list;
mod ui_palette;
mod ui_win;
#[cfg(windows)]
mod win;
#[cfg(windows)]
mod windows_fs;
#[cfg(test)]
mod gui_tests;

use std::path::PathBuf;

/// Text needs far less than the largest texture a GPU offers, and the font atlas
/// takes the full width of its side in memory. Previews stay within `preview::MAX_SIDE`.
const MAX_TEXTURE_SIDE: usize = 2048;

/// A plugin rather than `raw_input_hook`, which runs for the main window only. Windows with
/// different limits would make egui rebuild the font atlas each time it switches between them.
struct TextureLimit;

impl egui::Plugin for TextureLimit {
    fn debug_name(&self) -> &'static str {
        "TextureLimit"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        input.max_texture_side = Some(input.max_texture_side.map_or(MAX_TEXTURE_SIDE, |side| side.min(MAX_TEXTURE_SIDE)));
    }
}

struct Shell {
    app: app::App,
    /// What the window's frame was last dressed for.
    #[cfg(windows)]
    dressed: Option<(win::WindowLook, Option<egui::Theme>)>,
}

#[cfg(windows)]
impl Shell {
    /// The title bar takes the theme's colour, or the whole window goes on Mica.
    fn dress_window(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(RawWindowHandle::Win32(handle)) = frame.window_handle().map(|w| w.as_raw()) else { return };
        let rgb = |c: egui::Color32| [c.r(), c.g(), c.b()];
        let pal = self.app.pal;
        // Only the OpenGL renderer hands its transparency on to the desktop.
        self.app.effect_unavailable |= frame.gl().is_none();
        let look = win::WindowLook {
            dark: pal.dark,
            bar: rgb(pal.window),
            text: rgb(pal.text),
            mica: self.app.st.window_effect && !self.app.effect_unavailable,
        };
        // Windows dresses the title bar anew when the system theme changes.
        let wanted = Some((look, ctx.input(|i| i.raw.system_theme)));
        if self.dressed == wanted { return; }
        self.dressed = wanted;
        let mica = win::dress_window(handle.hwnd.get(), look);
        self.app.effect_unavailable |= look.mica && !mica;
        if self.app.see_through != mica {
            self.app.see_through = mica;
            ctx.request_repaint();
        }
    }
}

/// A plugin so that text fields in every window get their clipboard shortcuts.
#[cfg(windows)]
#[derive(Default)]
struct ClipboardKeys {
    held: std::collections::HashSet<egui::Key>,
}

#[cfg(windows)]
impl egui::Plugin for ClipboardKeys {
    fn debug_name(&self) -> &'static str {
        "ClipboardKeys"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        file_transfer::shortcuts(input, &mut self.held, ctx.text_edit_focused());
    }
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.draw(ui);
        #[cfg(target_os = "linux")]
        linux_drag::handoff(&mut self.app, ui.ctx(), _frame);
        #[cfg(windows)]
        self.dress_window(ui.ctx(), _frame);
    }

    /// Nothing of its own behind the panels when the desktop's material shows there.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let behind = if self.app.see_through { egui::Color32::TRANSPARENT } else { self.app.pal.window };
        behind.to_normalized_gamma_f32()
    }
}

/// A clear system UI font when one is installed; otherwise egui's built-in font.
fn fonts(ctx: &egui::Context) {
    let candidates: &[&str] = if cfg!(windows) {
        &["C:\\Windows\\Fonts\\segoeui.ttf"]
    } else {
        &[
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        ]
    };
    let mut defs = egui::FontDefinitions::default();
    if let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) {
        defs.font_data.insert("ui".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        defs.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    ctx.set_fonts(defs);
}

fn options(renderer: eframe::Renderer, place: settings::WindowPlace) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer,
        clipboard_shortcuts: !cfg!(windows),
        viewport: egui::ViewportBuilder::default()
            .with_title("Ferrous Commander")
            .with_inner_size(place.size())
            .with_min_inner_size(settings::WindowPlace::MIN)
            .with_maximized(place.maximized)
            .with_icon(std::sync::Arc::new(icons::app_icon()))
            .with_app_id("ferrous-commander"),
        ..Default::default()
    }
}

fn run(renderer: eframe::Renderer, shot: Option<PathBuf>, loaded: Option<(settings::Settings, Option<String>)>) -> eframe::Result {
    archive::detect_tools();
    let place = loaded.as_ref().map(|(settings, _)| settings.window).unwrap_or_default();
    eframe::run_native(
        "Ferrous Commander",
        options(renderer, place),
        Box::new(move |cc| {
            cc.egui_ctx.add_plugin(TextureLimit);
            #[cfg(windows)]
            cc.egui_ctx.add_plugin(ClipboardKeys::default());
            fonts(&cc.egui_ctx);
            Ok(Box::new(Shell {
                app: app::App::with_settings(shot, loaded),
                #[cfg(windows)]
                dressed: None,
            }))
        }),
    )
}

#[cfg(test)]
#[test]
fn every_window_gets_the_same_texture_limit() {
    let ctx = egui::Context::default();
    ctx.add_plugin(TextureLimit);
    for offered in [Some(16384), Some(MAX_TEXTURE_SIDE), None] {
        let input = egui::RawInput { max_texture_side: offered, ..Default::default() };
        let mut seen = 0;
        let mut output = ctx.run_ui(input, |ui| seen = ui.input(|i| i.max_texture_side));
        output.textures_delta.clear();
        assert_eq!(seen, MAX_TEXTURE_SIDE);
    }
}

/// By default glibc gives every busy thread its own heap and hands freed memory back only
/// from the very top of it. Folder and preview workers allocate what the interface later
/// frees, so that memory would stay with the process.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn keep_the_heap_small() {
    // SAFETY: takes plain integers, and runs before any other thread exists.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 1 << 20);
        libc::mallopt(libc::M_MMAP_THRESHOLD, 256 * 1024);
    }
}

fn main() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    keep_the_heap_small();
    #[cfg(windows)]
    win::load_libraries_from_system_only();
    #[cfg(windows)]
    if openwith::run_handler_helper() { return; }
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == preview::helper::FLAG) {
        preview::helper::serve();
        return;
    }
    let mut shot = None;
    let mut start: Option<PathBuf> = None;
    let mut k = 1;
    while k < args.len() {
        match args[k].to_str().unwrap_or("") {
            "--screenshot" if k + 1 < args.len() => {
                shot = Some(PathBuf::from(&args[k + 1]));
                k += 1;
            }
            "--help" | "-h" => {
                println!("ferrous-commander [folder] [--screenshot <file.png>] [--install] [--uninstall [--silent]] [--export-icon <file>] [--version]");
                return;
            }
            "--version" | "-V" => {
                println!("ferrous-commander {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--install" | "--uninstall" => {
                let uninstalling = args[k] == "--uninstall";
                let r = if uninstalling { install::uninstall() } else { install::install() };
                #[cfg(windows)]
                if uninstalling && !args.iter().any(|a| a == "--silent") {
                    // Started from "Installed apps", where there is no console to print to.
                    match &r {
                        Ok(m) => install::show_message(m, false),
                        Err(e) => install::show_message(e, true),
                    }
                }
                match r {
                    Ok(m) => println!("{m}"),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            "--export-icon" if k + 1 < args.len() => {
                if let Err(e) = install::export_icon(std::path::Path::new(&args[k + 1])) {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
                return;
            }
            _ => start = Some(PathBuf::from(&args[k])),
        }
        k += 1;
    }
    if let Some(dir) = start.filter(|d| d.is_dir()).and_then(|d| fsx::full_folder(&d).ok()) {
        app::set_start_dir(dir);
    }
    // The light OpenGL renderer first; the wgpu renderer (DirectX/Vulkan/Metal) if OpenGL is missing.
    // FERROUS_RENDERER=wgpu picks wgpu directly.
    let (first, second) = match std::env::var("FERROUS_RENDERER").as_deref() {
        Ok("wgpu") => (eframe::Renderer::Wgpu, eframe::Renderer::Glow),
        _ => (eframe::Renderer::Glow, eframe::Renderer::Wgpu),
    };
    // Read before the window exists, which opens at the size it was left at.
    let loaded = shot.is_none().then(settings::Settings::load);
    if let Err(e) = run(first, shot.clone(), loaded.clone()) {
        eprintln!("renderer {first:?} failed ({e}); trying {second:?}");
        if let Err(e) = run(second, shot, loaded) {
            eprintln!("Could not start: {e}");
            std::process::exit(1);
        }
    }
}
