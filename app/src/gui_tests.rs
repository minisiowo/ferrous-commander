//! Drives the real UI headlessly with simulated mouse and keyboard input.

#[test]
fn the_configuration_folder_opens_in_a_new_tab_of_the_app() {
    let root = std::env::temp_dir().join(format!("fc_config_folder_{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::new(Some(root.join("unused.png")));
    let tabs = app.panes[app.active].tabs.len();
    app.settings_open = true;
    app.show_configuration_folder(Ok(root.clone()));
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert!(!app.settings_open);
    assert_eq!(app.panes[app.active].tabs.len(), tabs + 1);
    assert_eq!(app.panes[app.active].path(), root.as_path());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tree_marquee_matches_painted_rows_after_scrolling() {
    let root = std::env::temp_dir().join(format!("fc_tree_marquee_{}", std::process::id()));
    let nested = root.join("branch/nested");
    std::fs::create_dir_all(&nested).unwrap();
    for n in 0..100 {
        std::fs::write(nested.join(format!("file{n:03}.txt")), b"x").unwrap();
        std::fs::write(root.join(format!("root{n:03}.txt")), b"x").unwrap();
    }
    let ctx = egui::Context::default();
    let mut app = App::new(Some(root.join("unused.png")));
    app.panes[0].tab().path = root.clone();
    app.st.tree_files = true;
    app.st.show_extensions = true;
    app.panes[0].expanded.insert(root.join("branch"));
    app.panes[0].expanded.insert(nested.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.panes[0].view.len() != 202 {
        assert!(std::time::Instant::now() < deadline, "tree load timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Target painted labels, without repeating the list's row arithmetic.
    for (filter, additive, backwards) in [("", false, false), ("file", false, true), ("", true, true)] {
        app.panes[0].filter = filter.into();
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        let row = app.panes[0].view.iter().position(|&k|
            app.panes[0].tree_entries[k].path == nested.join("file060.txt")).unwrap();
        app.panes[0].scroll_to = Some(row);
        for _ in 0..3 { frame(&ctx, &mut app, vec![], Modifiers::NONE); }
        let output = ctx.run_ui(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))),
            ..Default::default()
        }, |ui| app.draw(ui));
        let pane = app.pane_rects.iter().find(|(i, _)| *i == 0).unwrap().1;
        let label_center = |name: &str| output.shapes.iter().find_map(|shape| {
            if let egui::epaint::Shape::Text(text) = &shape.shape {
                let rect = Rect::from_min_size(text.pos, text.galley.size());
                if text.galley.job.text == name && pane.contains(rect.center())
                    && shape.clip_rect.contains(rect.center()) {
                    return Some(rect.center().y);
                }
            }
            None
        }).unwrap_or_else(|| panic!("missing visible label {name}"));
        let a = pos2(pane.min.x + 100.0, label_center("file050.txt"));
        let b = pos2(pane.min.x + 180.0, label_center("file052.txt"));
        let (start, end) = if backwards { (b, a) } else { (a, b) };
        app.panes[0].selected.clear();
        let kept = root.join("root099.txt");
        if additive { app.panes[0].selected.insert(kept.clone()); }
        let mods = if additive { Modifiers::CTRL } else { Modifiers::NONE };
        frame(&ctx, &mut app, vec![Event::PointerMoved(start)], mods);
        frame(&ctx, &mut app, vec![Event::PointerButton {
            pos: start, button: PointerButton::Primary, pressed: true, modifiers: mods,
        }], mods);
        frame(&ctx, &mut app, vec![Event::PointerMoved(end)], mods);
        frame(&ctx, &mut app, vec![Event::PointerButton {
            pos: end, button: PointerButton::Primary, pressed: false, modifiers: mods,
        }], mods);
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        let mut expected = std::collections::HashSet::from([
            nested.join("file050.txt"), nested.join("file051.txt"), nested.join("file052.txt"),
        ]);
        if additive { expected.insert(kept); }
        assert_eq!(app.panes[0].selected, expected,
            "marquee must follow painted nested rows (filter={filter}, additive={additive})");
    }
    std::fs::remove_dir_all(root).unwrap();
}

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

use crate::app::App;
use crate::ui::{HEADER_H, TAB_H};

#[test]
fn drag_tip_stays_readable_at_window_edges() {
    use crate::app::{DragPaths, DropTarget};
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused-drag.png")));
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(550.0, 420.0));
    for pos in [pos2(5.0, 5.0), pos2(540.0, 410.0), pos2(280.0, 200.0)] {
        for mods in [Modifiers::NONE, Modifiers::COMMAND] {
            app.drop_zones = vec![(screen, DropTarget::Folder(std::path::PathBuf::from(
                "Desktop with a very long destination folder name that must be truncated")))];
            egui::DragAndDrop::set_payload(&ctx, DragPaths(vec![std::path::PathBuf::from("source.txt")]));
            for pass in 0..3 {
                let mut output = ctx.run_ui(RawInput {
                    screen_rect: Some(screen),
                    events: vec![Event::ModifiersChanged(mods), Event::PointerMoved(pos)],
                    ..Default::default()
                }, |ui| app.handle_drop(ui.ctx()));
                output.textures_delta.clear();
                if pass < 2 { continue; } // Areas first perform an invisible sizing pass.
                let texts: Vec<_> = output.shapes.iter().filter_map(|shape| {
                    if let egui::epaint::Shape::Text(t) = &shape.shape { Some(t) } else { None }
                }).collect();
                assert_eq!(texts.len(), 3, "operation, destination and modifier hint must be visible");
                for t in texts {
                    assert_eq!(t.galley.rows.len(), 1, "drag text must not wrap: {}", t.galley.job.text);
                    let rect = Rect::from_min_size(t.pos, t.galley.size());
                    assert!(screen.contains_rect(rect), "drag text must stay inside the window: {rect:?}");
                }
            }
        }
    }
}

fn frame(ctx: &egui::Context, app: &mut App, mut events: Vec<Event>, mods: Modifiers) {
    events.insert(0, Event::ModifiersChanged(mods));
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| app.draw(ui));
    out.textures_delta.clear();
}

fn click(ctx: &egui::Context, app: &mut App, pos: Pos2, mods: Modifiers) {
    frame(ctx, app, vec![Event::PointerMoved(pos)], mods);
    frame(
        ctx,
        app,
        vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: mods,
        }],
        mods,
    );
    frame(
        ctx,
        app,
        vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: mods,
        }],
        mods,
    );
    frame(ctx, app, vec![], Modifiers::NONE);
}

fn key(ctx: &egui::Context, app: &mut App, k: Key, mods: Modifiers) {
    let ev = |pressed| Event::Key {
        key: k,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: mods,
    };
    frame(ctx, app, vec![ev(true)], mods);
    frame(ctx, app, vec![ev(false)], mods);
    frame(ctx, app, vec![], Modifiers::NONE);
}

#[test]
fn folder_columns_have_distinct_widget_ids() {
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused-columns.png")));
    app.st.show_columns = true;
    for pane in &mut app.panes {
        pane.tabs[pane.active].path = std::env::current_dir().unwrap();
    }
    for layout in [crate::settings::Layout::Standard, crate::settings::Layout::Columns] {
        app.st.layout = layout;
        for _ in 0..3 {
            let mut output = ctx.run_ui(RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(3000.0, 900.0))),
                ..Default::default()
            }, |ui| app.draw(ui));
            output.textures_delta.clear();
            for shape in &output.shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape {
                    assert!(!text.galley.job.text.contains("use of widget ID"),
                        "{}", text.galley.job.text);
                }
            }
        }
    }
}

