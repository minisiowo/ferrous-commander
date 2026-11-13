//! The quick palette, driven through the real window with keys only.

use super::*;
use crate::palette::Mode;

fn palette(app: &App) -> &crate::ui_palette::PaletteUi {
    app.palette.as_ref().expect("the palette is open")
}

/// Runs frames until the palette lists `title`, as the search below the folder reports late.
fn wait_for_palette(ctx: &egui::Context, app: &mut App, title: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !palette(app).titles().contains(&title) {
        assert!(std::time::Instant::now() < deadline, "{title} never came up: {:?}", palette(app).titles());
        frame(ctx, app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A panel on 30 folders with the standard keys.
fn palette_app(tag: &str) -> (egui::Context, App, std::path::PathBuf) {
    let (ctx, mut app, root) = neovim_app(tag);
    app.st.vim_keys = false;
    (ctx, app, root)
}

fn alt_p(ctx: &egui::Context, app: &mut App) {
    // Alt lets the character through on some systems; it must not be typed anywhere.
    type_key(ctx, app, Key::P, Modifiers::ALT, Some("p"));
}

fn ctrl_shift_p(ctx: &egui::Context, app: &mut App) {
    type_key(ctx, app, Key::P, Modifiers::COMMAND | Modifiers::SHIFT, None);
}

fn settle(ctx: &egui::Context, app: &mut App) {
    for _ in 0..3 { frame(ctx, app, vec![], Modifiers::NONE); }
}

/// Where and in which font the text `wanted` is painted.
fn painted_font(ctx: &egui::Context, app: &mut App, wanted: &str) -> (egui::FontId, Pos2) {
    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 750.0))), ..Default::default() };
    let mut output = ctx.run_ui(input, |ui| app.draw(ui));
    output.textures_delta.clear();
    output.shapes.iter().find_map(|s| match &s.shape {
        egui::epaint::Shape::Text(t) if t.galley.job.text == wanted => Some((t.galley.job.sections[0].format.font_id.clone(), t.pos)),
        _ => None,
    }).unwrap_or_else(|| panic!("{wanted} is not painted"))
}

