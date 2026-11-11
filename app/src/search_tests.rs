use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("fc_search_{}_{stamp}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn file(&self, name: &str, size: usize) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; size]).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait(search: &Search) -> (Vec<PathBuf>, Progress) {
    let until = Instant::now() + Duration::from_secs(5);
    while !search.done.load(Ordering::Acquire) {
        assert!(Instant::now() < until, "search did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
    (
        search
            .results
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.path.clone())
            .collect(),
        search.progress.lock().unwrap().clone(),
    )
}

#[test]
fn masks_types_sizes_and_exclusions_share_one_walker() {
    let root = Fixture::new();
    let a = root.file("Song.MP3", 20);
    let b = root.file("nested/song.flac", 40);
    root.file("excluded/song.mp3", 30);
    root.file("nested/song.txt", 30);
    root.file("small.mp3", 2);
    root.file("large.mp3", 80);
    let options = Criteria {
        name: "*.mp3;*.flac".into(),
        mode: NameMode::Wildcard,
        file_type: FileType::Audio,
        min_size: Some(10),
        max_size: Some(50),
        excluded_dirs: vec!["excluded".into()],
        ..Default::default()
    };
    let run = start(root.0.clone(), options.clone(), egui::Context::default()).unwrap();
    let (found, p) = wait(&run);
    assert_eq!(found.len(), 2);
    assert!(found.contains(&a) && found.contains(&b));
    assert_eq!(p.outcome, Outcome::Complete);
    assert_eq!(p.skipped, 0);
    let run = start(
        root.0.clone(),
        Criteria {
            recursive: false,
            ..options
        },
        egui::Context::default(),
    )
    .unwrap();
    assert_eq!(wait(&run).0, [a]);
}

#[test]
fn regex_case_and_modification_dates_are_validated() {
    let root = Fixture::new();
    let old = root.file("Report-2025.txt", 10);
    let new = root.file("Report-2026.txt", 10);
    let boundary = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&old)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(boundary - Duration::from_secs(10)))
        .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&new)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(boundary + Duration::from_secs(10)))
        .unwrap();
    let options = Criteria {
        name: "^report-[0-9]+\\.txt$".into(),
        mode: NameMode::Regex,
        modified_from: Some(boundary),
        ..Default::default()
    };
    let run = start(root.0.clone(), options.clone(), egui::Context::default()).unwrap();
    assert_eq!(wait(&run).0, [new]);
    let run = start(
        root.0.clone(),
        Criteria {
            case_sensitive: true,
            ..options
        },
        egui::Context::default(),
    )
    .unwrap();
    assert!(wait(&run).0.is_empty());
    assert!(
        start(
            root.0.clone(),
            Criteria {
                name: "[".into(),
                mode: NameMode::Regex,
                ..Default::default()
            },
            egui::Context::default()
        )
        .is_err()
    );
    assert!(
        start(
            root.0.clone(),
            Criteria {
                min_size: Some(100),
                max_size: Some(10),
                ..Default::default()
            },
            egui::Context::default()
        )
        .is_err()
    );
    let run = start(
        root.0.clone(),
        Criteria {
            modified_before: Some(boundary),
            file_type: FileType::Files,
            ..Default::default()
        },
        egui::Context::default(),
    )
    .unwrap();
    assert_eq!(wait(&run).0, [old]);
}

#[test]
fn incomplete_searches_report_errors_and_limits() {
    let root = Fixture::new();
    for i in 0..8 {
        root.file(&format!("result{i}"), 1);
    }
    let run = start(
        root.0.clone(),
        Criteria {
            limit: 3,
            ..Default::default()
        },
        egui::Context::default(),
    )
    .unwrap();
    let (found, p) = wait(&run);
    assert_eq!(found.len(), 3);
    assert_eq!(p.outcome, Outcome::Limited);
    assert!(p.limit_reason.is_some());
    let run = start(
        root.0.join("missing"),
        Criteria::default(),
        egui::Context::default(),
    )
    .unwrap();
    let (_, p) = wait(&run);
    assert_eq!(p.skipped, 1);
    assert_eq!(p.problems[0].0, root.0.join("missing"));
}

#[cfg(unix)]
#[test]
fn recursive_search_does_not_follow_link_cycles() {
    let root = Fixture::new();
    let file = root.file("sub/report.txt", 1);
    std::os::unix::fs::symlink(&root.0, root.0.join("sub/loop")).unwrap();
    let run = search(
        root.0.clone(),
        "report".into(),
        false,
        egui::Context::default(),
    );
    assert_eq!(wait(&run).0, [file]);
}

#[test]
fn sizes_accept_decimal_and_binary_units() {
    assert_eq!(parse_size("1.5 MiB").unwrap(), Some(1_572_864));
    assert_eq!(parse_size("10 MB").unwrap(), Some(10_000_000));
    assert_eq!(parse_size("").unwrap(), None);
    for bad in ["-1", "abc", "2 XB", "1e1000"] {
        assert!(parse_size(bad).is_err(), "{bad}");
    }
}