#[test]
fn short_stacked_panels_keep_summary_clear() {
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused_summary.png")));
    app.st.show_hidden = false;
    for layout in [crate::settings::Layout::Standard, crate::settings::Layout::Columns] {
        app.st.layout = layout;
        for ratio in [0.12, 0.20, 0.88] {
            app.tiles = crate::tiles::Tile::Split {
                vertical: true, ratio,
                a: Box::new(crate::tiles::Tile::Leaf(0)),
                b: Box::new(crate::tiles::Tile::Leaf(1)),
            };
            let mut output = ctx.run_ui(RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))),
                ..Default::default()
            }, |ui| app.draw(ui));
            output.textures_delta.clear();
            for (_, pane) in &app.pane_rects {
                let status = Rect::from_min_max(
                    pos2(pane.min.x + 1.0, pane.max.y - 1.0 - crate::ui::STATUS_H),
                    pane.max - vec2(1.0, 1.0),
                );
                let mut summaries = 0;
                for shape in &output.shapes {
                    if let egui::epaint::Shape::Text(text) = &shape.shape {
                        let visible = Rect::from_min_size(text.pos, text.galley.size())
                            .intersect(shape.clip_rect).intersect(status);
                        if visible.is_positive() {
                            assert!(text.galley.job.text.contains("items"),
                                "control overlaps summary at ratio {ratio}: {}", text.galley.job.text);
                            summaries += 1;
                        }
                    }
                }
                assert_eq!(summaries, 1, "each panel retains its own visible summary");
            }
        }
    }
}

