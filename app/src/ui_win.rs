//! Floating windows: dialogs, preview, search, settings, notifications, drag label, screenshot.

use std::sync::atomic::Ordering;

use egui::{Align2, CornerRadius, FontId, Frame, Key, Margin, Rect, RichText, Sense, Stroke, pos2, vec2};

use crate::app::{Act, App, Dialog, DragPaths, DropTarget, Notice, ToastKind, is_computer};
use crate::fsx;
use crate::icons;
use crate::settings::Layout;
use crate::theme::{ThemeName, hex};
use crate::ui::fit_text;

fn settings_section(ui: &mut egui::Ui, title: &str, accent: egui::Color32) {
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong().color(accent));
    ui.add_space(4.0);
}

impl App {
    pub fn dialogs(&mut self, ctx: &egui::Context) {
        #[cfg(not(windows))]
        self.app_chooser_ui(ctx);
        #[cfg(target_os = "linux")]
        if let Some(mut properties) = self.properties.take() {
            let (keep, changed) = properties.show(ctx);
            if changed { self.acts.push(Act::Refresh); }
            if keep { self.properties = Some(properties); }
        }
        let Some(mut d) = self.dialog.take() else { return };
        let mut keep = true;
        let (title, hint, label) = match &d {
            Dialog::AddLocation { .. } => ("Add location", "Folder path or network share", "Add"),
            Dialog::NewFolder { .. } => ("Create new folder", "Enter folder name", "Create"),
            Dialog::NewFile { .. } => ("New file", "File name", "Create"),
            Dialog::RenameFavorite { .. } => ("Rename shortcut", "Folder name", "Rename"),
            Dialog::RenameGroup { .. } => ("Rename group", "Group name", "Rename"),
            Dialog::NewGroup { .. } => ("New group", "New Group name", "Create"),
            Dialog::Password { .. } => ("Password needed", "Password", "Extract"),
            Dialog::Compress { .. } => ("Create archive", "Archive name", "Compress"),
        };
        let pal = self.pal;
        let creates = matches!(d, Dialog::NewFolder { .. } | Dialog::NewFile { .. });
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, -60.0))
            .default_width(360.0)
            .show(ctx, |ui| {
                if let Dialog::Password { archive, wrong, .. } = &d {
                    ui.label(RichText::new(fsx::display_name(archive)).color(pal.text).strong());
                    if *wrong { ui.label(RichText::new("Wrong password. Try again.").color(hex("#ef7063"))); }
                }
                let secret = matches!(d, Dialog::Password { .. });
                let (name, focus) = match &mut d {
                    Dialog::AddLocation { name, focus, .. }
                    | Dialog::RenameFavorite { name, focus, .. }
                    | Dialog::NewFolder { name, focus, .. }
                    | Dialog::NewFile { name, focus, .. }
                    | Dialog::RenameGroup { name, focus, .. }
                    | Dialog::NewGroup { name, focus }
                    | Dialog::Compress { name, focus, .. }
                    | Dialog::Password { name, focus, .. } => (name, focus),
                };
                let resp = ui.add(egui::TextEdit::singleline(name).hint_text(hint).password(secret).desired_width(340.0));
                if *focus {
                    resp.request_focus();
                    *focus = false;
                }
                let name = name.clone();
                if let Dialog::AddLocation { group, .. } = &mut d {
                    egui::ComboBox::from_label("Group").selected_text(self.st.favorites.get(*group).map(|g| g.name.as_str()).unwrap_or("Choose group")).show_ui(ui, |ui| {
                        for (i, g) in self.st.favorites.iter().enumerate() { ui.selectable_value(group, i, &g.name); }
                    });
                    ui.label(RichText::new(if cfg!(windows) { r"Use a full path, e.g. C:\Work or \\server\share." } else { "Use a full path, e.g. /home/user/Work or /mnt/share." }).color(pal.text_weak).size(11.0));
                    ui.label(RichText::new("Saved as a shortcut; offline locations can be added too.").color(pal.text_weak).size(11.0));
                }
                if let Dialog::Compress { sources, kind, .. } = &mut d {
                    egui::ComboBox::from_label("Format").selected_text(kind.suffix()).show_ui(ui, |ui| {
                        for option in crate::compress::Kind::ALL { ui.selectable_value(kind, option, option.suffix()); }
                    });
                    ui.label(RichText::new(format!("{} item(s), saved in this folder. Existing files are never replaced.", sources.len())).color(pal.text_weak).size(11.0));
                }
                if matches!(d, Dialog::RenameFavorite { .. }) {
                    ui.label(RichText::new("Changes the name in the sidebar only; the folder is not renamed. Leave empty to use the folder's name.").color(pal.text_weak).size(11.0));
                }
                let valid = matches!(d, Dialog::RenameFavorite { .. }) || (secret && !name.is_empty()) || !name.trim().is_empty() && (!matches!(d, Dialog::AddLocation { .. }) || std::path::Path::new(name.trim()).is_absolute());
                let valid = valid && match &d {
                    Dialog::Compress { kind, .. } => fsx::valid_name(&crate::compress::file_name(&name, *kind)),
                    _ => true,
                };
                if !name.trim().is_empty() && !valid {
                    ui.label(if matches!(d, Dialog::Compress { .. }) { "This name can't be used here." } else { "Enter a full folder path or network share." });
                }

                let (enter, ctrl, shift, esc) = ui.input(|inp| {
                    (inp.key_pressed(Key::Enter), inp.modifiers.command, inp.modifiers.shift, inp.key_pressed(Key::Escape))
                });
                ui.add_space(6.0);
                let mut go: Option<bool> = None;
                ui.horizontal(|ui| {
                    if ui.add_enabled(valid, egui::Button::new(label)).on_hover_text("Enter").clicked() {
                        go = Some(false);
                    }
                    if creates && ui.button("...and open").on_hover_text("Ctrl + Enter").clicked() {
                        go = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
                if let Dialog::NewFolder { pane, .. } = &d {
                    let n = self.panes[*pane].selected.len();
                    if n > 0 {
                        let t = format!("Shift + Enter: create and move {n} selected item(s) into it");
                        ui.label(RichText::new(t).color(pal.text_weak).size(11.0));
                    }
                }
                if valid && enter && resp.lost_focus() {
                    go = Some(ctrl);
                }
                if esc {
                    keep = false;
                }
                let Some(open_after) = go else { return };
                keep = false;
                match &d {
                    Dialog::AddLocation { group, .. } => {
                        self.acts.push(Act::AddFavorite(*group, std::path::PathBuf::from(name.trim())));
                    }
                    Dialog::NewFolder { pane, .. } => {
                        let pane = *pane;
                        let sel = if shift && enter { self.selection(pane) } else { Vec::new() };
                        self.create_item(pane, &name, true, open_after && sel.is_empty());
                        if !sel.is_empty() {
                            let dest = self.panes[pane].path().join(&name);
                            if dest.is_dir() {
                                self.start_job(crate::ops::OpKind::Move, sel, Some(dest), ui.ctx());
                            }
                        }
                    }
                    Dialog::NewFile { pane, .. } => self.create_item(*pane, &name, false, open_after),
                    Dialog::RenameFavorite { group, path, .. } => {
                        if let Some(g) = self.st.favorites.get_mut(*group).filter(|g| g.paths.contains(path)) {
                            g.set_label(path, &name);
                        }
                        self.mark_dirty();
                    }
                    Dialog::RenameGroup { group, .. } => {
                        if let Some(g) = self.st.favorites.get_mut(*group)
                            && !name.trim().is_empty() {
                                g.name = name.trim().to_string();
                            }
                        self.mark_dirty();
                    }
                    Dialog::NewGroup { .. } => {
                        if !name.trim().is_empty() {
                            self.group_add_new(name.trim().to_string());
                        }
                    }
                    Dialog::Compress { pane, sources, kind, .. } => {
                        self.compress(*pane, sources.clone(), crate::compress::file_name(&name, *kind), *kind, ui.ctx());
                    }
                    Dialog::Password { archive, dest, .. } => {
                        self.extract_with_password(archive.clone(), dest.clone(), name.clone(), ui.ctx());
                    }
                }
            });
        if keep {
            self.dialog = Some(d);
        }
    }

    /// Shows one notice at a time. Escape always takes the choice that changes nothing,
    /// and Enter does nothing, so a stray key press can never confirm a deletion.
    pub fn notices_ui(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.notices.first() else { return };
        let pal = self.pal;
        let title = match notice {
            Notice::ConfirmDestroy { .. } => "Delete permanently?".to_string(),
            Notice::ConfirmClose => "File operations are still running".to_string(),
            Notice::ConfirmRun { .. } => "Run this program?".to_string(),
            Notice::Problems { title, .. } => title.clone(),
        };
        let mut dismiss = ctx.input(|inp| inp.key_pressed(Key::Escape));
        let mut confirm = false;
        egui::Window::new(title)
            .id(egui::Id::unique("notice"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, -60.0))
            .default_width(440.0)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let list = |ui: &mut egui::Ui, lines: &[String]| {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for line in lines { ui.add(egui::Label::new(RichText::new(line).color(pal.text)).selectable(true)); }
                    });
                };
                match notice {
                    Notice::ConfirmDestroy { items } => {
                        let mut reasons: Vec<&str> = items.iter().map(|(_, reason)| reason.as_str()).filter(|reason| !reason.is_empty()).collect();
                        reasons.dedup();
                        let bin = if cfg!(windows) { "Recycle Bin" } else { "Trash" };
                        let intro = if reasons.is_empty() {
                            format!("{} item(s) will be deleted without going to the {bin}.", items.len())
                        } else {
                            format!("{} item(s) cannot go to the {bin}: {}.", items.len(), reasons.join("; "))
                        };
                        ui.label(RichText::new(intro).color(pal.text));
                        ui.label(RichText::new("Nothing has been deleted yet. If you continue, these items cannot be recovered.").color(hex("#ef7063")).strong());
                        ui.add_space(6.0);
                        let names: Vec<String> = items.iter().map(|(path, _)| fsx::display_name(path)).collect();
                        list(ui, &names);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            dismiss |= ui.button("Keep the files").clicked();
                            confirm = ui.button(RichText::new("Delete permanently").color(hex("#ef7063"))).clicked();
                        });
                    }
                    Notice::ConfirmClose => {
                        ui.label(RichText::new("Closing now would leave them unfinished. Cancelling stops safely: unfinished copies are discarded and source files stay in place.").color(pal.text));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            dismiss |= ui.button("Keep working").clicked();
                            confirm = ui.button("Cancel operations and close").clicked();
                        });
                    }
                    Notice::ConfirmRun { path, place, link } => {
                        // The whole name, unshortened: the ending is what matters here.
                        ui.add(egui::Label::new(RichText::new(fsx::display_name(path)).color(pal.text).strong()).selectable(true));
                        if let Some(target) = link {
                            ui.add(egui::Label::new(RichText::new(format!("Link to {target}")).color(pal.text)).selectable(true));
                        }
                        ui.label(RichText::new(format!("This file is on {place} and can do anything you can do on this computer. Run it only if you trust where it came from.")).color(pal.text));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            dismiss |= ui.button("Keep closed").clicked();
                            confirm = ui.button(RichText::new("Run").color(hex("#ef7063"))).clicked();
                        });
                    }
                    Notice::Problems { lines, .. } => {
                        list(ui, lines);
                        ui.add_space(6.0);
                        dismiss |= ui.button("Close").clicked();
                    }
                }
            });
        if !confirm && !dismiss { return; }
        let notice = self.notices.remove(0);
        if !confirm { return; }
        match notice {
            Notice::ConfirmDestroy { items } => {
                self.start_job(crate::ops::OpKind::Destroy, items.into_iter().map(|(path, _)| path).collect(), None, ctx);
            }
            Notice::ConfirmClose => {
                for job in &self.jobs { job.cancel.store(true, Ordering::Relaxed); }
                self.closing = true;
            }
            Notice::ConfirmRun { path, .. } => self.launch(&path),
            Notice::Problems { .. } => {}
        }
    }

    #[cfg(not(windows))]
    fn app_chooser_ui(&mut self, ctx: &egui::Context) {
        let Some(mut chooser) = self.app_chooser.take() else { return };
        chooser.poll();
        let mut open = true;
        let mut selected = None;
        let mut cancel = false;
        egui::Window::new("Choose another app")
            .id(egui::Id::unique("linux_app_chooser"))
            .open(&mut open).collapsible(false)
            .default_size(vec2(420.0, 380.0))
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("Open {} with:", fsx::display_name(&chooser.path)));
                let response = ui.add(egui::TextEdit::singleline(&mut chooser.query).hint_text("Find an installed application"));
                if chooser.focus { response.request_focus(); chooser.focus = false; }
                ui.separator();
                if chooser.loading { ui.spinner(); }
                let query = chooser.query.to_lowercase();
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    let mut count = 0;
                    for app in &chooser.apps {
                        if !app.name.to_lowercase().contains(&query) { continue; }
                        count += 1;
                        ui.push_id(&app.exec, |ui| {
                            if ui.selectable_label(false, &app.name).clicked() { selected = Some(app.clone()); }
                        });
                    }
                    if !chooser.loading && count == 0 { ui.label("No applications found."); }
                });
                ui.separator();
                ui.label(RichText::new("Applies to this file; your default application stays the same.").small());
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
            });
        if let Some(app) = selected {
            self.acts.push(Act::OpenWith(chooser.path, Some(app)));
        } else if open && !cancel {
            if chooser.loading { ctx.request_repaint_after(std::time::Duration::from_millis(100)); }
            self.app_chooser = Some(chooser);
        }
    }

    pub fn search_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.search.take() else { return };
        let mut open = true;
        let pal = self.pal;
        let i = self.active;
        let show_hidden = self.st.show_hidden;
        egui::Window::new("Search").open(&mut open).default_size(vec2(560.0, 440.0)).collapsible(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut s.query).hint_text("Names").desired_width(300.0));
                if s.focus {
                    resp.request_focus();
                    s.focus = false;
                }
                let enter = resp.lost_focus() && ui.input(|inp| inp.key_pressed(Key::Enter));
                let running = s.run.as_ref().is_some_and(|r| !r.done.load(Ordering::Relaxed));
                if running {
                    if ui.button("Stop").clicked()
                        && let Some(r) = &s.run {
                            r.cancel.store(true, Ordering::Relaxed);
                        }
                    ui.spinner();
                } else if (ui.button("Search").clicked() || enter) && !s.query.trim().is_empty() {
                    s.run = Some(crate::ops::search(s.root.clone(), s.query.clone(), show_hidden, ui.ctx().clone()));
                }
            });
            ui.label(RichText::new(format!("within  {}", s.root.display())).color(pal.text_weak).size(11.0));
            ui.separator();
            let Some(run) = &s.run else {
                ui.label(RichText::new("Type part of a name and press Enter. All words must match.").color(pal.text_weak));
                return;
            };
            let results = run.results.lock().unwrap().clone();
            let scanned = run.scanned.load(Ordering::Relaxed);
            let msg = format!("{} found   ·   {} items scanned", results.len(), scanned);
            ui.label(RichText::new(msg).color(pal.text_weak).size(11.0));
            let rh = self.row_h() + 4.0;
            let font = FontId::proportional(self.st.font_size);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, rh, results.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for e in &results[range] {
                    let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), rh), Sense::click());
                    let p = ui.painter();
                    if resp.hovered() {
                        p.rect_filled(rr, CornerRadius::same(1), pal.hover);
                    }
                    let ir = Rect::from_min_size(pos2(rr.min.x + 4.0, rr.center().y - 8.0), vec2(16.0, 16.0));
                    icons::entry(p, ir, e.kind, &e.ext, pal.dark, false);
                    let name = fit_text(p, &e.name, &font, rr.width() * 0.45);
                    let ng = p.text(pos2(ir.max.x + 8.0, rr.center().y), Align2::LEFT_CENTER, name, font.clone(), pal.text);
                    let parent = e.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    let small = FontId::proportional(11.0);
                    let pt = fit_text(p, &parent, &small, rr.max.x - ng.max.x - 20.0);
                    p.text(pos2(ng.max.x + 12.0, rr.center().y), Align2::LEFT_CENTER, pt, small, pal.text_weak);
                    if resp.clicked()
                        && let Some(par) = e.path.parent() {
                            self.acts.push(Act::NavigateSelect(i, par.to_path_buf(), e.path.clone()));
                        }
                    if resp.double_clicked() {
                        self.acts.push(Act::Open(i, e.path.clone()));
                    }
                }
            });
        });
        if open {
            self.search = Some(s);
        } else if let Some(r) = &s.run {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Opens in the active panel; the settings window closes so the new tab can be seen.
    pub fn show_configuration_folder(&mut self, saved: std::io::Result<std::path::PathBuf>) {
        match saved {
            Ok(dir) => {
                self.acts.push(Act::OpenNewTab(self.active, dir, true));
                self.settings_open = false;
            }
            Err(e) => self.toast(ToastKind::Error, format!("Cannot open configuration folder: {e}")),
        }
    }

    pub fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let before = serde_json::to_string(&self.st).unwrap_or_default();
        let weak = self.pal.text_weak;
        let accent = self.pal.accent;
        let mut install_result: Option<Result<String, String>> = None;
        let mut close_after_uninstall = false;
        let available = (ctx.content_rect().size() - vec2(48.0, 64.0)).max(vec2(300.0, 240.0));
        egui::Window::new("Settings")
            .id(egui::Id::unique("settings_panel_v2"))
            .open(&mut open).collapsible(false).resizable(true).vscroll(true)
            .default_size(vec2(660.0, 620.0).min(available))
            .min_size(vec2(420.0, 260.0).min(available))
            .max_size(available)
            .default_pos(ctx.content_rect().center() - vec2(688.0, 680.0).min(available) * 0.5)
            .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(RichText::new("Appearance & layout").strong().color(accent));
            ui.add_space(4.0);
            egui::Grid::new("settings_grid").num_columns(2).spacing(vec2(16.0, 8.0)).show(ui, |ui| {
                ui.label("Theme");
                ui.horizontal_wrapped(|ui| {
                    for t in ThemeName::choices() {
                        ui.selectable_value(&mut self.st.theme, t, t.label());
                    }
                });
                ui.end_row();
                ui.label("Accent color");
                ui.horizontal(|ui| {
                    if self.st.theme.has_own_accent() {
                        ui.weak(format!("Follows the {} theme", self.st.theme.label()));
                        return;
                    }
                    let mut c = hex(&self.st.accent);
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        self.st.accent = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
                    }
                    for preset in ["#0078D7", "#0F92FE", "#88C866", "#C78EDD", "#EDA95D", "#EF7063"] {
                        let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                        ui.painter().circle_filled(r.center(), 7.0, hex(preset));
                        if resp.clicked() {
                            self.st.accent = preset.into();
                        }
                    }
                });
                ui.end_row();
                ui.label("Window layout");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.st.layout, Layout::Standard, "Standard");
                    ui.selectable_value(&mut self.st.layout, Layout::Columns, "Columns");
                });
                ui.end_row();
                ui.label("Panels");
                ui.horizontal(|ui| {
                    ui.label(format!("{} open", self.panes.len()));
                    if ui.button("+ New panel").on_hover_text("Ctrl+Shift+Enter").clicked() {
                        self.acts.push(Act::OpenPanel(self.active, None));
                    }
                    if self.panes.len() > 1 && ui.button("Close active").on_hover_text("Ctrl+Shift+Q").clicked() {
                        self.acts.push(Act::ClosePanel(self.active));
                    }
                });
                ui.end_row();
                ui.label("Font size");
                ui.add(egui::Slider::new(&mut self.st.font_size, 10.0..=18.0).step_by(0.5));
                ui.end_row();
            });
            settings_section(ui, "Files & folders", accent);
            ui.checkbox(&mut self.st.show_hidden, "Show hidden files and folders   (Ctrl+H)");
            ui.checkbox(&mut self.st.show_extensions, "Show file extensions");
            ui.checkbox(&mut self.st.age_colors, "Color the Age column by file age");
            ui.checkbox(&mut self.st.mebibytes, "Use old definition of Megabyte (MiB, as in Windows Explorer)");
            ui.checkbox(&mut self.st.sorting.folders_on_top, "Keep folders above files");
            settings_section(ui, "Workspace", accent);
            ui.checkbox(&mut self.st.show_columns, "Show folder columns   ( ` )");
            ui.checkbox(&mut self.st.show_preview, "Show preview / file details pane");
            ui.checkbox(&mut self.st.show_sidebar, "Show sidebar");
            settings_section(ui, "Sidebar groups", accent);
            ui.checkbox(&mut self.st.show_cloud, "Show automatically detected Cloud folders");
            ui.label(RichText::new("Cloud lists local sync folders, detected from OneDrive settings and folder names in your home directory.").color(weak).size(11.0));
            ui.label(RichText::new("Drag group headings to reorder. Use + to add a local or network path.").color(weak).size(11.0));
                    ui.menu_button("Restore groups…", |ui| {
                        if ui.button("History").clicked() {
                            self.st.show_history = true;
                            self.mark_dirty();
                            ui.close();
                        }
                        if ui.button("Default folders").clicked() {
                            for group in crate::settings::Settings::default().favorites {
                                if !self.st.favorites.iter().any(|g| g.name == group.name) {
                                    self.st.favorites.push(group);
                                }
                            }
                            self.mark_dirty();
                            ui.close();
                        }
                        for k in 0..self.st.removed_groups.len() {
                            if ui.button(self.st.removed_groups[k].name.clone()).clicked() {
                                let group = self.st.removed_groups.remove(k);
                                self.st.favorites.push(group);
                                self.mark_dirty();
                                ui.close();
                                break;
                            }
                        }
                    });

            settings_section(ui, "Configuration file", accent);
            if let Some(dir) = crate::settings::config_directory() {
                ui.label(RichText::new(dir.join("settings.lua").display().to_string()).color(weak).size(11.0));
            }
            ui.label(RichText::new("Text format: Lua table. Close the app before editing; changes load on next start.").color(weak).size(11.0));
            if ui.button("Open configuration folder").clicked() {
                self.save();
                let saved = self.st.save_result();
                self.show_configuration_folder(saved);
            }
            settings_section(ui, "Application", accent);
            ui.horizontal(|ui| {
                if crate::install::installed_by_package_manager() {
                    ui.label(RichText::new("✔ Installed by the system package manager").color(weak));
                } else if crate::install::is_installed() {
                    ui.label(RichText::new("✔ Installed for this user").color(weak));
                    if ui.button("Uninstall").clicked() {
                        let result = crate::install::uninstall();
                        close_after_uninstall = result.is_ok() && crate::install::must_exit_after_uninstall();
                        install_result = Some(result.map_err(|e| format!("Uninstall failed: {e}")));
                    }
                } else if ui.button("Install for this user (app menu, no admin needed)").clicked() {
                    install_result = Some(crate::install::install().map_err(|e| format!("Install failed: {e}")));
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Restore to default").clicked() {
                    let favorites = std::mem::take(&mut self.st.favorites);
                    let tags = std::mem::take(&mut self.st.tags);
                    self.st = crate::settings::Settings { favorites, tags, ..Default::default() };
                }
                ui.label(RichText::new("Settings are saved automatically").color(weak).size(11.0));
                ui.label(RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION"))).color(weak).size(11.0));
            });
        });
        match install_result {
            Some(Ok(m)) => self.toast(ToastKind::Success, m),
            Some(Err(e)) => self.toast(ToastKind::Error, e),
            None => {}
        }
        if close_after_uninstall {
            self.save();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if serde_json::to_string(&self.st).unwrap_or_default() != before {
            self.mark_dirty();
            self.reload_all();
            for p in &mut self.panes {
                p.view_key = None;
            }
        }
        if !open {
            self.settings_open = false;
        }
    }

    pub fn overlays(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        if self.toasts.is_empty() && self.jobs.is_empty() {
            return;
        }
        let area = egui::Area::new(egui::Id::unique("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -34.0))
            .order(egui::Order::Foreground)
            .interactable(true);
        area.show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            let frame = || Frame::NONE.fill(pal.raised).corner_radius(CornerRadius::same(1)).stroke(Stroke::new(1.0, pal.border));
            for job in &self.jobs {
                let pr = job.progress.lock().unwrap();
                frame().inner_margin(Margin::same(10)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} {}/{}", job.kind.verb(), pr.done_items, pr.total_items)).color(pal.text));
                        ui.label(RichText::new(&pr.current).color(pal.text_weak).size(11.0));
                        if ui.small_button("Cancel").clicked() {
                            job.cancel.store(true, Ordering::Relaxed);
                        }
                    });
                    let f = if pr.total_items > 0 { pr.done_items as f32 / pr.total_items as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(f).desired_height(4.0).fill(pal.accent));
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            for t in &self.toasts {
                let bar = match t.kind {
                    ToastKind::Info => pal.accent,
                    ToastKind::Success => hex("#88c866"),
                    ToastKind::Error => hex("#ef7063"),
                };
                let m = Margin { left: 16, right: 12, top: 8, bottom: 8 };
                let resp = frame().inner_margin(m).show(ui, |ui| {
                    ui.label(RichText::new(&t.text).color(pal.text));
                });
                let r = resp.response.rect;
                let br = Rect::from_min_size(r.min + vec2(5.0, 7.0), vec2(3.0, r.height() - 14.0));
                ui.painter().rect_filled(br, CornerRadius::same(2), bar);
            }
        });
    }

    pub fn handle_drop(&mut self, ctx: &egui::Context) {
        let Some(payload) = egui::DragAndDrop::payload::<DragPaths>(ctx) else { return };
        #[cfg(windows)]
        if ctx.input(|inp| inp.pointer.hover_pos().is_none_or(|p| !inp.content_rect().contains(p)))
            && crate::file_transfer::left_button_down()
        {
            // Keep pane/favorite drops local; hand the selection to the Shell
            // once the user leaves our window while holding the mouse button.
            egui::DragAndDrop::clear_payload(ctx);
            match crate::file_transfer::drag(&payload.0) {
                Ok(true) => self.acts.push(Act::Refresh),
                Ok(false) => {}, // Escape or an unsupported destination.
                Err(e) => self.toast(ToastKind::Error, e),
            }
            ctx.stop_dragging();
            ctx.input_mut(|inp| inp.pointer = Default::default());
            ctx.request_repaint();
            return;
        }
        let (pos, released, ctrl) = ctx.input(|inp| (inp.pointer.hover_pos(), inp.pointer.any_released(), inp.modifiers.command));
        let Some(pos) = pos else { return };
        let target = self.drop_zones.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, t)| t.clone());
        let pal = self.pal;
        // drag label (resources CursorMove, CursorCopy, CursorMoveExtra)
        let n = payload.0.len();
        let label = match &target {
            Some(DropTarget::Folder(_)) => {
                format!("{} · {n} {}", if ctrl { "COPY" } else { "MOVE" }, if n == 1 { "item" } else { "items" })
            }
            Some(DropTarget::Favorites(_)) => "Add to Favorites".into(),
            None => format!("{n} item(s)"),
        };
        // Give the tooltip its own width: an Area otherwise inherits the tiny
        // amount of space beside the pointer and wraps even short words.
        let screen = ctx.input(|inp| inp.content_rect());
        let width = 300.0_f32.min(screen.width() - 24.0).max(1.0);
        let height = 100.0_f32.min(screen.height());
        let x = if pos.x + 16.0 + width > screen.right() { pos.x - width - 16.0 } else { pos.x + 16.0 };
        let y = if pos.y + 12.0 + height > screen.bottom() { pos.y - height - 12.0 } else { pos.y + 12.0 };
        let tip_pos = pos2(x.max(screen.left()), y.max(screen.top()));
        let area = egui::Area::new(egui::Id::unique("dragtip")).fixed_pos(tip_pos).order(egui::Order::Tooltip).interactable(false);
        area.show(ctx, |ui| {
            ui.set_width(width);
            let f = Frame::NONE.fill(pal.raised).corner_radius(CornerRadius::same(1)).inner_margin(Margin::symmetric(12, 8));
            f.stroke(Stroke::new(1.0, pal.accent)).show(ui, |ui| {
                ui.set_width(width - 26.0);
                ui.add(egui::Label::new(RichText::new(&label).color(pal.text).size(13.0).strong()).truncate());
                if let Some(DropTarget::Folder(p)) = &target {
                    let to = if is_computer(p) { "This PC".into() } else { fsx::display_name(p) };
                    ui.add(egui::Label::new(RichText::new(format!("To: {to}")).color(pal.text).size(12.0)).truncate());
                    ui.add_space(4.0);
                    let hint = if ctrl { "Release Ctrl to move" } else { "Hold Ctrl to copy" };
                    ui.add(egui::Label::new(RichText::new(hint).color(pal.text_weak).size(12.0)).truncate());
                }
            });
        });
        if released {
            egui::DragAndDrop::clear_payload(ctx);
            match target {
                Some(DropTarget::Folder(p)) if !is_computer(&p) => {
                    self.acts.push(Act::Drop { target: p, paths: payload.0.clone(), copy: ctrl });
                }
                Some(DropTarget::Favorites(g)) => {
                    for p in payload.0.iter() {
                        self.acts.push(Act::AddFavorite(g, p.clone()));
                    }
                }
                _ => {}
            }
        }
    }

    /// `--screenshot <file.png>`: draw ~120 frames, save the window picture, exit.
    pub fn screenshot(&mut self, ctx: &egui::Context) {
        let frame = self.frame;
        let Some(shot) = &mut self.shot else { return };
        ctx.request_repaint();
        if shot.done.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if frame >= 120 && !shot.requested {
            shot.requested = true;
            let file = shot.file.clone();
            let done = shot.done.clone();
            let c2 = ctx.clone();
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let mut bytes = Vec::with_capacity(w * h * 4);
                for p in &img.pixels {
                    bytes.extend_from_slice(&p.to_srgba_unmultiplied());
                }
                if let Some(dir) = file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = image::save_buffer(&file, &bytes, w as u32, h as u32, image::ColorType::Rgba8) {
                    eprintln!("screenshot failed: {e}");
                }
                done.store(true, Ordering::Relaxed);
                c2.request_repaint();
            });
        }
        if frame > 900 {
            eprintln!("screenshot timed out");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
