use super::*;
use egui::{Event, Pos2, RawInput};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};

fn tab() -> SearchTab {
    let root = std::env::temp_dir();
    let mut tab = SearchTab::new(0, root.clone(), false);
    let mut entries = Vec::new();
    for (name, size) in [
        ("result-c.txt", 30),
        ("result-a.txt", 10),
        ("result-b.txt", 20),
    ] {
        let mut e = Entry::named(root.join(name), name.into(), false, fsx::Kind::Text);
        e.size = size;
        entries.push(e);
    }
    tab.run = Some(Search {
        results: Arc::new(Mutex::new(entries)),
        done: Arc::new(AtomicBool::new(true)),
        cancel: Arc::new(AtomicBool::new(false)),
        scanned: Arc::new(AtomicUsize::new(3)),
        progress: Arc::new(Mutex::new(search::Progress {
            outcome: Outcome::Complete,
            ..Default::default()
        })),
    });
    tab.summary = "Result files".into();
    tab.focus = false;
    tab.sort_dirty = true;
    tab.collect();
    tab
}

#[test]
fn selections_survive_sorting_and_tabs_keep_independent_results() {
    let mut window = SearchUi {
        tabs: vec![tab()],
        active: 0,
        next_id: 1,
        layer: None,
        keyboard_active: false,
    };
    let tab = &mut window.tabs[0];
    tab.select(0, Modifiers::NONE);
    tab.select(2, Modifiers::SHIFT);
    assert_eq!(tab.paths().len(), 3);
    tab.select(1, Modifiers::COMMAND);
    let paths = tab.paths();
    assert_eq!(paths.len(), 2);
    tab.sort = Column::Size;
    tab.ascending = false;
    tab.sort_dirty = true;
    tab.collect();
    let after = tab.paths();
    assert_eq!(after, paths.into_iter().rev().collect::<Vec<_>>());
    let cancel = tab.run.as_ref().unwrap().cancel.clone();
    window.add_tab(std::env::temp_dir().join("another"), true);
    assert!(window.tabs[1].paths().is_empty());
    assert_eq!(window.tabs[0].paths(), after);
    assert!(!cancel.load(Ordering::Relaxed));
    window.tabs.remove(0);
    assert!(
        cancel.load(Ordering::Relaxed),
        "closing a result tab cancels its worker"
    );
}

#[test]
fn nested_selections_are_operated_on_once_without_quadratic_deduplication() {
    let root = std::env::temp_dir().join("search-selection");
    let mut paths: Vec<PathBuf> = (0..20_000)
        .map(|i| root.join("folder").join(format!("file{i}")))
        .collect();
    paths.push(root.join("folder"));
    paths.push(root.join("folder"));
    paths.push(root.join("folder-with-similar-name").join("keep.txt"));
    assert_eq!(
        fsx::outermost_paths(paths),
        [
            root.join("folder"),
            root.join("folder-with-similar-name").join("keep.txt")
        ]
    );
}

#[test]
fn invalid_inputs_preserve_previous_results_and_dates_include_the_last_day() {
    let ctx = egui::Context::default();
    let mut tab = tab();
    tab.inputs.min_size = "nonsense".into();
    tab.start(&ctx);
    assert!(tab.error.is_some());
    assert_eq!(tab.view.len(), 3);
    tab.inputs.min_size.clear();
    tab.inputs.from = "2026-11-11".into();
    tab.inputs.through = "2026-11-11".into();
    let (_, criteria) = tab.inputs.criteria().unwrap();
    let (start, end) = (
        criteria.modified_from.unwrap(),
        criteria.modified_before.unwrap(),
    );
    assert!(end > start);
    assert_eq!(
        chrono::DateTime::<Local>::from(start)
            .date_naive()
            .to_string(),
        "2026-11-11"
    );
    assert_eq!(
        chrono::DateTime::<Local>::from(end)
            .date_naive()
            .to_string(),
        "2026-11-12"
    );
    tab.inputs.root = "relative/path".into();
    assert!(tab.inputs.criteria().is_err());
}

fn frame(
    ctx: &egui::Context,
    app: &mut App,
    mut events: Vec<Event>,
    modifiers: Modifiers,
) -> egui::FullOutput {
    events.insert(0, Event::ModifiersChanged(modifiers));
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| app.draw(ui));
    out.textures_delta.clear();
    out
}
fn painted(ctx: &egui::Context, app: &mut App, text: &str) -> Rect {
    for _ in 0..5 {
        let out = frame(ctx, app, vec![], Modifiers::NONE);
        if let Some(r) = out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(t) if t.galley.job.text == text => {
                Some(t.galley.rect.translate(t.pos.to_vec2()))
            }
            _ => None,
        }) {
            return r;
        }
    }
    panic!("{text} was not drawn");
}
fn click(ctx: &egui::Context, app: &mut App, at: Pos2) {
    frame(ctx, app, vec![Event::PointerMoved(at)], Modifiers::NONE);
    for pressed in [true, false] {
        frame(
            ctx,
            app,
            vec![Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }],
            Modifiers::NONE,
        );
    }
    frame(ctx, app, vec![], Modifiers::NONE);
}
fn app() -> (App, egui::Context) {
    let mut app = App::new(Some(PathBuf::from("unused.png")));
    // Other GUI tests can set START_DIR to a temporary folder and remove it.
    // Use a stable scope so their cleanup cannot make this pane navigate upward.
    app.navigate(0, std::env::temp_dir(), None);
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(true);
    app.search = Some(SearchUi {
        tabs: vec![tab()],
        active: 0,
        next_id: 1,
        layer: None,
        keyboard_active: false,
    });
    (app, ctx)
}