#[test]
fn click_select_rename_and_panels() {
    let dir = std::env::temp_dir().join(format!("fc_gui_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.join(n), b"x").unwrap();
    }
    crate::app::set_start_dir(dir.clone());
    let ctx = egui::Context::default();
    let mut app = App::new(Some(dir.join("unused.png")));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.panes[0].view.len() != 3 {
        assert!(
            std::time::Instant::now() < deadline,
            "background folder load timed out"
        );
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let r = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    let rh = app.row_h();
    let top = r.min.y + 1.0 + TAB_H + HEADER_H + 22.0;
    let row = |k: usize| pos2(r.min.x + 80.0, top + rh * k as f32 + rh / 2.0);

    click(&ctx, &mut app, row(0), Modifiers::NONE);
    assert!(
        app.panes[0].selected.contains(&dir.join("a.txt")),
        "click selects: {:?}",
        app.panes[0].selected
    );
    click(&ctx, &mut app, row(2), Modifiers::CTRL);
    assert_eq!(app.panes[0].selected.len(), 2, "ctrl+click adds");
    click(&ctx, &mut app, row(1), Modifiers::SHIFT);
    assert_eq!(
        app.panes[0].selected.len(),
        2,
        "shift+click selects a range from the anchor"
    );
    // Clicking empty space leaves no outlined row, and nothing for Delete to act on.
    click(&ctx, &mut app, row(0), Modifiers::NONE);
    assert_eq!(app.panes[0].cursor, Some(0));
    click(&ctx, &mut app, row(8), Modifiers::NONE);
    assert!(app.panes[0].selected.is_empty());
    assert_eq!(app.panes[0].cursor, None, "no outline after clicking empty space");
    assert!(app.selection(0).is_empty(), "commands have no hidden target");
    key(&ctx, &mut app, Key::ArrowDown, Modifiers::NONE);
    assert_eq!(app.panes[0].cursor, Some(0), "the keyboard brings the outline back");
    // A marquee from empty space replaces the outlined row as well.
    drag_mouse(&ctx, &mut app, row(8), row(1));
    assert_eq!(app.panes[0].cursor, None, "no outline left after a marquee");
    assert!(!app.panes[0].selected.is_empty());
    click(&ctx, &mut app, row(0), Modifiers::NONE);
    click(&ctx, &mut app, row(2), Modifiers::SHIFT);
    assert_eq!(app.panes[0].selected.len(), 3);

    key(&ctx, &mut app, Key::F2, Modifiers::NONE);
    assert!(app.panes[0].rename.is_some(), "F2 starts renaming");

    let before = app.panes.len();
    key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    app.panes[0].rename = None;
    key(
        &ctx,
        &mut app,
        Key::Enter,
        Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.panes.len(),
        before + 1,
        "Ctrl+Shift+Enter opens a panel"
    );
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert_eq!(app.pane_rects.len(), before + 1);
    key(
        &ctx,
        &mut app,
        Key::Q,
        Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        },
    );
    assert_eq!(app.panes.len(), before, "Ctrl+Shift+Q closes it");

    // Drag from empty list space across all three file rows.
    let start = pos2(r.min.x + 180.0, top + rh * 5.0);
    let end = pos2(r.min.x + 45.0, top + 1.0);
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerMoved(start)],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerButton {
            pos: start,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerMoved(end)],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerButton {
            pos: end,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert_eq!(
        app.panes[0].selected.len(),
        3,
        "rubber band selects every intersected file"
    );

    // Opening from the left must split the last panel on the right.
    let left = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    app.open_panel(0, None);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert_eq!(
        app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1,
        left
    );
    app.close_panel(app.panes.len() - 1);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);

    // Move the left panel to the bottom of the right one by dragging its only tab.
    let tab = app.panes[0].tabs.pop().unwrap();
    app.panes[0].tabs = vec![tab];
    app.panes[0].active = 0;
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let right = app.pane_rects.iter().find(|(k, _)| *k == 1).unwrap().1;
    let start = pos2(left.min.x + 60.0, left.min.y + TAB_H / 2.0);
    let end = pos2(right.center().x, right.max.y - 30.0);
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerMoved(start)],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerButton {
            pos: start,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerMoved(end)],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerMoved(end)],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerButton {
            pos: end,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let moved = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    let other = app.pane_rects.iter().find(|(k, _)| *k == 1).unwrap().1;
    assert!(
        moved.min.y > other.min.y,
        "dragging the only tab moves its panel below target"
    );
    app.tiles = crate::tiles::Tile::Split {
        vertical: false,
        ratio: 0.5,
        a: Box::new(crate::tiles::Tile::Leaf(0)),
        b: Box::new(crate::tiles::Tile::Leaf(1)),
    };
    frame(&ctx, &mut app, vec![], Modifiers::NONE);

    // drag the divider between the first two panels to resize them
    let a = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    let start = pos2(a.max.x + 2.0, a.center().y);
    let end = pos2(start.x - 200.0, start.y);
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerMoved(start)],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerButton {
            pos: start,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    for k in 1..=10 {
        frame(
            &ctx,
            &mut app,
            vec![Event::PointerMoved(pos2(
                start.x - 20.0 * k as f32,
                start.y,
            ))],
            Modifiers::NONE,
        );
    }
    frame(
        &ctx,
        &mut app,
        vec![Event::PointerButton {
            pos: end,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let a2 = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    assert!(
        (a.width() - a2.width() - 200.0).abs() < 8.0,
        "divider drag resizes: {} -> {}",
        a.width(),
        a2.width()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sidebar_drag_and_offline_location() {
    use crate::settings::{FavGroup, SidebarSection};
    use crate::app::{Dialog, DropTarget};
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused-sidebar.png")));
    app.st.favorites = vec![
        FavGroup::new("First", vec![]),
        FavGroup::new("Second", vec![]),
    ];
    // Keep the dragged groups above live drive/cloud discovery, whose completion
    // can otherwise move the headings between pointer-down and pointer-up.
    app.st.sidebar_order = vec![SidebarSection::Favorite(0), SidebarSection::Favorite(1),
        SidebarSection::Drives, SidebarSection::Cloud, SidebarSection::History];
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let group_rect = |app: &App, index| app.drop_zones.iter().find_map(|(r, t)| {
        matches!(t, DropTarget::Favorites(g) if *g == index).then_some(*r)
    }).unwrap();
    let first = group_rect(&app, 0);
    let second = group_rect(&app, 1);
    let source = first.min + vec2(20.0, 15.0);
    let target = pos2(second.min.x + 30.0, second.max.y - 8.0);
    frame(&ctx, &mut app, vec![Event::PointerMoved(source)], Modifiers::NONE);
    frame(&ctx, &mut app, vec![Event::PointerButton { pos: source, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Modifiers::NONE);
    frame(&ctx, &mut app, vec![Event::PointerMoved(source + vec2(0.0, 12.0))], Modifiers::NONE);
    frame(&ctx, &mut app, vec![Event::PointerMoved(target)], Modifiers::NONE);
    frame(&ctx, &mut app, vec![Event::PointerButton { pos: target, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    let order = &app.st.sidebar_order;
    assert!(order.iter().position(|s| *s == SidebarSection::Favorite(1)) < order.iter().position(|s| *s == SidebarSection::Favorite(0)), "mouse drag reorders groups: {order:?}");
    let offline = if cfg!(windows) { r"\\offline-server\share\folder" } else { "/offline-server/share/folder" };
    app.dialog = Some(Dialog::AddLocation { group: 0, name: offline.into(), focus: true });
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    assert!(app.st.favorites[0].paths.contains(&std::path::PathBuf::from(offline)), "offline share is saved without probing it");
    app.st.show_cloud = false;
    let saved = serde_json::to_string(&app.st).unwrap();
    let restored: crate::settings::Settings = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.sidebar_order, app.st.sidebar_order);
    assert!(!restored.show_cloud);
    assert_eq!(restored.favorites[0].paths, app.st.favorites[0].paths);
}

fn drag_mouse(ctx: &egui::Context, app: &mut App, source: Pos2, target: Pos2) {
    frame(ctx, app, vec![Event::PointerMoved(source)], Modifiers::NONE);
    frame(ctx, app, vec![Event::PointerButton { pos: source, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Modifiers::NONE);
    frame(ctx, app, vec![Event::PointerMoved(source + vec2(0.0, 6.0))], Modifiers::NONE);
    frame(ctx, app, vec![Event::PointerMoved(target)], Modifiers::NONE);
    frame(ctx, app, vec![Event::PointerMoved(target)], Modifiers::NONE);
    frame(ctx, app, vec![Event::PointerButton { pos: target, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    frame(ctx, app, vec![], Modifiers::NONE);
}

#[test]
fn sidebar_shortcuts_rename_and_drag() {
    use crate::app::{Dialog, DragPaths, DropTarget};
    use crate::settings::{FavGroup, SidebarSection};
    use std::path::PathBuf;
    let (a, b, c) = (PathBuf::from("/fc-a"), PathBuf::from("/fc-b"), PathBuf::from("/fc-c"));
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused-shortcuts.png")));
    app.st.favorites = vec![
        FavGroup::new("First", vec![a.clone(), b.clone(), c.clone()]),
        FavGroup::new("Second", vec![]),
    ];
    app.st.sidebar_order = vec![SidebarSection::Favorite(0), SidebarSection::Favorite(1),
        SidebarSection::Drives, SidebarSection::Cloud, SidebarSection::History];
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let group_rect = |app: &App, index| app.drop_zones.iter().find_map(|(r, t)| {
        matches!(t, DropTarget::Favorites(g) if *g == index).then_some(*r)
    }).unwrap();
    // Three rows are bottom-aligned inside the card's 6 px margin, 1 px apart.
    let row_h = app.row_h() + 2.0;
    let row = |card: Rect, k: f32| pos2(card.min.x + 40.0, card.max.y - 6.0 - (2.5 - k) * row_h);

    app.dialog = Some(Dialog::RenameFavorite { group: 0, path: a.clone(), name: "Projekty".into(), focus: true });
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    assert_eq!(app.st.favorites[0].label(&a), "Projekty");

    let first = group_rect(&app, 0);
    drag_mouse(&ctx, &mut app, row(first, 0.0), row(first, 2.0) + vec2(0.0, 7.0));
    assert_eq!(app.st.favorites[0].paths, [b.clone(), c.clone(), a.clone()], "mouse drag reorders shortcuts");

    let first = group_rect(&app, 0);
    let second = group_rect(&app, 1);
    drag_mouse(&ctx, &mut app, row(first, 2.0), second.center());
    assert_eq!(app.st.favorites[0].paths, [b.clone(), c.clone()]);
    assert_eq!(app.st.favorites[1].paths, [a.clone()], "shortcuts move between groups");
    assert_eq!(app.st.favorites[1].label(&a), "Projekty", "custom name moves too");
    assert!(app.st.favorites[0].labels.is_empty());

    // Files dropped from a panel on a group card must reach the group, not be
    // swallowed by the card's own drag-and-drop handling.
    let second = group_rect(&app, 1);
    let dropped = PathBuf::from(if cfg!(windows) { r"C:\fc-dropped" } else { "/fc-dropped" });
    frame(&ctx, &mut app, vec![Event::PointerMoved(second.center())], Modifiers::NONE);
    frame(&ctx, &mut app, vec![Event::PointerButton { pos: second.center(), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Modifiers::NONE);
    egui::DragAndDrop::set_payload(&ctx, DragPaths(vec![dropped.clone()]));
    frame(&ctx, &mut app, vec![Event::PointerMoved(second.center() + vec2(1.0, 1.0))], Modifiers::NONE);
    frame(&ctx, &mut app, vec![Event::PointerButton { pos: second.center(), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert!(app.st.favorites[1].paths.contains(&dropped), "file drop on a group adds the shortcut");
}

fn drag_tab(ctx: &egui::Context, app: &mut App, start: Pos2, end: Pos2) {
    frame(ctx, app, vec![Event::PointerMoved(start)], Modifiers::NONE);
    frame(ctx, app, vec![Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Modifiers::NONE);
    for _ in 0..2 {
        frame(ctx, app, vec![Event::PointerMoved(end)], Modifiers::NONE);
    }
    frame(ctx, app, vec![Event::PointerButton { pos: end, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    frame(ctx, app, vec![], Modifiers::NONE);
}

#[test]
fn tabs_split_merge_reorder_and_cancel() {
    use crate::app::Tab;
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused-tab-test.png")));
    let home = std::env::temp_dir();
    let mut first = Tab::new(home.clone());
    first.back.push(home.join("previous"));
    app.panes[0].tabs = vec![first, Tab::new(home.join("second"))];
    app.panes[0].active = 0;
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let rect = |app: &App, id| app.pane_rects.iter().find(|(k, _)| *k == id).unwrap().1;
    let a = rect(&app, 0);
    let b = rect(&app, 1);
    drag_tab(&ctx, &mut app, pos2(a.min.x + 50.0, a.min.y + 16.0), pos2(b.center().x, b.max.y - 20.0));
    assert_eq!(app.panes.len(), 3, "one tab splits into a new panel");
    assert_eq!(app.panes[0].tabs.len(), 1);
    assert_eq!(app.panes[2].tabs[0].back, vec![home.join("previous")]);
    assert!(rect(&app, 2).min.y > rect(&app, 1).min.y);
    let c = rect(&app, 2);
    let a = rect(&app, 0);
    drag_tab(&ctx, &mut app, pos2(c.min.x + 50.0, c.min.y + 16.0), a.center());
    assert_eq!(app.panes.len(), 2, "empty source panel closes on merge");
    assert_eq!(app.panes[0].tabs.len(), 2);
    assert_eq!(app.panes[0].tabs[1].back, vec![home.join("previous")]);
    let a = rect(&app, 0);
    drag_tab(&ctx, &mut app, pos2(a.min.x + 240.0, a.min.y + 16.0), pos2(a.min.x + 5.0, a.min.y + 16.0));
    assert_eq!(app.panes[0].tabs[0].back, vec![home.join("previous")], "tab strip reorders actual tabs");
    drag_tab(&ctx, &mut app, pos2(a.min.x + 50.0, a.min.y + 16.0), pos2(10.0, 10.0));
    assert_eq!(app.panes.len(), 2, "drop outside workspace cancels");
    assert_eq!(app.panes[0].tabs.len(), 2);
    // Remove the lower-index source when merging its final tab into another panel.
    app.dock_tab(0, 0, 1, 0, None);
    app.dock_tab(0, 0, 1, 0, None);
    assert_eq!(app.panes.len(), 1);
    assert_eq!(app.active, 0);
    let mut leaves = Vec::new();
    app.tiles.leaves(&mut leaves);
    assert_eq!(leaves, vec![0]);
}

#[test]
fn tab_shows_move_cursor_only_while_dragged() {
    use egui::CursorIcon::{Default as Arrow, Grabbing};
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused-tab-cursor.png")));
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let pane = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    let tab = pos2(pane.min.x + 50.0, pane.min.y + 16.0);
    let mut step = |events: Vec<Event>| {
        let mut out = ctx.run_ui(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))),
            events,
            ..Default::default()
        }, |ui| app.draw(ui));
        out.textures_delta.clear();
        out.platform_output.cursor_icon
    };
    let button = |pos, pressed| Event::PointerButton {
        pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE,
    };
    assert_eq!(step(vec![Event::PointerMoved(tab)]), Arrow, "hovering a tab is not a drag");
    assert_eq!(step(vec![Event::PointerMoved(tab)]), Arrow, "hover stays plain on later frames");
    assert_eq!(step(vec![button(tab, true)]), Arrow, "pressing without moving is not a drag");
    step(vec![Event::PointerMoved(tab + vec2(0.0, 40.0))]);
    assert_eq!(step(vec![Event::PointerMoved(tab + vec2(0.0, 40.0))]), Grabbing, "over a panel");
    let outside = pos2(10.0, 10.0);
    step(vec![Event::PointerMoved(outside)]);
    assert_eq!(step(vec![Event::PointerMoved(outside)]), Grabbing, "away from any drop target");
    step(vec![button(outside, false)]);
    step(vec![Event::PointerMoved(tab)]);
    assert_eq!(step(vec![Event::PointerMoved(tab)]), Arrow, "the move cursor ends with the drag");
}

#[test]
fn panel_handle_moves_a_panel_with_all_its_tabs() {
    use crate::app::Tab;
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused-panel-move.png")));
    let home = std::env::temp_dir().join(format!("fc_panel_move_{}", std::process::id()));
    for name in ["first", "second", "third"] {
        std::fs::create_dir_all(home.join(name)).unwrap();
    }
    let mut first = Tab::new(home.join("first"));
    first.back.push(home.join("previous"));
    app.panes[0].tabs = vec![first, Tab::new(home.join("second"))];
    app.panes[0].active = 1;
    app.panes[0].closed.push(home.join("closed"));
    app.panes[1].tabs = vec![Tab::new(home.join("third"))];
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let rect = |app: &App, id| app.pane_rects.iter().find(|(k, _)| *k == id).unwrap().1;
    let handle = |r: Rect| pos2(r.max.x - 16.0, r.min.y + 16.0);

    // Hovering is plain; only the drag shows the move cursor.
    let step = |app: &mut App, events: Vec<Event>| {
        let mut out = ctx.run_ui(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))),
            events,
            ..Default::default()
        }, |ui| app.draw(ui));
        out.textures_delta.clear();
        out.platform_output.cursor_icon
    };
    let (a, b) = (rect(&app, 0), rect(&app, 1));
    let h = handle(a);
    let button = |pos, pressed| Event::PointerButton {
        pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE,
    };
    step(&mut app, vec![Event::PointerMoved(h)]);
    assert_eq!(step(&mut app, vec![Event::PointerMoved(h)]), egui::CursorIcon::Default);
    step(&mut app, vec![button(h, true)]);
    let inside = pos2(a.center().x, a.max.y - 20.0);
    step(&mut app, vec![Event::PointerMoved(inside)]);
    assert_eq!(step(&mut app, vec![Event::PointerMoved(inside)]), egui::CursorIcon::Grabbing);
    step(&mut app, vec![button(inside, false)]);
    step(&mut app, vec![]);
    assert_eq!(app.panes.len(), 2, "dropping a panel on itself does nothing");
    assert_eq!(app.panes[0].tabs.len(), 2);

    // An edge moves the panel and keeps it whole.
    drag_tab(&ctx, &mut app, handle(a), pos2(b.max.x - 10.0, b.center().y));
    assert_eq!(app.panes.len(), 2);
    assert_eq!(app.panes[0].tabs.len(), 2);
    assert!(rect(&app, 0).min.x > rect(&app, 1).min.x, "the panel now sits right of the other");

    // The middle merges every tab, keeping order, history and the tab in use.
    let (source, target) = (handle(rect(&app, 0)), rect(&app, 1).center());
    drag_tab(&ctx, &mut app, source, target);
    assert_eq!(app.panes.len(), 1);
    let names: Vec<_> = app.panes[0].tabs.iter().map(|t| t.path.file_name().unwrap().to_owned()).collect();
    assert_eq!(names, ["third", "first", "second"]);
    assert_eq!(app.panes[0].active, 2, "the moved panel's active tab stays active");
    assert_eq!(app.panes[0].tabs[1].back, vec![home.join("previous")]);
    assert_eq!(app.panes[0].closed, vec![home.join("closed")]);
    assert_eq!(app.active, 0);

    // A lone panel has no handle: that corner holds the new-panel button.
    let corner = handle(rect(&app, 0));
    drag_tab(&ctx, &mut app, corner, pos2(10.0, 400.0));
    assert_eq!(app.panes.len(), 1);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn tab_close_buttons_remove_tabs_and_their_last_panel() {
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused_close_tabs.png")));
    let home = app.panes[0].path().to_path_buf();
    for pane in &mut app.panes {
        pane.tabs.truncate(1);
        pane.active = 0;
    }
    app.new_tab(0, Some(home.clone()), false);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let pane = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    // Close the inactive tab directly, without first selecting it.
    let w = ((pane.width() - 2.0 - 140.0) / 2.0).clamp(12.0, 190.0);
    let x = pos2(pane.min.x + 5.0 + 2.0 * w - 14.0, pane.min.y + 1.0 + (TAB_H + 4.0) / 2.0);
    click(&ctx, &mut app, x, Modifiers::NONE);
    assert_eq!(app.panes[0].tabs.len(), 1);
    assert_eq!(app.panes.len(), 2);
    let pane = app.pane_rects.iter().find(|(k, _)| *k == 0).unwrap().1;
    let w = (pane.width() - 2.0 - 140.0).clamp(12.0, 190.0);
    click(&ctx, &mut app, pos2(pane.min.x + 5.0 + w - 14.0, x.y), Modifiers::NONE);
    assert_eq!(app.panes.len(), 1, "closing the last tab removes its panel");
    assert_eq!(app.active, 0);
    assert_eq!(app.panes[0].tabs.len(), 1);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let pane = app.pane_rects[0].1;
    let pos = pos2(pane.min.x + 5.0 + 190.0 - 14.0, x.y);
    frame(&ctx, &mut app, vec![Event::PointerMoved(pos), Event::PointerButton {
        pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE,
    }], Modifiers::NONE);
    let output = ctx.run_ui(RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))),
        events: vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }],
        ..Default::default()
    }, |ui| app.draw(ui));
    assert!(output.viewport_output.values().any(|v| v.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Close))),
        "closing the final tab closes the application window");
}

#[cfg(not(windows))]
#[test]
fn linux_root_navigation_and_app_chooser_cancel() {
    let ctx = egui::Context::default();
    let mut app = App::new(Some(std::env::temp_dir().join("unused_linux.png")));
    app.panes[0].tab().path = "/".into();
    app.go_up(0);
    assert!(crate::app::is_computer(app.panes[0].path()));
    assert!(crate::app::is_computer(&app.column_chain(std::path::Path::new("/tmp"))[0]));
    app.acts.push(crate::app::Act::OpenWith(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"), None));
    app.end_frame(&ctx);
    assert!(app.app_chooser.is_some());
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    assert!(app.app_chooser.is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_properties_keyboard_and_cancel() {
    let ctx = egui::Context::default();
    let root = std::env::temp_dir().join(format!("fc_props_ui_{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("sample.txt"); std::fs::write(&path, "test").unwrap();
    let mut app = App::new(Some(root.join("unused.png")));
    app.acts.push(crate::app::Act::Properties(path));
    app.end_frame(&ctx);
    assert!(app.properties.is_some());
    for _ in 0..5 { frame(&ctx, &mut app, vec![], Modifiers::NONE); }
    key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    assert!(app.properties.is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn filtered_tree_folder_arrow_shows_nonmatching_contents() {
    let root = std::env::temp_dir().join(format!("fc_filtered_tree_{}", std::process::id()));
    let folder = root.join("Needle");
    let nested = folder.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(root.join("Needle-other")).unwrap();
    std::fs::create_dir_all(root.join("unrelated")).unwrap();
    std::fs::write(nested.join("sample.txt"), "hello").unwrap();
    std::fs::write(folder.join("contents.txt"), "hello").unwrap();
    // A shipped hidden-file rule applies on both Windows and Linux; a dot
    // prefix alone is intentionally visible on Windows.
    std::fs::write(folder.join("hidden.tmp"), "hidden").unwrap();
    let ctx = egui::Context::default();
    let mut app = App::new(Some(root.join("unused.png")));
    app.panes[0].tab().path = root.clone();
    app.st.tree_files = true;
    app.st.show_hidden = false;
    app.panes[0].filter = "NEEDLE".into();
    let wait = |app: &mut App, count: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            frame(&ctx, app, vec![], Modifiers::NONE);
            if app.panes[0].view.len() == count { break; }
            assert!(std::time::Instant::now() < deadline, "filtered tree load timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let arrow = |app: &App, path: &std::path::Path, indent: f32| {
        let row = app.panes[0].view.iter().position(|&k|
            app.panes[0].tree_entries[k].path == path).unwrap();
        let r = app.pane_rects.iter().find(|(i, _)| *i == 0).unwrap().1;
        pos2(r.min.x + 16.0 + indent,
            r.min.y + 1.0 + TAB_H + HEADER_H + 22.0 + app.row_h() * (row as f32 + 0.5))
    };
    wait(&mut app, 2);
    let pos = arrow(&app, &folder, 0.0);
    click(&ctx, &mut app, pos, Modifiers::NONE);
    wait(&mut app, 4);
    assert!(app.panes[0].expanded.contains(&folder));
    assert_eq!(app.panes[0].filter, "NEEDLE");
    let pos = arrow(&app, &nested, 18.0);
    click(&ctx, &mut app, pos, Modifiers::NONE);
    wait(&mut app, 5);
    assert!(app.panes[0].view.iter().any(|&k|
        app.panes[0].tree_entries[k].path == nested.join("sample.txt")));
    // Changing the query must remove contents inherited from the old match.
    app.panes[0].filter = "other".into();
    wait(&mut app, 1);
    assert_eq!(app.entry_at(0, 0).unwrap().path, root.join("Needle-other"));
    app.panes[0].filter = "needle".into();
    wait(&mut app, 5);
    let pos = arrow(&app, &folder, 0.0);
    click(&ctx, &mut app, pos, Modifiers::NONE);
    wait(&mut app, 2);
    assert!(!app.panes[0].expanded.contains(&folder));
    app.st.tree_files = false;
    app.panes[0].view_key = None;
    wait(&mut app, 2);
    app.panes[0].filter.clear();
    wait(&mut app, 3);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tree_folders_only_expand_by_arrow_and_double_click_open() {
    let root = std::env::temp_dir().join(format!("fc_tree_ui_{}", std::process::id()));
    let folder = root.join("branch");
    let nested = folder.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let file = nested.join("sample.txt");
    std::fs::write(&file, "hello").unwrap();
    std::fs::write(folder.join("hidden.tmp"), "hidden").unwrap();
    let ctx = egui::Context::default();
    let mut app = App::new(Some(root.join("unused.png")));
    app.panes[0].tab().path = root.clone();
    app.st.tree_files = true;
    app.st.show_hidden = false;
    let wait = |app: &mut App, count: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            frame(&ctx, app, vec![], Modifiers::NONE);
            if app.panes[0].view.len() == count { break; }
            assert!(std::time::Instant::now() < deadline, "tree folder load timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    wait(&mut app, 1);
    let r = app.pane_rects.iter().find(|(i, _)| *i == 0).unwrap().1;
    let top = r.min.y + 1.0 + TAB_H + HEADER_H + 22.0;
    let row = |k: usize| pos2(r.min.x + 100.0, top + app.row_h() * (k as f32 + 0.5));
    let first = row(0);
    let second = row(1);
    let first_arrow = pos2(r.min.x + 16.0, first.y);
    let second_arrow = pos2(r.min.x + 34.0, second.y);
    click(&ctx, &mut app, first, Modifiers::NONE);
    assert_eq!(app.panes[0].path(), root);
    assert!(app.panes[0].expanded.is_empty());
    assert_eq!(app.selection(0), vec![folder.clone()]);
    click(&ctx, &mut app, first_arrow, Modifiers::NONE);
    wait(&mut app, 2);
    assert_eq!(app.panes[0].path(), root);
    assert_eq!(app.entry_at(0, 1).unwrap().path, nested);
    click(&ctx, &mut app, second, Modifiers::NONE);
    assert!(!app.panes[0].expanded.contains(&nested));
    assert_eq!(app.selection(0), vec![nested.clone()]);
    click(&ctx, &mut app, second_arrow, Modifiers::NONE);
    wait(&mut app, 3);
    assert_eq!(app.entry_at(0, 2).unwrap().path, file);
    app.set_cursor(0, 2, Modifiers::NONE);
    assert_eq!(app.selection(0), vec![file.clone()]);
    assert_eq!(app.counts(0), (3, 1, 5));
    app.acts.push(crate::app::Act::SelectAll(0));
    app.end_frame(&ctx);
    assert_eq!(app.selection(0).len(), 3);
    app.set_cursor(0, 1, Modifiers::NONE);
    key(&ctx, &mut app, Key::ArrowLeft, Modifiers::NONE);
    wait(&mut app, 2);
    assert!(!app.panes[0].expanded.contains(&nested));
    key(&ctx, &mut app, Key::ArrowRight, Modifiers::NONE);
    wait(&mut app, 3);
    // Let the earlier click expire before a deliberate double click.
    for _ in 0..45 { frame(&ctx, &mut app, vec![], Modifiers::NONE); }
    click(&ctx, &mut app, first_arrow, Modifiers::NONE);
    wait(&mut app, 1);
    assert!(!app.selection(0).contains(&file));
    assert_eq!(app.panes[0].path(), root);
    for _ in 0..45 { frame(&ctx, &mut app, vec![], Modifiers::NONE); }
    click(&ctx, &mut app, first, Modifiers::NONE);
    assert_eq!(app.panes[0].path(), root);
    assert!(!app.panes[0].expanded.contains(&folder));
    click(&ctx, &mut app, first, Modifiers::NONE);
    assert_eq!(app.panes[0].path(), folder);
    assert!(app.panes[0].expanded.is_empty());
    // The actual toolbar toggle must safely change a populated view both ways.
    let toggle = pos2(103.0, 732.0);
    click(&ctx, &mut app, toggle, Modifiers::NONE);
    assert!(!app.st.tree_files);
    click(&ctx, &mut app, toggle, Modifiers::NONE);
    assert!(app.st.tree_files);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_colors_sync_between_file_menu_and_sidebar() {
    use crate::settings::FavGroup;
    use crate::theme::{TAGS, hex};
    let root = std::env::temp_dir().join(format!("fc_colors_{}", std::process::id()));
    let folder = root.join("Colored folder");
    std::fs::create_dir_all(&folder).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::new(Some(root.join("unused.png")));
    app.panes[0].tab().path = root.clone();
    app.st.favorites = vec![FavGroup::new("Colors", vec![folder.clone()])];
    app.st.sidebar_order.clear();
    app.st.show_history = false;
    app.st.show_cloud = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.panes[0].view.is_empty() {
        assert!(std::time::Instant::now() < deadline);
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    fn snapshot(ctx: &egui::Context, app: &mut App) -> egui::FullOutput {
        frame(ctx, app, vec![], Modifiers::NONE);
        let mut out = ctx.run_ui(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))),
            ..Default::default()
        }, |ui| app.draw(ui));
        out.textures_delta.clear();
        out
    }
    fn label(out: &egui::FullOutput, name: &str, left: bool, split: f32) -> Pos2 {
        out.shapes.iter().find_map(|s| {
            if let egui::epaint::Shape::Text(t) = &s.shape {
                let r = Rect::from_min_size(t.pos, t.galley.size());
                if t.galley.job.text == name && (r.center().x < split) == left {
                    return Some(r.center());
                }
            }
            None
        }).unwrap_or_else(|| panic!("missing label {name}"))
    }
    for (sidebar, choice, expected) in [(false, "Red", Some(0)), (true, "Blue", Some(4)), (true, "Clear color", None)] {
        let out = snapshot(&ctx, &mut app);
        let split = app.pane_rects.iter().find(|(i, _)| *i == 0).unwrap().1.left();
        let pos = label(&out, "Colored folder", sidebar, split);
        frame(&ctx, &mut app, vec![Event::PointerMoved(pos)], Modifiers::NONE);
        for pressed in [true, false] {
            frame(&ctx, &mut app, vec![Event::PointerButton {
                pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE,
            }], Modifiers::NONE);
        }
        let out = snapshot(&ctx, &mut app);
        let pos = label(&out, "Folder color", false, 0.0);
        click(&ctx, &mut app, pos, Modifiers::NONE);
        let out = snapshot(&ctx, &mut app);
        let pos = label(&out, choice, false, 0.0);
        click(&ctx, &mut app, pos, Modifiers::NONE);
        let out = snapshot(&ctx, &mut app);
        assert_eq!(app.tag_color(&folder), expected.map(|n| hex(TAGS[n].1)), "sidebar={sidebar}, selected={:?}, tags={:?}", app.selection(0), app.st.tags);
        // Check actual folder fronts painted on both sides of the divider.
        let color = app.folder_color(&folder);
        for left in [true, false] {
            assert!(out.shapes.iter().any(|s| matches!(&s.shape,
                egui::epaint::Shape::Rect(r) if r.fill == color && (r.rect.center().x < split) == left)),
                "folder icon color must agree in sidebar and files");
        }
        let saved = crate::lua_config::encode(&serde_json::to_value(&app.st).unwrap());
        let restored: crate::settings::Settings = serde_json::from_value(crate::lua_config::decode(&saved).unwrap()).unwrap();
        assert_eq!(restored.tags, app.st.tags);
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn key_event(key: Key, repeat: bool, modifiers: Modifiers) -> Event {
    Event::Key { key, physical_key: None, pressed: true, repeat, modifiers }
}

/// A pane on a private folder holding `keep.txt`, with the cursor on it.
fn safety_app(name: &str) -> (std::path::PathBuf, std::path::PathBuf, egui::Context, App) {
    let root = std::env::temp_dir().join(format!("fc_safety_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("keep.txt");
    std::fs::write(&file, b"keep").unwrap();
    let ctx = egui::Context::default();
    let mut app = App::new(Some(root.join("unused.png")));
    app.panes[0].tab().path = root.clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.panes[0].view.len() != 1 {
        assert!(std::time::Instant::now() < deadline, "folder load timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.active = 0;
    app.panes[0].cursor = Some(0);
    (root, file, ctx, app)
}

#[test]
fn held_keys_do_not_repeat_file_operations() {
    use crate::app::Notice;
    let (root, file, ctx, mut app) = safety_app("repeat");
    assert_eq!(app.selection(0), [file.clone()]);
    let release = |k, m| Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: m };
    // egui derives `repeat` itself, from a second press with no release in between. An
    // open notice swallows the first press, which would really delete the file.
    app.notices.push(Notice::Problems { title: "Busy".into(), lines: Vec::new() });
    frame(&ctx, &mut app, vec![key_event(Key::Delete, false, Modifiers::NONE)], Modifiers::NONE);
    frame(&ctx, &mut app, vec![key_event(Key::D, false, Modifiers::COMMAND)], Modifiers::COMMAND);
    app.notices.clear();
    frame(&ctx, &mut app, vec![key_event(Key::Delete, false, Modifiers::NONE)], Modifiers::NONE);
    frame(&ctx, &mut app, vec![key_event(Key::D, false, Modifiers::COMMAND)], Modifiers::COMMAND);
    assert!(app.jobs.is_empty(), "autorepeat started an operation");
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    // Control: a real press of the same shortcut does reach the operation.
    frame(&ctx, &mut app, vec![release(Key::D, Modifiers::COMMAND)], Modifiers::COMMAND);
    frame(&ctx, &mut app, vec![key_event(Key::D, false, Modifiers::COMMAND)], Modifiers::COMMAND);
    assert_eq!(app.jobs.len(), 1, "keyboard must be live in this test");
    // On Linux the job ends once the whole filesystem is synced, which takes a while when
    // other tests write to the same slow disk.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !app.jobs.is_empty() {
        let step = app.jobs[0].progress.lock().unwrap().current.clone();
        assert!(std::time::Instant::now() < deadline, "duplicate timed out at: {step}");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shift_delete_asks_first_and_then_deletes_for_good() {
    use crate::app::Notice;
    let (root, file, ctx, mut app) = safety_app("forever");
    let press = |ctx: &egui::Context, app: &mut App| {
        frame(ctx, app, vec![key_event(Key::Delete, false, Modifiers::SHIFT)], Modifiers::SHIFT);
        frame(ctx, app, vec![Event::Key { key: Key::Delete, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::SHIFT }], Modifiers::SHIFT);
    };
    press(&ctx, &mut app);
    assert!(matches!(&app.notices[0], Notice::ConfirmDestroy { items } if items.len() == 1 && items[0].0 == file && items[0].1.is_empty()));
    assert!(app.jobs.is_empty());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep", "nothing happens before the answer");
    click_text(&ctx, &mut app, "Keep the files");
    assert!(app.notices.is_empty() && app.jobs.is_empty());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");

    press(&ctx, &mut app);
    click_text(&ctx, &mut app, "Delete permanently");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !app.jobs.is_empty() {
        assert!(std::time::Instant::now() < deadline, "delete timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.notices.is_empty(), "deleted without problems");
    assert!(std::fs::symlink_metadata(&file).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn plain_delete_goes_to_the_trash_without_asking() {
    use crate::app::Notice;
    use crate::ops::OpKind;
    let (root, file, ctx, mut app) = safety_app("plain");
    // Gone from disk but still listed: the real job runs and nothing lands in the real trash.
    std::fs::remove_file(&file).unwrap();
    frame(&ctx, &mut app, vec![key_event(Key::Delete, false, Modifiers::NONE)], Modifiers::NONE);
    assert!(app.jobs.first().is_none_or(|job| job.kind == OpKind::Trash));
    assert!(!app.notices.iter().any(|n| matches!(n, Notice::ConfirmDestroy { .. })));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !app.jobs.is_empty() {
        assert!(std::time::Instant::now() < deadline, "delete timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn compressing_asks_for_a_name_and_selects_the_new_archive() {
    use crate::app::{Act, Dialog};
    let (root, file, ctx, mut app) = safety_app("compress");
    app.acts.push(Act::Compress(0, vec![file.clone()]));
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert!(matches!(app.dialog, Some(Dialog::Compress { .. })));
    assert!(app.jobs.is_empty(), "nothing starts before the answer");
    click_text(&ctx, &mut app, "Compress");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !app.jobs.is_empty() {
        assert!(std::time::Instant::now() < deadline, "compress timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.notices.is_empty(), "packed without problems");
    let archive = file.with_extension("zip");
    assert!(archive.is_file());
    assert!(app.panes[0].selected.contains(&archive));
    assert_eq!(std::fs::read(&file).unwrap(), b"keep", "the original stays");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn keys_cannot_confirm_a_permanent_deletion() {
    use crate::app::Notice;
    let (root, file, ctx, mut app) = safety_app("confirm");
    app.notices.push(Notice::ConfirmDestroy { items: vec![(file.clone(), "network locations have no Recycle Bin".into())] });
    for k in [Key::Enter, Key::Space, Key::Delete, Key::D] {
        frame(&ctx, &mut app, vec![key_event(k, false, Modifiers::NONE)], Modifiers::NONE);
        assert_eq!(app.notices.len(), 1, "{k:?} answered the question");
        assert!(app.jobs.is_empty(), "{k:?} started an operation");
    }
    frame(&ctx, &mut app, vec![key_event(Key::Escape, false, Modifiers::NONE)], Modifiers::NONE);
    assert!(app.notices.is_empty() && app.jobs.is_empty());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn window_stays_open_until_operations_stop() {
    use crate::app::Notice;
    use crate::ops::{Job, OpKind, Progress};
    use egui::{ViewportCommand, ViewportEvent, ViewportId};
    use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
    let (root, _, ctx, mut app) = safety_app("close");
    let commands = |ctx: &egui::Context, app: &mut App, close: bool| {
        let mut input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))), ..Default::default() };
        if close { input.viewports.get_mut(&ViewportId::ROOT).unwrap().events.push(ViewportEvent::Close); }
        let mut out = ctx.run_ui(input, |ui| app.draw(ui));
        out.textures_delta.clear();
        out.viewport_output[&ViewportId::ROOT].commands.clone()
    };
    assert!(!commands(&ctx, &mut app, true).contains(&ViewportCommand::CancelClose), "nothing running, close freely");
    let progress = Arc::new(Mutex::new(Progress { total_items: 1, ..Default::default() }));
    let cancel = Arc::new(AtomicBool::new(false));
    app.jobs.push(Job { kind: OpKind::Move, dest: None, progress: progress.clone(), cancel: cancel.clone() });
    assert!(commands(&ctx, &mut app, true).contains(&ViewportCommand::CancelClose));
    assert!(matches!(app.notices[0], Notice::ConfirmClose));
    assert!(commands(&ctx, &mut app, true).contains(&ViewportCommand::CancelClose));
    assert_eq!(app.notices.len(), 1, "one question, however often the user clicks X");
    // What "Cancel operations and close" does, up to the worker reporting back.
    app.notices.clear();
    app.closing = true;
    cancel.store(true, Ordering::Relaxed);
    assert!(!commands(&ctx, &mut app, false).contains(&ViewportCommand::Close), "worker is still running");
    progress.lock().unwrap().finished = true;
    progress.lock().unwrap().errors.push("Cancelled".into());
    assert!(commands(&ctx, &mut app, false).contains(&ViewportCommand::Close), "closes once the worker has stopped");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn failures_stay_on_screen_until_acknowledged() {
    use crate::app::Notice;
    use crate::ops::{Job, OpKind, Progress};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    let (root, _, ctx, mut app) = safety_app("problems");
    let progress = Progress { finished: true, total_items: 1, errors: vec!["data.bin: Disk full".into()], ..Default::default() };
    app.jobs.push(Job { kind: OpKind::Copy, dest: None, progress: Arc::new(Mutex::new(progress)), cancel: Arc::new(AtomicBool::new(false)) });
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert!(matches!(&app.notices[0], Notice::Problems { lines, .. } if lines[0].contains("Disk full")));
    std::fs::remove_dir_all(root).unwrap();
}

/// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_drive_asks -- --ignored
#[test]
#[ignore = "Writes to the mounted filesystems named in FERROUS_FS_TARGETS"]
#[cfg(target_os = "linux")]
fn real_drive_asks_before_running_and_starts_nothing_when_refused() {
    use crate::app::Notice;
    let targets = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
    let (root, _, ctx, mut app) = safety_app("usb_run");
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap();
    for local in [root.join("start.sh"), home.join("start.sh"), "/usr/bin/sh".into()] {
        assert_eq!(crate::drives::foreign_place(&local), None, "{local:?}: local programs stay one key press away");
    }
    for target in targets.split(':') {
        let dir = std::path::Path::new(target).join(format!("fc_run_test_{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let marker = root.join("it ran");
        for name in ["start.sh", "START.BAT", "Install.desktop"] {
            let file = dir.join(name);
            std::fs::write(&file, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
            app.open(0, file.clone());
            app.open(0, file.clone());
            assert_eq!(app.notices.len(), 1, "one question, however often it is opened");
            assert!(matches!(&app.notices[0], Notice::ConfirmRun { path, place, .. } if *path == file && place.contains("removable")));
            click_text(&ctx, &mut app, "Keep closed");
            assert!(app.notices.is_empty());
        }
        // A harmless name proves nothing: the system follows links and reads contents.
        let program = dir.join("photo.jpg");
        std::fs::copy("/usr/bin/true", &program).unwrap();
        app.open(0, program);
        assert!(matches!(&app.notices[0], Notice::ConfirmRun { link: None, .. }));
        click_text(&ctx, &mut app, "Keep closed");
        std::fs::write(dir.join("payload.jar"), "stub").unwrap();
        let link = dir.join("notes.txt");
        // FAT and exFAT have no links.
        if std::os::unix::fs::symlink("payload.jar", &link).is_ok() {
            app.open(0, link);
            assert!(matches!(&app.notices[0], Notice::ConfirmRun { link: Some(target), .. } if target == "payload.jar"));
            click_text(&ctx, &mut app, "Keep closed");
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!marker.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn eject_waits_for_running_operations_and_tabs_leave_a_drive_that_went() {
    use crate::app::{DriveInfo, Notice};
    use crate::ops::{Job, OpKind, Progress};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    let (root, _, ctx, mut app) = safety_app("eject");
    let drive = DriveInfo { path: root.clone(), name: "Stick".into(), label: String::new(), total: 0, free: 0,
        network: false, remote: String::new(), eject: Some("/dev/ferrous-test-no-such-device".into()) };

    app.jobs.push(Job { kind: OpKind::Copy, dest: None, progress: Arc::new(Mutex::new(Progress::default())), cancel: Arc::new(AtomicBool::new(false)) });
    app.eject(&drive);
    assert!(app.ejecting.is_empty(), "nothing starts while files are moving");
    assert!(matches!(&app.notices[0], Notice::Problems { lines, .. } if lines[0].contains("still running")));
    app.notices.clear();
    app.jobs.clear();

    // A failure keeps the tab and says why. udisks refuses a device that does not exist.
    app.eject(&drive);
    assert_eq!(app.ejecting.len(), 1);
    app.eject(&drive);
    assert_eq!(app.ejecting.len(), 1, "a second click does not start a second eject");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !app.ejecting.is_empty() {
        assert!(std::time::Instant::now() < deadline, "eject timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(&app.notices[0], Notice::Problems { title, .. } if title.contains("not ejected")));
    assert_eq!(app.panes[0].path(), root.as_path());
    app.notices.clear();

    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Ok("It is safe to remove the drive".to_string())).unwrap();
    app.ejecting.push((root.clone(), "Stick".into(), rx));
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert!(crate::app::is_computer(app.panes[0].path()), "the tab left the drive");
    assert!(app.notices.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

/// Ejects the drive for real, so it has to be plugged in again afterwards.
/// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_drive_ejects -- --ignored --nocapture
#[test]
#[ignore = "Unmounts and powers off the drive named in FERROUS_FS_TARGETS"]
#[cfg(target_os = "linux")]
fn real_drive_ejects_only_when_nothing_uses_it() {
    let target = std::path::PathBuf::from(std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS"));
    let drive = crate::drives::list().into_iter().find(|d| d.path == target).expect("drive is listed");
    let id = drive.eject.clone().expect("a USB drive offers Eject");
    // Unwritten data has to survive too, so this leaves some behind.
    let file = target.join("fc-eject-test.txt");
    std::fs::write(&file, "still here after replugging").unwrap();
    let held = std::fs::File::open(&file).unwrap();
    let busy = crate::drives::eject(&target, &id);
    eprintln!("while in use: {busy:?}");
    assert!(busy.unwrap_err().contains("in use"));
    assert!(file.exists(), "a refused eject leaves the drive mounted");
    drop(held);
    let done = crate::drives::eject(&target, &id);
    eprintln!("when free: {done:?}");
    assert!(done.unwrap().contains("safe to remove"));
    assert!(!file.exists() && !crate::drives::list().iter().any(|d| d.path == target));
}

/// Click the painted text `label`, wherever the layout put it.
fn click_text(ctx: &egui::Context, app: &mut App, label: &str) {
    let mut found = None;
    for _ in 0..3 { // Windows first perform an invisible sizing pass.
        let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| app.draw(ui));
        out.textures_delta.clear();
        found = out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == label => Some(Rect::from_min_size(t.pos, t.galley.size()).center()),
            _ => None,
        });
    }
    click(ctx, app, found.unwrap_or_else(|| panic!("{label:?} is not on screen")), Modifiers::NONE);
}

#[test]
fn notice_buttons_do_what_they_say() {
    use crate::app::Notice;
    use crate::ops::{Job, OpKind, Progress};
    use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
    let (root, file, ctx, mut app) = safety_app("buttons");
    let confirm = |path: &std::path::Path| Notice::ConfirmDestroy { items: vec![(path.to_path_buf(), "network locations have no Recycle Bin".into())] };

    app.notices.push(confirm(&file));
    click_text(&ctx, &mut app, "Keep the files");
    assert!(app.notices.is_empty() && app.jobs.is_empty());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");

    // A name that does not exist: the whole path runs, and nothing can really be lost.
    app.notices.push(confirm(&root.join("already-gone.txt")));
    click_text(&ctx, &mut app, "Delete permanently");
    // Failing on a missing name can take less than the click's own frame. The error below
    // proves the job ran either way.
    assert!(app.jobs.first().is_none_or(|job| job.kind == OpKind::Destroy));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !app.jobs.is_empty() {
        assert!(std::time::Instant::now() < deadline, "delete timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(&app.notices[0], Notice::Problems { lines, .. } if lines[0].contains("already-gone.txt")));
    click_text(&ctx, &mut app, "Close");
    assert!(app.notices.is_empty());

    // Running a name that does not exist starts nothing, whatever the launcher does.
    let run = || Notice::ConfirmRun { path: root.join("missing-setup.exe"), place: "a removable drive", link: None };
    app.notices.push(run());
    click_text(&ctx, &mut app, "Keep closed");
    assert!(app.notices.is_empty());
    app.notices.push(run());
    frame(&ctx, &mut app, vec![Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    assert_eq!(app.notices.len(), 1, "Enter never confirms");
    click_text(&ctx, &mut app, "Run");
    assert!(app.notices.is_empty());

    let job = Job { kind: OpKind::Move, dest: None, progress: Arc::new(Mutex::new(Progress::default())), cancel: Arc::new(AtomicBool::new(false)) };
    let cancel = job.cancel.clone();
    app.jobs.push(job);
    app.notices.push(Notice::ConfirmClose);
    click_text(&ctx, &mut app, "Keep working");
    assert!(!app.closing && !cancel.load(Ordering::Relaxed));
    app.notices.push(Notice::ConfirmClose);
    click_text(&ctx, &mut app, "Cancel operations and close");
    assert!(app.closing && cancel.load(Ordering::Relaxed));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn archives_unpack_by_double_click_and_from_the_menu() {
    use crate::ops::OpKind;
    let (root, _, ctx, mut app) = safety_app("archives");
    let archive = root.join("pack.zip");
    std::fs::write(&archive, include_bytes!("testdata/sample.zip")).unwrap();
    app.reload_all();
    let wait_for = |ctx: &egui::Context, app: &mut App, what: &str, done: &dyn Fn(&App) -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !done(app) {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            frame(ctx, app, vec![], Modifiers::NONE);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    wait_for(&ctx, &mut app, "the archive to be listed", &|app| app.panes[0].view.len() == 2);
    let row = |ctx: &egui::Context, app: &mut App| {
        let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| app.draw(ui));
        out.textures_delta.clear();
        out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == "pack.zip" => Some(Rect::from_min_size(t.pos, t.galley.size()).center()),
            _ => None,
        }).expect("the archive's row is painted")
    };

    let pos = row(&ctx, &mut app);
    click(&ctx, &mut app, pos, Modifiers::NONE);
    click(&ctx, &mut app, pos, Modifiers::NONE);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert!(app.jobs.first().is_none_or(|job| job.kind == OpKind::Extract));
    wait_for(&ctx, &mut app, "the double click", &|app| app.jobs.is_empty());
    let first = root.join("pack");
    assert_eq!(std::fs::read(first.join("top.txt")).unwrap(), b"top\n");
    assert!(app.notices.is_empty());
    assert_eq!(app.panes[0].selected.iter().collect::<Vec<_>>(), [&first], "what arrived is selected");

    // Let the earlier click expire, so the right click stands alone.
    std::thread::sleep(std::time::Duration::from_millis(600));
    wait_for(&ctx, &mut app, "the new folder to be listed", &|app| app.panes[0].view.len() == 3);
    let pos = row(&ctx, &mut app);
    frame(&ctx, &mut app, vec![Event::PointerMoved(pos)], Modifiers::NONE);
    for pressed in [true, false] {
        frame(&ctx, &mut app, vec![Event::PointerButton {
            pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE,
        }], Modifiers::NONE);
    }
    click_text(&ctx, &mut app, "Extract here");
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    wait_for(&ctx, &mut app, "the menu's extraction", &|app| app.jobs.is_empty());
    assert_eq!(std::fs::read(root.join("pack - Copy").join("top.txt")).unwrap(), b"top\n");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn locked_archives_ask_again_until_the_password_fits() {
    use crate::app::Dialog;
    let (root, _, ctx, mut app) = safety_app("password");
    let archive = root.join("locked.7z");
    std::fs::write(&archive, include_bytes!("testdata/locked-p.7z")).unwrap();
    let settle = |ctx: &egui::Context, app: &mut App, what: &str, done: &dyn Fn(&App) -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !done(app) {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            frame(ctx, app, vec![], Modifiers::NONE);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let asked = |app: &App, expected: bool| matches!(&app.dialog, Some(Dialog::Password { wrong, .. }) if *wrong == expected);
    app.open(0, archive);
    settle(&ctx, &mut app, "the first question", &|app| asked(app, false));
    assert!(app.notices.is_empty(), "a locked archive is not a failure");

    let answer = |ctx: &egui::Context, app: &mut App, password: &str| {
        if let Some(Dialog::Password { name, .. }) = &mut app.dialog { *name = password.into(); }
        click_text(ctx, app, "Extract");
    };
    answer(&ctx, &mut app, "not it");
    settle(&ctx, &mut app, "the second question", &|app| asked(app, true));
    assert!(!root.join("in").exists());

    answer(&ctx, &mut app, "secret");
    settle(&ctx, &mut app, "the extraction", &|app| app.dialog.is_none() && app.jobs.is_empty());
    assert_eq!(std::fs::read(root.join("in/d/a.txt")).unwrap(), b"hello\n");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn space_opens_the_preview_arrows_change_its_file_and_escape_closes_it() {
    let (root, file, ctx, mut app) = safety_app("preview_keys");
    std::fs::write(root.join("second.txt"), b"two").unwrap();
    app.request_folder(&root, true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.panes[0].view.len() != 2 {
        assert!(std::time::Instant::now() < deadline, "folder load timed out");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.panes[0].cursor = Some(0);
    frame(&ctx, &mut app, vec![Event::Text(" ".into())], Modifiers::NONE);
    assert!(app.preview_window);
    assert_eq!(app.preview.path.as_ref(), Some(&file));
    key(&ctx, &mut app, Key::ArrowDown, Modifiers::NONE);
    assert_eq!(app.preview.path.as_ref(), Some(&root.join("second.txt")));
    key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    assert!(!app.preview_window);
    assert!(app.quick.is_none());
    frame(&ctx, &mut app, vec![Event::Text(" ".into())], Modifiers::NONE);
    assert!(app.preview_window, "a preview closed from the list opens again at once");
    std::fs::remove_dir_all(root).unwrap();
}
