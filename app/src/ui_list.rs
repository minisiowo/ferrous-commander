//! Folder header, the file list with its columns, menus, details and preview.

use std::path::PathBuf;

use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Modifiers, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::app::{Act, App, DragPaths, DropTarget, ToastKind, is_computer};
use crate::preview::{Key as PreviewKey, PreviewContent};
use crate::fsx::{self, Entry, SortBy};
use crate::icons;
use crate::theme::{TAGS, blend, hex};
use crate::ui::{chip, sort_chip, fit_text, icon_button, take_bottom, take_left, take_top};

thread_local! {
    static OPEN_WITH: std::cell::RefCell<std::collections::HashMap<String, AppMenu>> = Default::default();
}
struct AppMenu {
    apps: Option<Vec<crate::openwith::AppEntry>>,
    rx: std::sync::mpsc::Receiver<Vec<crate::openwith::AppEntry>>,
    queried_at: std::time::Instant,
}
fn open_with_apps(
    path: &std::path::Path,
    ctx: &egui::Context,
) -> Option<Vec<crate::openwith::AppEntry>> {
    OPEN_WITH.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Windows recommendations change when the user selects a new default.
        cache.retain(|_, menu| menu.queried_at.elapsed() < std::time::Duration::from_secs(30));
        let key = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let menu = cache.entry(key).or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            let path = path.to_path_buf();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(crate::openwith::apps_for(&path));
                ctx.request_repaint();
            });
            AppMenu {
                apps: None,
                rx,
                queried_at: std::time::Instant::now(),
            }
        });
        if let Ok(apps) = menu.rx.try_recv() {
            menu.apps = Some(apps);
        }
        menu.apps.clone()
    })
}

/// File-list columns from Default.json.
#[derive(Clone, Copy, PartialEq)]
pub enum Col {
    Name,
    Ext,
    Age,
    Size,
    Date,
    Time,
}
/// (column, title, MinWidth + Padding, RemovalOrder, right aligned).
/// Size has ElementAlign 2 in Default.json, so it sits at the right edge.
const COLS: [(Col, &str, f32, f32, bool); 6] = [
    (Col::Name, "Name", 180.0 + 23.0, 1000.0, false),
    (Col::Ext, "Ext", 40.0 + 9.0, 30.0, false),
    (Col::Age, "Age", 34.0 + 4.0 + 12.0, 40.0, true),
    (Col::Date, "Modified", 65.36 + 9.0, 50.0, false),
    (Col::Time, "Time", 32.48 + 8.0, 20.0, false),
    (Col::Size, "Size", 70.0, 60.0, true),
];