#[cfg(not(windows))]
#[test]
fn result_copy_and_trash_never_act_on_the_background_panes_selection() {
    let (mut app, ctx) = app();
    let background = std::env::temp_dir().join("background-selected.txt");
    app.panes[0].selected.insert(background.clone());
    let root = app.panes[0].path().to_path_buf();
    let name = painted(&ctx, &mut app, "result-a.txt");
    click(&ctx, &mut app, name.center());
    assert_eq!(app.search.as_ref().unwrap().tabs[0].selected.len(), 1);
    frame(
        &ctx,
        &mut app,
        vec![Event::Key {
            key: Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        }],
        Modifiers::COMMAND,
    );
    let copied = app.clip.as_ref().expect("search selection was copied");
    assert_eq!(&*copied.0, [std::env::temp_dir().join("result-a.txt")]);
    assert!(!copied.1);
    assert_eq!(app.panes[0].path(), root);
    frame(&ctx, &mut app, vec![Event::Cut], Modifiers::NONE);
    assert!(
        app.clip.as_ref().unwrap().1,
        "native clipboard events reach the result list"
    );
    frame(&ctx, &mut app, vec![Event::Copy], Modifiers::NONE);
    assert!(!app.clip.as_ref().unwrap().1);
    let copy = painted(&ctx, &mut app, "Copy");
    click(&ctx, &mut app, copy.center());
    assert!(
        app.search.as_ref().unwrap().owns_keyboard(&ctx),
        "toolbar focus must not enable background file shortcuts"
    );
    let trash = painted(&ctx, &mut app, "Trash…");
    click(&ctx, &mut app, trash.center());
    let Some(Notice::ConfirmTrash { items }) = app.notices.last() else {
        panic!("search deletion must ask first")
    };
    assert_eq!(items, &[std::env::temp_dir().join("result-a.txt")]);
}

#[test]
fn keyboard_navigation_scrolls_to_virtualized_results() {
    let (mut app, ctx) = app();
    let tab = &mut app.search.as_mut().unwrap().tabs[0];
    *tab.run.as_ref().unwrap().results.lock().unwrap() = (0..2000)
        .map(|i| {
            let name = format!("item-{i:04}.txt");
            Entry::named(
                std::env::temp_dir().join(&name),
                name,
                false,
                fsx::Kind::Text,
            )
        })
        .collect();
    tab.sort_dirty = true;
    tab.collect();
    let first = painted(&ctx, &mut app, "item-0000.txt");
    click(&ctx, &mut app, first.center());
    frame(
        &ctx,
        &mut app,
        vec![Event::Key {
            key: Key::End,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let last = painted(&ctx, &mut app, "item-1999.txt");
    assert!(last.min.y > 0.0 && last.max.y < 900.0);
    let tab = &app.search.as_ref().unwrap().tabs[0];
    assert!(tab.scroll > 0.0);
    assert!(
        tab.selected
            .contains(&std::env::temp_dir().join("item-1999.txt"))
    );
}

#[test]
fn stopping_a_search_keeps_partial_results() {
    let (mut app, ctx) = app();
    let run = app.search.as_ref().unwrap().tabs[0].run.as_ref().unwrap();
    run.done.store(false, Ordering::Release);
    run.progress.lock().unwrap().outcome = Outcome::Running;
    let stop = painted(&ctx, &mut app, "Stop");
    click(&ctx, &mut app, stop.center());
    let tab = &app.search.as_ref().unwrap().tabs[0];
    assert!(tab.run.as_ref().unwrap().cancel.load(Ordering::Relaxed));
    assert_eq!(tab.view.len(), 3);
}

#[test]
fn filters_and_the_first_result_remain_visible_in_a_small_window() {
    for size in [vec2(640.0, 480.0), vec2(900.0, 700.0), vec2(1200.0, 900.0)] {
        let (mut app, ctx) = app();
        app.search.as_mut().unwrap().tabs[0].inputs.advanced = true;
        let screen = Rect::from_min_size(Pos2::ZERO, size);
        let mut visible = false;
        for _ in 0..8 {
            let mut out = ctx.run_ui(
                RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| app.draw(ui),
            );
            out.textures_delta.clear();
            visible |= out.shapes.iter().any(|shape| match &shape.shape {
                egui::Shape::Text(t) if t.galley.job.text == "result-a.txt" => {
                    let rect = t.galley.rect.translate(t.pos.to_vec2());
                    screen.contains_rect(rect) && shape.clip_rect.contains_rect(rect)
                }
                _ => false,
            });
        }
        assert!(visible, "the results were pushed out of {size:?}");
    }
}

#[test]
fn search_preview_uses_its_own_loader_and_never_replaces_the_main_preview() {
    let (mut app, ctx) = app();
    let mut tab = tab();
    tab.select(0, Modifiers::NONE);
    let entry = tab.cursor_entry().unwrap();
    let mut preview = Preview::new();
    preview.remember(
        PreviewKey::of(&entry),
        PreviewContent::Text(Arc::new(crate::preview::TextDoc::new(
            "independent preview".into(),
            false,
            Default::default(),
        ))),
    );
    tab.preview = Some(preview);
    let main = app.preview.path.clone();
    let mut out = ctx.run_ui(Default::default(), |ui| {
        app.search_preview(
            ui,
            &mut tab,
            Rect::from_min_size(Pos2::ZERO, vec2(300.0, 400.0)),
        )
    });
    out.textures_delta.clear();
    assert_eq!(app.preview.path, main);
    assert_eq!(
        tab.preview.as_ref().unwrap().path.as_ref(),
        Some(&entry.path)
    );
    assert!(out.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.job.text.contains("independent preview"))));
}