#[test]
fn the_hint_looks_like_the_text_that_replaces_it() {
    let (ctx, mut app, root) = palette_app("palette_hint");
    alt_p(&ctx, &mut app);
    let hint = painted_font(&ctx, &mut app, Mode::Places.hint());
    word(&ctx, &mut app, "x");
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    let typed = painted_font(&ctx, &mut app, "x");
    assert_eq!(hint.0, typed.0, "the same font");
    assert!((hint.1 - typed.1).abs().max_elem() < 0.51, "in the same place: {:?} and {:?}", hint.1, typed.1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn alt_p_opens_the_palette_which_takes_the_keyboard_and_goes_to_a_folder() {
    let (ctx, mut app, root) = palette_app("palette_go");
    alt_p(&ctx, &mut app);
    assert_eq!(palette(&app).query, "", "the shortcut's own letter is not typed");
    assert!(paints_text(&ctx, &mut app, Mode::Places.hint()));
    word(&ctx, &mut app, "dir07");
    assert_eq!(palette(&app).query, "dir07");
    assert_eq!(app.panes[0].filter, "", "the list behind does not filter");
    assert_eq!(palette(&app).titles()[0], "dir07");
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    assert!(app.palette.is_none(), "Enter closes it");
    settle(&ctx, &mut app);
    assert_eq!(app.panes[0].path(), root.join("dir07").as_path());
    assert_eq!(app.st.recent_folders[0], root.join("dir07"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_palette_moves_with_the_arrows_and_opens_a_new_tab_with_ctrl_enter() {
    let (ctx, mut app, root) = palette_app("palette_keys");
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "dir0");
    assert_eq!(palette(&app).titles()[..3], ["dir00", "dir01", "dir02"]);
    for arrow in [Key::ArrowDown, Key::ArrowDown, Key::ArrowDown, Key::ArrowUp] {
        key(&ctx, &mut app, arrow, Modifiers::NONE);
    }
    assert_eq!(palette(&app).selected, 2);
    assert_eq!(palette(&app).query, "dir0", "the arrows do not reach the text");
    key(&ctx, &mut app, Key::Enter, Modifiers::COMMAND);
    settle(&ctx, &mut app);
    assert_eq!(app.panes[0].tabs.len(), 2);
    assert_eq!(app.panes[0].path(), root.join("dir02").as_path());

    alt_p(&ctx, &mut app);
    key(&ctx, &mut app, Key::ArrowUp, Modifiers::NONE);
    assert_eq!(palette(&app).selected, palette(&app).titles().len() - 1, "up from the top goes to the end");
    key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    assert!(app.palette.is_none(), "Esc closes it");
    alt_p(&ctx, &mut app);
    alt_p(&ctx, &mut app);
    assert!(app.palette.is_none(), "its own shortcut closes it too");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_palette_runs_commands_and_remembers_the_last_ones() {
    let (ctx, mut app, root) = palette_app("palette_commands");
    ctrl_shift_p(&ctx, &mut app);
    assert_eq!(palette(&app).mode(), Mode::Commands);
    word(&ctx, &mut app, "hidden");
    assert_eq!(palette(&app).query, ">hidden");
    assert_eq!(palette(&app).titles()[0], "Hidden files");
    assert!(paints_text(&ctx, &mut app, "Ctrl+H"), "the command's shortcut is shown");
    let hidden = app.st.show_hidden;
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    assert_eq!(app.st.show_hidden, !hidden);
    assert!(app.palette.is_none());
    assert_eq!(app.st.palette_recent, ["toggle_hidden"]);

    ctrl_shift_p(&ctx, &mut app);
    assert_eq!(palette(&app).titles()[0], "Hidden files", "the last one used comes first");
    word(&ctx, &mut app, "sidebar");
    assert_eq!(palette(&app).titles()[0], "Sidebar");
    key(&ctx, &mut app, Key::Enter, Modifiers::COMMAND);
    assert!(!app.st.show_sidebar);
    assert!(app.palette.is_some(), "Ctrl+Enter changes a setting and stays");
    key(&ctx, &mut app, Key::Enter, Modifiers::COMMAND);
    assert!(app.st.show_sidebar, "so it can be changed back at once");
    key(&ctx, &mut app, Key::Escape, Modifiers::NONE);

    ctrl_shift_p(&ctx, &mut app);
    word(&ctx, &mut app, "newtab");
    assert_eq!(palette(&app).titles()[0], "New tab", "letters may skip the space");
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    settle(&ctx, &mut app);
    assert_eq!(app.panes[0].tabs.len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tab_changes_what_the_palette_lists_and_history_is_latest_first() {
    let (ctx, mut app, root) = palette_app("palette_history");
    for name in ["dir03", "dir11", "dir20"] {
        app.navigate(0, root.join(name), None);
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
    }
    alt_p(&ctx, &mut app);
    assert_eq!(palette(&app).titles()[..2], ["dir11", "dir03"], "recent folders first, without the one in view");
    key(&ctx, &mut app, Key::Tab, Modifiers::NONE);
    assert_eq!(palette(&app).mode(), Mode::Commands);
    key(&ctx, &mut app, Key::Tab, Modifiers::NONE);
    assert_eq!(palette(&app).mode(), Mode::History);
    assert_eq!(palette(&app).titles(), ["dir20", "dir11", "dir03"]);
    // Not just "03": text also matches the parent path, and that holds this process's id.
    word(&ctx, &mut app, "r03");
    assert_eq!(palette(&app).query, "@r03", "typing goes on after the mode's character");
    assert_eq!(palette(&app).titles(), ["dir03"]);
    key(&ctx, &mut app, Key::Tab, Modifiers::SHIFT);
    assert_eq!(palette(&app).query, ">r03", "the text is kept across modes");
    key(&ctx, &mut app, Key::Tab, Modifiers::NONE);
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    settle(&ctx, &mut app);
    assert_eq!(app.panes[0].path(), root.join("dir03").as_path());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_palette_finds_files_below_the_folder_and_shows_them_in_place() {
    let (ctx, mut app, root) = palette_app("palette_deep");
    let file = root.join("dir05").join("inner").join("quarterly report.txt");
    std::fs::write(&file, b"x").unwrap();
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "quarterly");
    wait_for_palette(&ctx, &mut app, "quarterly report.txt");
    assert_eq!(palette(&app).titles(), ["quarterly report.txt"]);
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.panes[0].selected.contains(&file) {
        assert!(std::time::Instant::now() < deadline, "the file was never selected");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(app.panes[0].path(), file.parent().unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn erasing_letters_keeps_the_files_found_on_screen_while_the_search_starts_again() {
    let (ctx, mut app, root) = palette_app("palette_erase");
    for name in ["daniel-a.txt", "daniel-b.txt"] {
        std::fs::write(root.join("dir05").join("inner").join(name), b"x").unwrap();
    }
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "daniel");
    wait_for_palette(&ctx, &mut app, "daniel-a.txt");
    wait_for_palette(&ctx, &mut app, "daniel-b.txt");
    // From "daniel" down to "da": each shorter text still matches both files.
    for _ in 0..4 {
        key(&ctx, &mut app, Key::Backspace, Modifiers::NONE);
        let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < until {
            frame(&ctx, &mut app, vec![], Modifiers::NONE);
            let titles = palette(&app).titles();
            assert!(titles.contains(&"daniel-a.txt") && titles.contains(&"daniel-b.txt"),
                "for {:?} the list lost a file: {titles:?}", palette(&app).query);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    assert_eq!(palette(&app).query, "da");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn text_that_narrows_an_ended_search_is_answered_without_searching_again() {
    let (ctx, mut app, root) = palette_app("palette_narrow");
    for name in ["quarterly report.txt", "quartz.txt"] {
        std::fs::write(root.join("dir05").join("inner").join(name), b"x").unwrap();
    }
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "quart");
    wait_for_palette(&ctx, &mut app, "quartz.txt");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while palette(&app).searching() {
        assert!(std::time::Instant::now() < deadline, "the search never ended");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    word(&ctx, &mut app, "erly");
    let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
    while std::time::Instant::now() < until {
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        assert!(!palette(&app).searching(), "the disk is not walked again");
        assert!(palette(&app).titles().contains(&"quarterly report.txt"), "{:?}", palette(&app).titles());
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(palette(&app).titles(), ["quarterly report.txt"]);
    // Erasing widens the text, which only the disk can answer.
    for _ in 0..6 { key(&ctx, &mut app, Key::Backspace, Modifiers::NONE); }
    assert_eq!(palette(&app).query, "qua");
    wait_for_palette(&ctx, &mut app, "quartz.txt");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_palette_goes_to_a_typed_path_and_its_shortcut_can_be_changed() {
    use crate::keymap::{Command, Layer, parse_keys};
    let (ctx, mut app, root) = palette_app("palette_path");
    app.st.keymap.set_keys(Layer::Standard, Command::Palette.info(), &[parse_keys("ctrl+y").unwrap()]);
    app.keymap_changed();
    type_key(&ctx, &mut app, Key::P, Modifiers::ALT, None);
    assert!(app.palette.is_none(), "the old shortcut is free");
    type_key(&ctx, &mut app, Key::Y, Modifiers::COMMAND, None);
    let wanted = root.join("dir09").join("inner");
    app.palette.as_mut().unwrap().query = wanted.display().to_string();
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    assert_eq!(palette(&app).titles()[0], wanted.display().to_string());
    assert!(!palette(&app).searching(), "a path is gone to, not searched for");
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    settle(&ctx, &mut app);
    assert_eq!(app.panes[0].path(), wanted.as_path());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_question_takes_the_keyboard_from_the_palette() {
    let (ctx, mut app, root) = palette_app("palette_dialog");
    alt_p(&ctx, &mut app);
    app.dialog = Some(crate::app::Dialog::NewGroup { name: String::new(), focus: true });
    settle(&ctx, &mut app);
    assert!(app.palette.is_none());
    app.dialog = None;
    std::fs::remove_dir_all(root).unwrap();
}

fn paths(app: &App) -> Vec<Vec<std::path::PathBuf>> {
    app.panes.iter().map(|p| p.tabs.iter().map(|t| t.path.clone()).collect()).collect()
}

#[test]
fn a_layout_brings_back_its_panels_from_alt_p() {
    let (ctx, mut app, root) = palette_app("layouts");
    assert!(!paints_text(&ctx, &mut app, "Layouts"), "no card while there are no layouts");
    app.acts.push(crate::app::Act::OpenPanel(0, Some(root.join("dir03"))));
    settle(&ctx, &mut app);
    app.panes[0].tabs.push(crate::app::Tab::new(root.join("dir04")));
    let saved = paths(&app);
    let active = app.active;
    app.acts.push(crate::app::Act::SaveLayout("work".into()));
    settle(&ctx, &mut app);
    assert_eq!(app.st.layouts.len(), 1);
    assert!(paints_text(&ctx, &mut app, "Layouts"), "the card shows once there is one");
    app.acts.push(crate::app::Act::SaveLayout("work".into()));
    settle(&ctx, &mut app);
    assert_eq!(app.st.layouts.len(), 1, "a second layout cannot take the same name");

    // Another layout: fewer panels, other folders.
    while app.panes.len() > 1 {
        app.acts.push(crate::app::Act::ClosePanel(app.panes.len() - 1));
        settle(&ctx, &mut app);
    }
    app.panes[0].tabs = vec![crate::app::Tab::new(root.join("dir09"))];
    app.panes[0].active = 0;
    settle(&ctx, &mut app);

    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "work");
    assert_eq!(palette(&app).titles()[0], "work");
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    settle(&ctx, &mut app);
    assert_eq!(paths(&app), saved, "every panel and tab comes back");
    assert_eq!(app.active, active);
    let mut leaves = Vec::new();
    app.tiles.leaves(&mut leaves);
    assert_eq!(leaves.len(), app.panes.len(), "the layout covers every panel");

    app.acts.push(crate::app::Act::RenameLayout(0, "home".into()));
    app.acts.push(crate::app::Act::DeleteLayout(0));
    settle(&ctx, &mut app);
    assert!(app.st.layouts.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_layout_whose_folder_is_gone_opens_on_what_is_left() {
    let (ctx, mut app, root) = palette_app("layout_gone");
    let gone = root.join("dir05/inner");
    app.panes[0].tabs = vec![crate::app::Tab::new(gone.clone())];
    app.acts.push(crate::app::Act::SaveLayout("gone".into()));
    settle(&ctx, &mut app);
    std::fs::remove_dir_all(&gone).unwrap();
    app.panes[0].tabs = vec![crate::app::Tab::new(root.clone())];
    app.acts.push(crate::app::Act::OpenLayout(0));
    app.acts.push(crate::app::Act::OpenLayout(7));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.panes[0].path() != root.join("dir05") {
        assert!(std::time::Instant::now() < deadline, "stayed on {}", app.panes[0].path().display());
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_sidebar_actions_and_every_on_off_setting_are_in_the_palette() {
    let (ctx, mut app, root) = palette_app("palette_complete");
    ctrl_shift_p(&ctx, &mut app);
    word(&ctx, &mut app, "new");
    let titles = palette(&app).titles();
    for wanted in ["New layout from the current panels…", "New favorites group…"] {
        assert!(titles.contains(&wanted), "{wanted} in {titles:?}");
    }
    for toggle in crate::settings::TOGGLES.iter().filter(|t| t.here() && t.id != "WindowEffect") {
        key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
        ctrl_shift_p(&ctx, &mut app);
        word(&ctx, &mut app, toggle.title);
        assert_eq!(palette(&app).titles()[0], toggle.title);
    }
    key(&ctx, &mut app, Key::Escape, Modifiers::NONE);
    let before = app.st.show_cloud;
    ctrl_shift_p(&ctx, &mut app);
    word(&ctx, &mut app, "cloud folders");
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    settle(&ctx, &mut app);
    assert_eq!(app.st.show_cloud, !before, "Enter flips it");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_file_opened_before_is_offered_from_any_folder() {
    let (ctx, mut app, root) = palette_app("palette_opened");
    let file = root.join("dir05").join("inner").join("budget.ods");
    std::fs::write(&file, b"x").unwrap();
    // As the system reports it opened; opening it for real would start another program.
    app.report_opened(&file, Ok(()));
    app.report_opened(&root.join("dir06"), Ok(()));
    assert_eq!(app.st.recent_files, [file.clone()], "folders are not files");
    app.navigate(0, root.join("dir09"), None);
    settle(&ctx, &mut app);
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "budg");
    assert_eq!(palette(&app).titles()[0], "budget.ods", "found without searching below the folder");
    std::fs::remove_dir_all(root).unwrap();
}

/// Indexes `root` as if it were home, and waits until the index is there.
fn index_root(ctx: &egui::Context, app: &mut App, root: &std::path::Path) {
    app.st.index_roots = vec![root.to_path_buf()];
    let generation = app.index.generation;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.index.generation == generation {
        assert!(std::time::Instant::now() < deadline, "the index was never built");
        frame(ctx, app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_for_index_answer(ctx: &egui::Context, app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !palette(app).index_answered() {
        assert!(std::time::Instant::now() < deadline, "the index never answered");
        frame(ctx, app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    settle(ctx, app);
}

#[test]
fn the_index_finds_what_lies_outside_the_folder_in_view() {
    let (ctx, mut app, root) = palette_app("palette_index");
    let far = root.join("dir05").join("inner").join("far-away notes.txt");
    std::fs::write(&far, b"x").unwrap();
    app.navigate(0, root.join("dir09"), None);
    index_root(&ctx, &mut app, &root);
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "far-away");
    wait_for_palette(&ctx, &mut app, "far-away notes.txt");
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.panes[0].selected.contains(&far) {
        assert!(std::time::Instant::now() < deadline, "the file was never selected");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn what_is_below_the_folder_in_view_comes_before_the_same_name_elsewhere() {
    let (ctx, mut app, root) = palette_app("palette_index_rank");
    for dir in ["dir05", "dir09"] {
        std::fs::write(root.join(dir).join("inner").join("budget.txt"), b"x").unwrap();
    }
    app.navigate(0, root.join("dir09"), None);
    index_root(&ctx, &mut app, &root);
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "budget");
    wait_for_index_answer(&ctx, &mut app);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while palette(&app).searching() {
        assert!(std::time::Instant::now() < deadline, "the search never ended");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    settle(&ctx, &mut app);
    assert_eq!(palette(&app).titles(), ["budget.txt", "budget.txt"]);
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    settle(&ctx, &mut app);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.panes[0].selected.is_empty() {
        assert!(std::time::Instant::now() < deadline, "nothing was selected");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.panes[0].selected.contains(&root.join("dir09").join("inner").join("budget.txt")));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn what_was_deleted_since_indexing_is_not_offered() {
    let (ctx, mut app, root) = palette_app("palette_index_gone");
    let gone = root.join("dir05").join("inner").join("deleted-report.txt");
    std::fs::write(&gone, b"x").unwrap();
    app.navigate(0, root.join("dir09"), None);
    index_root(&ctx, &mut app, &root);
    std::fs::remove_file(&gone).unwrap();
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "deleted-report");
    wait_for_index_answer(&ctx, &mut app);
    assert!(palette(&app).titles().is_empty(), "{:?}", palette(&app).titles());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_place_gone_since_it_was_listed_is_reported_and_not_gone_to() {
    let (ctx, mut app, root) = palette_app("palette_index_vanish");
    let gone = root.join("dir05").join("inner").join("vanishing.txt");
    std::fs::write(&gone, b"x").unwrap();
    app.navigate(0, root.join("dir09"), None);
    index_root(&ctx, &mut app, &root);
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "vanishing");
    wait_for_palette(&ctx, &mut app, "vanishing.txt");
    std::fs::remove_file(&gone).unwrap();
    key(&ctx, &mut app, Key::Enter, Modifiers::NONE);
    settle(&ctx, &mut app);
    assert_eq!(app.panes[0].path(), root.join("dir09").as_path());
    assert!(app.toasts.iter().any(|t| t.text.contains("no longer exists")));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn with_the_index_off_only_the_folder_in_view_is_searched() {
    let (ctx, mut app, root) = palette_app("palette_index_off");
    std::fs::write(root.join("dir05").join("inner").join("elsewhere.txt"), b"x").unwrap();
    app.navigate(0, root.join("dir09"), None);
    index_root(&ctx, &mut app, &root);
    app.st.index_home = false;
    settle(&ctx, &mut app);
    assert!(app.index.index().is_none(), "turning it off lets the index go");
    alt_p(&ctx, &mut app);
    word(&ctx, &mut app, "elsewhere");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    frame(&ctx, &mut app, vec![], Modifiers::NONE);
    std::thread::sleep(std::time::Duration::from_millis(200));
    while palette(&app).searching() {
        assert!(std::time::Instant::now() < deadline, "the search never ended");
        frame(&ctx, &mut app, vec![], Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    settle(&ctx, &mut app);
    assert!(palette(&app).titles().is_empty(), "{:?}", palette(&app).titles());
    std::fs::remove_dir_all(root).unwrap();
}