/// Columns that fit, dropping the lowest removal order first; Name takes the rest.
fn visible_cols(width: f32) -> Vec<(Col, &'static str, f32, bool)> {
    let mut order: Vec<usize> = (0..COLS.len()).collect();
    order.sort_by(|a, b| COLS[*b].3.total_cmp(&COLS[*a].3));
    let mut used = 0.0;
    let mut keep = [false; 6];
    for k in order {
        if used + COLS[k].2 <= width || k == 0 {
            keep[k] = true;
            used += COLS[k].2;
        }
    }
    let mut out: Vec<(Col, &'static str, f32, bool)> = COLS
        .iter()
        .enumerate()
        .filter(|(k, _)| keep[*k])
        .map(|(_, c)| (c.0, c.1, c.2, c.4))
        .collect();
    out[0].2 += (width - used).max(0.0);
    out
}

impl App {
    pub fn header(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let pal = self.pal;
        let path = self.panes[i].path().to_path_buf();
        // path row, then folder name with item count, then the toolbar
        let crumb_r = Rect::from_min_size(r.min + vec2(8.0, 2.0), vec2(r.width() - 16.0, 22.0));
        let name_r = Rect::from_min_size(
            pos2(r.min.x + 10.0, crumb_r.max.y + 2.0),
            vec2(r.width() - 20.0, 22.0),
        );
        let tools_r = Rect::from_min_max(
            pos2(r.min.x + 6.0, name_r.max.y + 1.0),
            pos2(r.max.x - 6.0, name_r.max.y + 27.0),
        );
        let title_r = crumb_r;
        {
            let p = ui.painter_at(name_r);
            let title = if is_computer(&path) {
                "This PC".to_string()
            } else {
                fsx::display_name(&path)
            };
            let f = FontId::proportional(13.5);
            let g = p.text(
                name_r.left_center(),
                Align2::LEFT_CENTER,
                fit_text(&p, &title, &f, name_r.width() * 0.6),
                f,
                pal.text,
            );
            let (n, sel, total) = self.counts(i);
            let mut c = format!("{n} items");
            if sel > 0 {
                c += &format!(" · {sel} selected");
            }
            p.text(
                pos2(g.max.x + 10.0, name_r.center().y),
                Align2::LEFT_CENTER,
                c,
                FontId::proportional(11.0),
                pal.text_weak,
            );
            p.text(
                name_r.right_center(),
                Align2::RIGHT_CENTER,
                fsx::fmt_size(total, self.st.mebibytes),
                FontId::proportional(11.0),
                pal.text_weak,
            );
        }

        if let Some(mut text) = self.panes[i].path_edit.take() {
            let mut keep = true;
            ui.scope_builder(UiBuilder::new().max_rect(title_r), |ui| {
                let te = egui::TextEdit::singleline(&mut text)
                    .font(FontId::proportional(12.0))
                    .margin(egui::Margin::symmetric(4, 2))
                    .desired_width(title_r.width())
                    .hint_text("Path");
                let resp = ui.put(title_r, te);
                if !resp.has_focus() && !resp.lost_focus() {
                    resp.request_focus();
                }
                if resp.lost_focus() {
                    keep = false;
                    if ui.input(|inp| inp.key_pressed(Key::Enter)) {
                        self.go_to_typed(i, &text);
                    }
                }
            });
            if keep {
                self.panes[i].path_edit = Some(text);
            }
        } else {
            self.breadcrumb(ui, i, title_r, &path);
        }

        ui.scope_builder(UiBuilder::new().max_rect(tools_r), |ui| {
            ui.set_clip_rect(tools_r.intersect(ui.clip_rect()));
            egui::ScrollArea::horizontal()
                .id_salt(("sort_tools", i))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let tab = &self.panes[i].tabs[self.panes[i].active];
                        let (can_back, can_fwd) = (!tab.back.is_empty(), !tab.fwd.is_empty());
                        if icon_button(ui, "⬆", "Open parent folder (Backspace)", false, &pal)
                            .clicked()
                        {
                            self.acts.push(Act::Up(i));
                        }
                        if icon_button(
                            ui,
                            "⬅",
                            "Go back in tab history (Alt + Left arrow)",
                            false,
                            &pal,
                        )
                        .clicked()
                            && can_back
                        {
                            self.acts.push(Act::Back(i));
                        }
                        if icon_button(
                            ui,
                            "➡",
                            "Go forward in tab history (Alt + Right arrow)",
                            false,
                            &pal,
                        )
                        .clicked()
                            && can_fwd
                        {
                            self.acts.push(Act::Forward(i));
                        }
                        if icon_button(ui, "⟳", "Refresh content (F5)", false, &pal).clicked() {
                            self.acts.push(Act::Refresh);
                        }
                        ui.add_space(8.0);
                        let s = self.cache.get(&path).map(|l| l.sorting).unwrap_or_default();
                        for (by, label, tip) in [
                            (SortBy::Name, "NAME", "Sorts items by name"),
                            (SortBy::Date, "DATE", "Sorts items by date modified"),
                            (
                                SortBy::Size,
                                "SIZE",
                                "Sorts files by size | Folders can't be sorted",
                            ),
                            (SortBy::Type, "TYPE", "Sorts items by type"),
                        ] {
                            let on = s.by == by;
                            if sort_chip(ui, label, on, on.then_some(s.ascending), &pal).on_hover_text(tip).clicked() {
                                self.acts.push(Act::SetSort(i, by));
                            }
                        }
                        if chip(ui, "Folders first", s.folders_on_top, &pal)
                            .on_hover_text("Keep folders above files (On/Off)")
                            .clicked()
                        {
                            self.acts.push(Act::ToggleFoldersOnTop(i));
                        }
                        ui.add_space(6.0);
                        if !self.panes[i].filter.is_empty() {
                            let f = self.panes[i].filter.clone();
                            let resp = chip(ui, &format!("Contains: {f}  ✖"), true, &pal);
                            if resp
                                .on_hover_text("Typing filters this folder. Esc clears it.")
                                .clicked()
                            {
                                self.panes[i].filter.clear();
                            }
                        }
                    });
                });
        });
        let actions_r = Rect::from_min_max(
            pos2(tools_r.min.x, tools_r.max.y + 2.0),
            pos2(tools_r.max.x, r.max.y),
        );
        ui.scope_builder(UiBuilder::new().max_rect(actions_r), |ui| {
            ui.set_clip_rect(actions_r.intersect(ui.clip_rect()));
            egui::ScrollArea::horizontal()
                .id_salt(("file_actions", i))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        if !self.panes[i].selected.is_empty() {
                            ui.add_space(6.0);
                            if icon_button(ui, "🗑", "Delete selected items (Del)", false, &pal)
                                .clicked()
                            {
                                self.acts.push(Act::Delete(i));
                            }
                            if icon_button(ui, "✏", "Rename (F2)", false, &pal).clicked() {
                                self.acts.push(Act::Rename(i));
                            }
                            if icon_button(ui, "🗐", "Copy to Clipboard (Ctrl+C)", false, &pal)
                                .clicked()
                            {
                                self.acts.push(Act::Copy(i));
                            }
                            if icon_button(ui, "✂", "Cut to Clipboard (Ctrl+X)", false, &pal)
                                .clicked()
                            {
                                self.acts.push(Act::Cut(i));
                            }
                        }
                        if icon_button(ui, ">_", "Open in command shell (F10)", false, &pal)
                            .clicked()
                        {
                            self.acts.push(Act::Terminal(self.panes[i].path().to_path_buf()));
                        }
                        if icon_button(ui, "🗋", "Create new file (Ctrl+N)", false, &pal).clicked()
                        {
                            self.acts.push(Act::NewFile(i));
                        }
                        if icon_button(ui, "🗀", "Create new folder (Ctrl+Shift+N)", false, &pal)
                            .clicked()
                        {
                            self.acts.push(Act::NewFolder(i));
                        }
                        if self.can_paste_files()
                            && icon_button(ui, "📋", "Paste from Clipboard (Ctrl+V)", false, &pal)
                                .clicked()
                        {
                            self.acts.push(Act::Paste(i, None));
                        }
                    });
                });
        });
    }

    fn go_to_typed(&mut self, i: usize, text: &str) {
        let raw = text.trim().trim_matches('"');
        let p = match raw.strip_prefix('~') {
            Some(rest) => dirs::home_dir()
                .map(|h| h.join(rest.trim_start_matches(['/', '\\'])))
                .unwrap_or_else(|| PathBuf::from(raw)),
            None => PathBuf::from(raw),
        };
        match fsx::probe_paths(std::slice::from_ref(&p), crate::app::PATH_PROBE_LIMIT)[0] {
            // No answer yet: the folder loader reports a share that stays silent.
            Some(fsx::PathKind::Folder) | None => self.acts.push(Act::Navigate(i, p)),
            Some(fsx::PathKind::File) => {
                if let Some(par) = p.parent() {
                    self.acts
                        .push(Act::NavigateSelect(i, par.to_path_buf(), p.clone()));
                }
            }
            Some(fsx::PathKind::Missing) => {
                self.toast(ToastKind::Error, format!("Folder {raw} not found"));
            }
        }
    }

    /// Big folder title followed by a clickable path; clicking the empty end edits the path.
    fn breadcrumb(&mut self, ui: &mut Ui, i: usize, title_r: Rect, path: &std::path::Path) {
        let pal = self.pal;
        let p = ui.painter_at(title_r);
        p.text(
            title_r.left_center() + vec2(2.0, 0.0),
            Align2::LEFT_CENTER,
            "☰",
            FontId::proportional(13.0),
            pal.text_weak,
        );
        let mut x = title_r.min.x + 20.0;
        let crumbs = self.column_chain(path);
        let cfont = FontId::proportional(12.0);
        let labels: Vec<String> = crumbs
            .iter()
            .map(|c| {
                if is_computer(c) {
                    "This PC".into()
                } else {
                    fsx::display_name(c)
                }
            })
            .collect();
        let widths: Vec<f32> = labels
            .iter()
            .map(|l| {
                p.layout_no_wrap(l.clone(), cfont.clone(), pal.text)
                    .size()
                    .x
                    + 16.0
            })
            .collect();
        let mut start = 0;
        while start + 1 < labels.len()
            && widths[start..].iter().sum::<f32>() > title_r.max.x - x - 30.0
        {
            start += 1;
        }
        for k in start..crumbs.len() {
            if x + widths[k] > title_r.max.x - 24.0 {
                break;
            }
            let cr = Rect::from_min_size(
                pos2(x, title_r.center().y - 10.0),
                vec2(widths[k] - 6.0, 20.0),
            );
            let resp = ui.interact(cr, ui.scope_id().with(("crumb", i, k)), Sense::click());
            if resp.hovered() {
                p.rect_filled(cr, CornerRadius::same(1), pal.hover);
            }
            let col = if resp.hovered() || k + 1 == crumbs.len() {
                pal.text
            } else {
                blend(pal.text_weak, pal.text, 0.5)
            };
            p.text(
                cr.center(),
                Align2::CENTER_CENTER,
                &labels[k],
                cfont.clone(),
                col,
            );
            if k + 1 < crumbs.len() {
                p.text(
                    pos2(cr.max.x + 3.0, cr.center().y),
                    Align2::CENTER_CENTER,
                    std::path::MAIN_SEPARATOR_STR,
                    cfont.clone(),
                    pal.text_weak,
                );
            }
            if resp.clicked() {
                self.acts.push(Act::Navigate(i, crumbs[k].clone()));
            }
            if resp.middle_clicked() {
                self.acts.push(Act::OpenNewTab(i, crumbs[k].clone(), false));
            }
            x += widths[k];
        }
        let er = Rect::from_min_max(pos2(x, title_r.min.y), title_r.max);
        if er.width() > 10.0 {
            let resp = ui.interact(er, ui.scope_id().with(("pathedit", i)), Sense::click());
            if resp.hovered() {
                p.text(
                    er.right_center() - vec2(2.0, 0.0),
                    Align2::RIGHT_CENTER,
                    "✏",
                    FontId::proportional(12.0),
                    pal.text_weak,
                );
            }
            if resp.on_hover_text("Edit path (\\ or Alt+D)").clicked() {
                self.panes[i].path_edit = Some(path.display().to_string());
            }
        }
    }

    pub fn file_list(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let pal = self.pal;
        let path = self.panes[i].path().to_path_buf();
        let Some(mut listing) = self.cache.remove(&path) else {
            return;
        };
        let original_entries = if self.st.tree_files {
            Some(std::mem::replace(&mut listing.entries, std::mem::take(&mut self.panes[i].tree_entries)))
        } else { None };
        let rh = self.row_h();
        let cols = visible_cols(r.width() - 14.0);
        let sorting = listing.sorting;

        // header row
        let hr = Rect::from_min_size(r.min, vec2(r.width(), 22.0));
        let hp = ui.painter_at(hr);
        let mut x = hr.min.x + 4.0;
        for (k, (c, title, w, right)) in cols.iter().enumerate() {
            let cr = Rect::from_min_size(pos2(x, hr.min.y), vec2(*w, hr.height()));
            x += w;
            let resp = ui.interact(cr, ui.scope_id().with(("colhdr", i, k)), Sense::click());
            let by = match c {
                Col::Name => SortBy::Name,
                Col::Ext => SortBy::Type,
                Col::Size => SortBy::Size,
                _ => SortBy::Date,
            };
            let on = sorting.by == by && !matches!(c, Col::Age | Col::Time);
            let col = if resp.hovered() || on {
                pal.text
            } else {
                pal.text_weak
            };
            let font = FontId::proportional(11.5);
            let label_rect = if *right {
                hp.text(
                    cr.right_center() - vec2(8.0 + if on { 12.0 } else { 0.0 }, 0.0),
                    Align2::RIGHT_CENTER,
                    *title,
                    font,
                    col,
                )
            } else {
                let off = if *c == Col::Name { 26.0 } else { 8.0 };
                hp.text(
                    cr.left_center() + vec2(off, 0.0),
                    Align2::LEFT_CENTER,
                    *title,
                    font,
                    col,
                )
            };
            if on {
                icons::sort_arrow(&hp, pos2(label_rect.max.x + 7.0, cr.center().y), sorting.ascending, col);
            }
            if resp.clicked() {
                self.acts.push(Act::SetSort(i, by));
            }
        }

        let list_r = Rect::from_min_max(pos2(r.min.x, hr.max.y), r.max);
        let n = self.panes[i].view.len();
        self.panes[i].list_h = list_r.height();
        let mut offset = None;
        if let Some(t) = self.panes[i].scroll_to.take() {
            let top = t as f32 * rh;
            let cur = self.panes[i].list_offset;
            if top < cur {
                offset = Some(top);
            } else if top + rh > cur + list_r.height() {
                offset = Some(top + rh - list_r.height());
            }
        }
        let ctx = ui.ctx().clone();
        let is_active = self.active == i;
        let dragging = egui::DragAndDrop::has_payload_of_type::<DragPaths>(&ctx);
        let cut = self.clip.as_ref().filter(|(_, cut)| *cut).map(|(paths, _)| paths.clone());
        let in_computer = is_computer(&path);
        let mut toggles = Vec::new();
        let mut clicks: Vec<(usize, Modifiers)> = Vec::new();
        let mut drag: Option<(PathBuf, bool)> = None;
        let marquee_id = ui.scope_id().with(("marquee", i));
        let mut marquee = ctx.data_mut(|d| d.get_temp::<(egui::Pos2, Vec<PathBuf>)>(marquee_id));
        let out = ui.scope_builder(UiBuilder::new().max_rect(list_r), |ui| {
            // show_rows reads spacing before calling its row painter. Keep its
            // virtual row positions in sync with painting and marquee hit tests.
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut sa = egui::ScrollArea::vertical()
                .id_salt(("list", i, &path))
                .auto_shrink([false, false]);
            if let Some(o) = offset {
                sa = sa.vertical_scroll_offset(o);
            }
            sa.show_rows(ui, rh, n, |ui, range| {
                for row in range {
                    let e = &listing.entries[self.panes[i].view[row]];
                    let (rr, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), rh),
                        Sense::click_and_drag(),
                    );
                    let selected = self.panes[i].selected.contains(&e.path);
                    self.paint_row(
                        ui,
                        i,
                        rr,
                        &resp,
                        e,
                        row,
                        &cols,
                        selected,
                        is_active,
                        dragging,
                        cut.as_deref().unwrap_or_default(),
                        in_computer,
                    );
                    // Selection is applied after drawing: the folder's listing is out of the
                    // cache while its rows are drawn.
                    let mods = ui.input(|inp| inp.modifiers);
                    let arrow_clicked = self.st.tree_files && e.is_dir
                        && resp.interact_pointer_pos().is_some_and(|pos| {
                            let indent = self.tree_indent(i, e, cols[0].2);
                            let arrow = Rect::from_min_size(
                                pos2(rr.min.x + indent - 12.0, rr.min.y),
                                vec2(18.0, rh),
                            );
                            arrow.contains(pos)
                        });
                    if resp.clicked() {
                        clicks.push((row, mods));
                        if arrow_clicked {
                            toggles.push(e.path.clone());
                        }
                    }
                    if resp.secondary_clicked() && !selected {
                        clicks.push((row, Modifiers::NONE));
                    }
                    if resp.double_clicked() && !arrow_clicked {
                        self.acts.push(Act::Open(i, e.path.clone()));
                    }
                    if resp.middle_clicked() && e.is_dir {
                        // config MMBOpenNewTab = True, SwitchToNewTab = True
                        self.acts.push(Act::OpenNewTab(i, e.path.clone(), true));
                    }
                    if resp.drag_started() && !in_computer {
                        if selected && !mods.shift {
                            drag = Some((e.path.clone(), selected));
                        } else if let Some(origin) = ui.input(|inp| inp.pointer.press_origin()) {
                            let base = if mods.ctrl || mods.command {
                                self.panes[i].selected.iter().cloned().collect()
                            } else {
                                Vec::new()
                            };
                            marquee = Some((origin + vec2(0.0, self.panes[i].list_offset), base));
                        }
                    }
                    if e.is_dir && !in_computer {
                        self.drop_zones
                            .push((rr, DropTarget::Folder(e.path.clone())));
                    }
                    resp.context_menu(|ui| self.item_menu(ui, i, e));
                }
            })
        });
        let so = out.inner;
        self.panes[i].list_offset = so.state.clamped_offset().y;

        // empty space below the rows
        let used = (n as f32 * rh - so.state.clamped_offset().y).max(0.0);
        if used < list_r.height() - 4.0 {
            let er = Rect::from_min_max(
                pos2(list_r.min.x, list_r.min.y + used),
                pos2(list_r.max.x - 10.0, list_r.max.y),
            );
            let resp = ui.interact(
                er,
                ui.scope_id().with(("listbg", i)),
                Sense::click_and_drag(),
            );
            if resp.drag_started()
                && let Some(origin) = ui.input(|inp| inp.pointer.press_origin()) {
                    let base = if ui.input(|inp| inp.modifiers.ctrl || inp.modifiers.command) {
                        self.panes[i].selected.iter().cloned().collect()
                    } else {
                        self.panes[i].clear_cursor();
                        Vec::new()
                    };
                    marquee = Some((origin + vec2(0.0, self.panes[i].list_offset), base));
                }
            if resp.clicked() {
                self.panes[i].selected.clear();
                self.panes[i].clear_cursor();
            }
            if resp.double_clicked() {
                // config UseDoubleClickToOpenParent = True
                self.acts.push(Act::Up(i));
            }
            resp.context_menu(|ui| self.background_menu(ui, i));
        }
        if let Some((origin, base)) = &marquee
            && let Some(pointer) = ui.input(|inp| inp.pointer.interact_pos()) {
                let selection =
                    Rect::from_two_pos(*origin - vec2(0.0, self.panes[i].list_offset), pointer)
                        .intersect(list_r);
                self.panes[i].selected = base.iter().cloned().collect();
                for row in 0..n {
                    let rr = Rect::from_min_size(
                        pos2(
                            list_r.min.x,
                            list_r.min.y + row as f32 * rh - self.panes[i].list_offset,
                        ),
                        vec2(list_r.width() - 10.0, rh),
                    );
                    if selection.intersects(rr) {
                        let path = listing.entries[self.panes[i].view[row]].path.clone();
                        self.panes[i].selected.insert(path);
                    }
                }
                ui.painter_at(list_r).rect_filled(
                    selection,
                    CornerRadius::ZERO,
                    pal.accent.gamma_multiply(0.15),
                );
                ui.painter_at(list_r).rect_stroke(
                    selection,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, pal.accent),
                    StrokeKind::Inside,
                );
            }
        if ui.input(|inp| inp.pointer.any_released() || !inp.pointer.primary_down()) {
            marquee = None;
        }
        ctx.data_mut(|d| {
            if let Some(state) = marquee {
                d.insert_temp(marquee_id, state);
            } else {
                d.remove::<(egui::Pos2, Vec<PathBuf>)>(marquee_id);
            }
        });
        if n == 0 {
            let msg = if let Some(e) = &listing.error {
                e.clone()
            } else if !self.panes[i].filter.is_empty() {
                "No matching items found".into()
            } else if listing.entries.iter().any(|e| e.hidden) && !self.st.show_hidden {
                "Contains hidden items".into()
            } else {
                "Empty".into()
            };
            ui.painter().text(
                list_r.center_top() + vec2(0.0, 40.0),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(14.0),
                pal.text_weak,
            );
        }
        if let Some(entries) = original_entries {
            self.panes[i].tree_entries = std::mem::replace(&mut listing.entries, entries);
        }
        self.cache.insert(path, listing);
        for (row, mods) in clicks {
            self.set_cursor(i, row, mods);
        }
        for path in toggles { self.toggle_tree_folder(i, path); }
        if let Some((p, selected)) = drag {
            let paths = if selected { self.selection(i) } else { vec![p] };
            egui::DragAndDrop::set_payload(&ctx, DragPaths(paths));
        }
    }

    fn tree_indent(&self, i: usize, e: &Entry, name_width: f32) -> f32 {
        if !self.st.tree_files { return 0.0; }
        let depth = e.path.strip_prefix(self.panes[i].path())
            .map(|p| p.components().count().saturating_sub(1)).unwrap_or(0);
        18.0 + (depth as f32 * 18.0).min((name_width - 80.0).max(0.0))
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_row(
        &mut self,
        ui: &mut Ui,
        i: usize,
        rr: Rect,
        resp: &egui::Response,
        e: &Entry,
        row: usize,
        cols: &[(Col, &'static str, f32, bool)],
        selected: bool,
        is_active: bool,
        dragging: bool,
        cut: &[PathBuf],
        in_computer: bool,
    ) {
        let pal = self.pal;
        let rh = rr.height();
        let p = ui.painter().clone();
        let bg = if dragging && e.is_dir && resp.contains_pointer() {
            blend(pal.panel, pal.accent, 0.5)
        } else if is_active && self.panes[i].cursor == Some(row) {
            pal.cursor(selected)
        } else if selected {
            pal.selection()
        } else if resp.hovered() {
            pal.hover
        } else if row % 2 == 1 {
            pal.row_odd
        } else {
            Color32::TRANSPARENT
        };
        p.rect_filled(rr, CornerRadius::same(1), bg);
        let dim = e.hidden || cut.contains(&e.path);
        let font = FontId::proportional(self.st.font_size);
        let small = FontId::proportional(self.st.font_size - 1.0);
        let weak = pal.text_weak;
        let main = if dim {
            pal.text.gamma_multiply(0.5)
        } else {
            pal.text
        };
        let renaming = self.panes[i]
            .rename
            .as_ref()
            .is_some_and(|(rp, _)| rp == &e.path);
        let tree_indent = self.tree_indent(i, e, cols[0].2);
        let mut x = rr.min.x + 4.0;
        for (c, _, w, right) in cols {
            let cr = Rect::from_min_size(pos2(x, rr.min.y), vec2(*w, rh));
            x += w;
            let put = |s: String, f: FontId, col: Color32| {
                if *right {
                    p.text(
                        cr.right_center() - vec2(8.0, 0.0),
                        Align2::RIGHT_CENTER,
                        s,
                        f,
                        col,
                    );
                } else {
                    p.text(
                        cr.left_center() + vec2(8.0, 0.0),
                        Align2::LEFT_CENTER,
                        s,
                        f,
                        col,
                    );
                }
            };
            match c {
                Col::Name => {
                    let ir = Rect::from_min_size(
                        pos2(cr.min.x + 2.0 + tree_indent, cr.center().y - 8.0),
                        vec2(16.0, 16.0),
                    );
                    if self.st.tree_files && e.is_dir {
                        let expanded = self.panes[i].expanded.contains(&e.path);
                        let center = pos2(ir.min.x - 9.0, cr.center().y);
                        let points = if expanded {
                            vec![center + vec2(-4.0, -2.0), center + vec2(4.0, -2.0), center + vec2(0.0, 3.0)]
                        } else {
                            vec![center + vec2(-2.0, -4.0), center + vec2(-2.0, 4.0), center + vec2(3.0, 0.0)]
                        };
                        p.add(egui::Shape::convex_polygon(points, weak, Stroke::NONE));
                        if expanded {
                            let status = match self.cache.get(&e.path) {
                                Some(l) if l.generation == 0 => Some("Loading…".to_string()),
                                Some(l) if l.error.is_some() => l.error.clone(),
                                Some(l) if l.entries.iter().all(|e| e.hidden && !self.st.show_hidden) => Some("Empty folder".to_string()),
                                None => Some("Loading…".to_string()),
                                _ => None,
                            };
                            if let Some(status) = status {
                                resp.clone().on_hover_text(status);
                            }
                        }
                    }
                    if in_computer {
                        icons::drive(&p, ir, weak);
                    } else if e.is_dir {
                        icons::folder(&p, ir, self.folder_color(&e.path));
                    } else {
                        icons::entry(&p, ir, e.kind, e.ext(), pal.dark, false);
                    }
                    if !renaming {
                        let name = if self.st.show_extensions { e.name() } else { e.stem() };
                        let tx = ir.max.x + 8.0;
                        p.text(
                            pos2(tx, cr.center().y),
                            Align2::LEFT_CENTER,
                            fit_text(&p, name, &font, cr.max.x - tx - 6.0),
                            font.clone(),
                            main,
                        );
                    }
                }
                Col::Ext => {
                    let s = if e.is_dir {
                        "[DIR]".to_string()
                    } else {
                        e.ext().to_lowercase()
                    };
                    put(
                        fit_text(&p, &s, &small, cr.width() - 6.0),
                        small.clone(),
                        weak,
                    );
                }
                Col::Age => {
                    let s = fsx::fmt_age(e.modified);
                    match fsx::age_color(e.modified).filter(|_| self.st.age_colors && !in_computer)
                    {
                        Some(ac) => {
                            // coloured badge
                            let g = p.layout_no_wrap(s.clone(), small.clone(), Color32::BLACK);
                            let br = Rect::from_min_size(
                                pos2(cr.max.x - 12.0 - g.size().x, cr.center().y - 8.0),
                                vec2(g.size().x + 8.0, 16.0),
                            );
                            p.rect_filled(br, CornerRadius::same(1), ac);
                            put(s, small.clone(), Color32::from_rgb(0x1a, 0x1a, 0x1e));
                        }
                        None => put(s, small.clone(), weak),
                    }
                }
                Col::Size => {
                    if !e.is_dir || in_computer {
                        // config ColorizeLargeFileSize: sizes over 1 GB stand out
                        let col = if e.size >= 1_000_000_000 {
                            pal.text_important
                        } else {
                            main
                        };
                        put(fsx::fmt_size(e.size, self.st.mebibytes), small.clone(), col);
                    }
                }
                Col::Date => put(fsx::fmt_date(e.modified), small.clone(), weak),
                Col::Time => put(fsx::fmt_time(e.modified), small.clone(), weak),
            }
        }
        if renaming {
            let focus = self.panes[i].rename_focus;
            let enter = ui.input(|inp| inp.key_pressed(Key::Enter));
            if let Some((_, text)) = self.panes[i].rename.as_mut() {
                let er = Rect::from_min_size(
                    pos2(rr.min.x + 26.0 + tree_indent, rr.min.y + 1.0),
                    vec2((cols[0].2 - 30.0 - tree_indent).max(30.0), rh - 2.0),
                );
                let te = egui::TextEdit::singleline(text)
                    .font(FontId::proportional(self.st.font_size))
                    .margin(vec2(4.0, 1.0));
                let r = ui.put(er, te);
                if focus {
                    r.request_focus();
                    self.panes[i].rename_focus = false;
                } else if r.lost_focus() {
                    if enter {
                        self.acts.push(Act::CommitRename(i));
                    } else {
                        // clicking outside cancels (config RenameClickingOutside)
                        self.panes[i].rename = None;
                    }
                }
            }
        }
    }

    /// Both sidebar shortcuts and file views edit the same path-based setting.
    pub fn path_color_menu(&mut self, ui: &mut Ui, label: &str, paths: Vec<PathBuf>) {
        let current = paths.first().and_then(|p| self.tag_color(p));
        ui.menu_button(label, |ui| {
            for (k, (name, c)) in TAGS.iter().enumerate() {
                let color = hex(c);
                let selected = current == Some(color)
                    && paths.iter().all(|p| self.tag_color(p) == Some(color));
                // Draw the swatch as geometry: the bullet glyph can be missing in
                // the platform font and appear as a small replacement box.
                let padding = ui.spacing().button_padding.x;
                let response = ui.scope(|ui| {
                    ui.spacing_mut().button_padding.x += 14.0;
                    ui.selectable_label(selected, RichText::new(*name).color(color))
                }).inner;
                ui.painter().circle_filled(
                    pos2(response.rect.left() + padding + 5.0, response.rect.center().y),
                    4.0,
                    color,
                );
                if response.clicked() {
                    self.acts.push(Act::TagPaths(paths.clone(), Some(k as u8)));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Clear color").clicked() {
                self.acts.push(Act::TagPaths(paths.clone(), None));
                ui.close();
            }
        });
    }

    fn item_menu(&mut self, ui: &mut Ui, i: usize, e: &Entry) {
        let act = |ui: &mut Ui, label: &str, a: Act, acts: &mut Vec<Act>| {
            if ui.button(label).clicked() {
                acts.push(a);
                ui.close();
            }
        };
        let mut acts = Vec::new();
        // The listing is temporarily outside the cache while rows are painted,
        // so selection(i) cannot resolve its entries here.
        let paths: Vec<PathBuf> = if self.panes[i].selected.contains(&e.path) {
            self.panes[i].selected.iter().cloned().collect()
        } else {
            vec![e.path.clone()]
        };
        if !e.is_dir && crate::archive::is_archive(&e.path) {
            let archives: Vec<PathBuf> = paths.iter().filter(|p| crate::archive::is_archive(p)).cloned().collect();
            act(ui, "Extract here", Act::Extract(archives.clone(), None), &mut acts);
            let other = self.panes[self.other(i)].path().to_path_buf();
            if self.panes.len() > 1 && !crate::app::is_computer(&other) {
                act(ui, "Extract to the other panel", Act::Extract(archives, Some(other)), &mut acts);
            }
        } else {
            act(ui, "Open", Act::Open(i, e.path.clone()), &mut acts);
        }
        if !e.is_dir {
            let apps = open_with_apps(&e.path, ui.ctx());
            ui.menu_button("Open with", |ui| {
                match &apps {
                    None => {
                        ui.label("Finding applications...");
                    }
                    Some(apps) if apps.is_empty() => {
                        ui.label("No recommended apps found");
                    }
                    Some(apps) => {
                        for a in apps {
                            act(
                                ui,
                                &a.name,
                                Act::OpenWith(e.path.clone(), Some(a.clone())),
                                &mut acts,
                            );
                        }
                    }
                }
                ui.separator();
                act(ui, "Choose another app...", Act::OpenWith(e.path.clone(), None), &mut acts);
            });
        }
        if e.is_dir {
            ui.menu_button("Open in...", |ui| {
                act(
                    ui,
                    "New tab",
                    Act::OpenNewTab(i, e.path.clone(), true),
                    &mut acts,
                );
                act(
                    ui,
                    "New panel",
                    Act::OpenPanel(i, Some(e.path.clone())),
                    &mut acts,
                );
                act(
                    ui,
                    "New tab in other panel",
                    Act::OpenOtherPane(i, e.path.clone()),
                    &mut acts,
                );
                act(ui, "Command shell", Act::Terminal(e.path.clone()), &mut acts);
            });
        }
        ui.separator();
        act(ui, "Copy                 Ctrl+C", Act::Copy(i), &mut acts);
        act(ui, "Cut                   Ctrl+X", Act::Cut(i), &mut acts);
        if self.can_paste_files() && e.is_dir {
            act(
                ui,
                "Paste into folder",
                Act::Paste(i, Some(e.path.clone())),
                &mut acts,
            );
        }
        if self.panes.len() > 1 {
            act(
                ui,
                "Copy to the other panel",
                Act::ToOther(i, false),
                &mut acts,
            );
            act(
                ui,
                "Move to the other panel",
                Act::ToOther(i, true),
                &mut acts,
            );
        }
        #[cfg(target_os = "linux")]
        if self.panes.len() > 1 {
            ui.menu_button("Create link in other panel", |ui| {
                act(ui, "Symbolic link", Act::LinkToOther(i, false), &mut acts);
                act(ui, "Hard link (regular files, same filesystem)", Act::LinkToOther(i, true), &mut acts);
            });
        }
        ui.separator();
        act(ui, "Rename               F2", Act::Rename(i), &mut acts);
        act(
            ui,
            "Duplicate            Ctrl+D",
            Act::Duplicate(i),
            &mut acts,
        );
        act(ui, "Copy as path", Act::CopyPath(i), &mut acts);
        #[cfg(target_os = "linux")]
        act(ui, "Properties / Permissions   Alt+Enter", Act::Properties(e.path.clone()), &mut acts);
        self.path_color_menu(ui, if e.is_dir { "Folder color" } else { "Tag (Color)" }, paths.clone());
        if e.is_dir {
            let groups: Vec<String> = self.st.favorites.iter().map(|g| g.name.clone()).collect();
            ui.menu_button("Add to favorites", |ui| {
                for (g, name) in groups.iter().enumerate() {
                    act(ui, name, Act::AddFavorite(g, e.path.clone()), &mut acts);
                }
            });
        }
        act(ui, "Compress...", Act::Compress(i, paths), &mut acts);
        ui.separator();
        if ui
            .button(RichText::new("Delete                Del").color(hex("#ef7063")))
            .clicked()
        {
            acts.push(Act::Delete(i));
            ui.close();
        }
        self.acts.extend(acts);
    }

    fn background_menu(&mut self, ui: &mut Ui, i: usize) {
        let mut acts = Vec::new();
        if self.can_paste_files() && ui.button("Paste                 Ctrl+V").clicked() {
            acts.push(Act::Paste(i, None));
            ui.close();
        }
        ui.menu_button("New...", |ui| {
            if ui.button("Folder     Ctrl+Shift+N").clicked() {
                acts.push(Act::NewFolder(i));
                ui.close();
            }
            if ui.button("File          Ctrl+N").clicked() {
                acts.push(Act::NewFile(i));
                ui.close();
            }
        });
        if ui.button("Select all          Ctrl+A").clicked() {
            acts.push(Act::SelectAll(i));
            ui.close();
        }
        if ui.button("Copy folder path").clicked() {
            ui.ctx()
                .copy_text(self.panes[i].path().display().to_string());
            ui.close();
        }
        if ui.button("Open in command shell   F10").clicked() {
            acts.push(Act::Terminal(self.panes[i].path().to_path_buf()));
            ui.close();
        }
        #[cfg(target_os = "linux")]
        if ui.button("Folder properties / Permissions").clicked() {
            acts.push(Act::Properties(self.panes[i].path().to_path_buf()));
            ui.close();
        }
        if ui.button("Refresh everything        F5").clicked() {
            acts.push(Act::Refresh);
            ui.close();
        }
        self.acts.extend(acts);
    }

    // ================================================================= details / preview

    pub fn focused_item(&self, i: usize) -> Option<Entry> {
        self.cursor_entry(i).cloned()
    }

    pub fn preview_box(&mut self, ui: &mut Ui, r: Rect, e: Option<&Entry>) {
        let pal = self.pal;
        let p = ui.painter_at(r);
        let current = e.is_some_and(|e| self.preview.path.as_ref() == Some(&e.path));
        match (&self.preview.content, current) {
            (PreviewContent::Image(tex, ..) | PreviewContent::Model(tex, _) | PreviewContent::Page { texture: tex, .. }, true) => {
                let s = tex.size_vec2();
                let scale = (r.width() / s.x).min(r.height() / s.y).min(1.0);
                let ir = Rect::from_center_size(r.center(), s * scale);
                p.image(tex.id(), ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            (PreviewContent::Animation(frames, _) | PreviewContent::Info(_, Some((frames, _)), _), true) if !frames.is_empty() => {
                let tex = &frames[0].0;
                let s = tex.size_vec2();
                let scale = (r.width() / s.x).min(r.height() / s.y).min(1.0);
                let ir = Rect::from_center_size(r.center(), s * scale);
                p.image(
                    tex.id(),
                    ir,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            (PreviewContent::Text(t), true) if r.width() > 160.0 && !t.text.is_empty() => {
                p.rect_filled(
                    r,
                    CornerRadius::same(1),
                    if pal.dark {
                        blend(pal.window, pal.panel, 0.5)
                    } else {
                        Color32::WHITE
                    },
                );
                let tr = r.shrink(6.0);
                let lines = ((tr.height() / 13.0) as usize).max(1);
                let snippet: String = t
                    .text
                    .lines()
                    .take(lines)
                    .map(|l| l.chars().take(200).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n");
                let g = p.layout(snippet, FontId::monospace(10.5), pal.text_weak, tr.width());
                p.galley(tr.min, g, pal.text_weak);
            }
            _ => {
                let s = r.width().min(r.height()) * 0.7;
                let ir = Rect::from_center_size(r.center(), vec2(s, s));
                match e {
                    Some(e) if e.is_dir => icons::folder(&p, ir, self.folder_color(&e.path)),
                    Some(e) => icons::entry(&p, ir, e.kind, e.ext(), pal.dark, true),
                    None => icons::folder(&p, ir, blend(pal.panel, fsx::Kind::Folder.color(), 0.6)),
                }
            }
        }
    }

    pub fn details_rows(&self, e: &Entry) -> Vec<(&'static str, String)> {
        let mb = self.st.mebibytes;
        let exact_size = || format!("{}  ({} bytes)", fsx::fmt_size(e.size, mb), e.size);
        let kind = if e.is_dir {
            "Directory".to_string()
        } else if e.ext().is_empty() {
            e.kind.label().to_string()
        } else {
            format!("{} (.{})", e.kind.label(), e.ext().to_lowercase())
        };
        let mut v = vec![
            ("Name", e.name().to_string()),
            ("Type", kind),
            ("Created", fsx::fmt_datetime(e.created)),
            ("Modified", fsx::fmt_datetime(e.modified)),
            ("Age", fsx::fmt_age(e.modified)),
        ];
        let current = self.preview.path.as_ref() == Some(&e.path);
        match &self.preview.content {
            PreviewContent::Folder {
                folders,
                files,
                size,
            } if current => {
                v.push(("Contains", format!("{folders} folders, {files} files")));
                v.push((
                    "Size",
                    format!("{} in files at top level", fsx::fmt_size(*size, mb)),
                ));
            }
            PreviewContent::Page { pages, .. } if current => {
                v.push(("Size", exact_size()));
                v.push(("Pages", pages.to_string()));
            }
            PreviewContent::Image(_, [w, h], _) | PreviewContent::Animation(_, [w, h]) if current => {
                v.push(("Size", exact_size()));
                v.push(("Resolution", format!("{w} × {h}")));
            }
            _ if !e.is_dir => v.push(("Size", exact_size())),
            _ => {}
        }
        let mut attrs = Vec::new();
        if e.hidden {
            attrs.push("hidden");
        }
        if e.readonly {
            attrs.push("read-only");
        }
        if !attrs.is_empty() {
            v.push(("Attributes", attrs.join(", ")));
        }
        v
    }

    pub fn details_panel(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let pal = self.pal;
        ui.painter().line_segment(
            [pos2(r.min.x + 8.0, r.min.y), pos2(r.max.x - 8.0, r.min.y)],
            Stroke::new(1.0, pal.raised),
        );
        let e = self.focused_item(i);
        if self.active == i {
            self.want_preview(e.as_ref().map(PreviewKey::of), ui.ctx());
        }
        let mut rr = r.shrink(8.0);
        let pw = (rr.height() * 1.6).min(rr.width() * 0.35);
        let pv = take_left(&mut rr, pw);
        self.preview_box(ui, pv, e.as_ref());
        let p = ui.painter_at(rr);
        let (n, sel, total) = self.counts(i);
        let summary = format!(
            "{n} items{}",
            if sel > 0 {
                format!("   ·   {sel} selected")
            } else {
                String::new()
            }
        );
        p.text(
            rr.right_top(),
            Align2::RIGHT_TOP,
            summary,
            FontId::proportional(13.0),
            pal.text,
        );
        p.text(
            rr.right_top() + vec2(0.0, 18.0),
            Align2::RIGHT_TOP,
            fsx::fmt_size(total, self.st.mebibytes),
            FontId::proportional(13.0),
            pal.text_weak,
        );
        if let Some(e) = &e {
            let x0 = rr.min.x + 12.0;
            let x1 = x0 + 78.0;
            let lh = 14.0;
            let f = FontId::proportional(11.0);
            for (k, (key, val)) in self.details_rows(e).iter().enumerate() {
                let y = rr.min.y + k as f32 * lh;
                if y + lh > rr.max.y {
                    break;
                }
                p.text(
                    pos2(x0, y),
                    Align2::LEFT_TOP,
                    *key,
                    f.clone(),
                    pal.text_weak,
                );
                p.text(
                    pos2(x1, y),
                    Align2::LEFT_TOP,
                    fit_text(&p, val, &f, rr.max.x - x1 - 130.0),
                    f.clone(),
                    pal.text,
                );
            }
        }
    }

    pub fn preview_pane(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let pal = self.pal;
        ui.painter().line_segment(
            [pos2(r.min.x, r.min.y + 8.0), pos2(r.min.x, r.max.y - 8.0)],
            Stroke::new(1.0, pal.raised),
        );
        let e = self.focused_item(i);
        if self.active == i {
            self.want_preview(e.as_ref().map(PreviewKey::of), ui.ctx());
        }
        let mut rr = r.shrink(10.0);
        let (n, sel, total) = self.counts(i);
        let top = take_top(&mut rr, 40.0);
        let p = ui.painter_at(top);
        p.text(
            top.left_top(),
            Align2::LEFT_TOP,
            format!("{n} items"),
            FontId::proportional(14.0),
            pal.text,
        );
        if sel > 0 {
            p.text(
                top.left_top() + vec2(0.0, 18.0),
                Align2::LEFT_TOP,
                format!("{sel} selected"),
                FontId::proportional(12.0),
                pal.text_important,
            );
        }
        p.text(
            top.right_top(),
            Align2::RIGHT_TOP,
            fsx::fmt_size(total, self.st.mebibytes),
            FontId::proportional(12.0),
            pal.text_weak,
        );
        let rows = e.as_ref().map(|e| self.details_rows(e)).unwrap_or_default();
        let det_h = rows.len() as f32 * 15.0 + 8.0;
        let dh = det_h.min(rr.height() * 0.5);
        let det = take_bottom(&mut rr, dh);
        self.preview_box(ui, rr, e.as_ref());
        let p = ui.painter_at(det);
        let f = FontId::proportional(11.0);
        for (k, (key, val)) in rows.iter().enumerate() {
            let y = det.min.y + 4.0 + k as f32 * 15.0;
            p.text(
                pos2(det.min.x, y),
                Align2::LEFT_TOP,
                *key,
                f.clone(),
                pal.text_weak,
            );
            p.text(
                pos2(det.min.x + 72.0, y),
                Align2::LEFT_TOP,
                fit_text(&p, val, &f, det.width() - 74.0),
                f.clone(),
                pal.text,
            );
        }
    }

    pub fn counts(&self, i: usize) -> (usize, usize, u64) {
        let pane = &self.panes[i];
        let Some(entries) = self.pane_entries(i) else {
            return (0, 0, 0);
        };
        let sel = pane.selected.len();
        let items = pane.view.iter().filter_map(|&k| entries.get(k));
        let total: u64 = if sel > 0 {
            items
                .filter(|e| pane.selected.contains(&e.path))
                .map(|e| e.size)
                .sum()
        } else {
            items.filter(|e| !e.is_dir).map(|e| e.size).sum()
        };
        (pane.view.len(), sel, total)
    }

    pub fn status_bar(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let pal = self.pal;
        let p = ui.painter_at(r);
        p.line_segment(
            [
                pos2(r.min.x + 8.0, r.min.y + 0.5),
                pos2(r.max.x - 8.0, r.min.y + 0.5),
            ],
            Stroke::new(1.0, pal.raised),
        );
        let (n, sel, total) = self.counts(i);
        let mut s = format!("{n} items");
        if sel > 0 {
            s += &format!(
                "   ·   {sel} selected   ·   {}",
                fsx::fmt_size(total, self.st.mebibytes)
            );
        }
        let f = FontId::proportional(11.5);
        let summary_width = p.layout_no_wrap(s.clone(), f.clone(), pal.text_weak).size().x;
        p.text(
            pos2(r.min.x + 10.0, r.center().y),
            Align2::LEFT_CENTER,
            s,
            f.clone(),
            pal.text_weak,
        );
        let mut right = vec![];
        if self.st.show_hidden {
            right.push("hidden shown".to_string());
        }
        if let Some((v, cut)) = &self.clip {
            right.push(format!(
                "{} {} on clipboard",
                v.len(),
                if *cut { "cut" } else { "copied" }
            ));
        }
        let right_text = right.join("   ·   ");
        let right_width = p.layout_no_wrap(right_text.clone(), f.clone(), pal.text_weak).size().x;
        if summary_width + right_width + 30.0 <= r.width() {
            p.text(
                pos2(r.max.x - 10.0, r.center().y),
                Align2::RIGHT_CENTER,
                right_text,
                f,
                pal.text_weak,
            );
        }
    }
}
