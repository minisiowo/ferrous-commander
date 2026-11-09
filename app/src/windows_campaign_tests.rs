//! The Windows test campaign: real volumes, a real Recycle Bin, deliberate failures. Each test
//! asserts the safe behaviour, so a failure is a finding. Needs the test disks listed in
//! C:\FerrousTest\vhd\letters.txt, the loopback share and Developer Mode. Run with:
//! cargo test --release campaign_ -- --ignored --nocapture --test-threads=1
use super::*;
use crate::windows_fs::exact;
use std::os::windows::fs::{symlink_dir, symlink_file, MetadataExt, OpenOptionsExt};

const PREFIX: &str = "ferrous-campaign-";
const LOCAL: &str = r"C:\FerrousTest\work";
const SHARE: &str = r"\\localhost\C$\FerrousTest\unc";

fn disk(label: &str) -> PathBuf {
    let list = std::fs::read_to_string(r"C:\FerrousTest\vhd\letters.txt").expect("letters.txt");
    let root = list.lines().find_map(|line| line.strip_prefix(&format!("{label}=")).map(str::to_owned));
    PathBuf::from(root.unwrap_or_else(|| panic!("no disk {label}")))
}

/// Name, place, and whether its filesystem has NTFS features.
fn targets() -> Vec<(&'static str, PathBuf, bool)> {
    vec![
        ("same-volume", PathBuf::from(LOCAL), true),
        ("share", PathBuf::from(SHARE), true),
        ("fat32", disk("FCFAT32"), false),
        ("exfat", disk("FCEXFAT"), false),
        ("small-ntfs", disk("FCNTFS1G"), true),
    ]
}

/// A private folder, removed afterwards even when it holds names like `nul`.
struct Work(PathBuf);
impl Work {
    fn new(parent: impl AsRef<Path>, tag: &str) -> Self {
        let plain = parent.as_ref().join(format!("{PREFIX}{tag}-{}", std::process::id()));
        let path = exact(&plain).unwrap();
        wipe(&path);
        std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        Self(path)
    }
    fn sub(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
impl Drop for Work { fn drop(&mut self) { wipe(&self.0); } }

fn no(cancel: bool) -> AtomicBool { AtomicBool::new(cancel) }

fn bin_items(name: &str) -> Vec<trash::TrashItem> {
    trash::os_limited::list().unwrap().into_iter().filter(|item| item.name == std::ffi::OsString::from(name)).collect()
}
fn bin_has(name: &str) -> bool {
    for _ in 0..40 {
        if !bin_items(name).is_empty() { return true; }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}
fn bin_clean(name: &str) {
    let items = bin_items(name);
    if !items.is_empty() { let _ = trash::os_limited::purge_all(items); }
}

/// What the Trash action really did to `listed`. Panics when the app would have said
/// "moved to the Recycle Bin" about something that is not there.
fn recycle(listed: &Path) -> String {
    let src = exact(listed).unwrap();
    let name = listed.file_name().unwrap().to_str().unwrap().to_owned();
    if let Some(reason) = recycle_blocker(&src) {
        assert!(std::fs::symlink_metadata(&src).is_ok(), "blocked, yet the item is gone");
        return format!("blocked: {reason}");
    }
    match shell_delete(&src) {
        Err(e) => {
            assert!(std::fs::symlink_metadata(&src).is_ok(), "reported an error, yet the item is gone: {e}");
            format!("refused: {e}")
        }
        Ok(_) => {
            let kept = bin_has(&name);
            bin_clean(&name);
            assert!(kept, "DATA LOSS: {listed:?} was reported as recycled but is not in the Recycle Bin");
            "in the Recycle Bin".into()
        }
    }
}

// ---------- data-loss hypotheses ----------

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h1_recycling_through_a_link_to_a_share_is_not_silent_destruction() {
    let (here, there) = (Work::new(LOCAL, "h1-here"), Work::new(SHARE, "h1-there"));
    let name = format!("{PREFIX}h1.txt");
    std::fs::write(there.0.join(&name), b"precious").unwrap();
    let link = here.0.join("link");
    symlink_dir(format!("{SHARE}\\{}", there.0.file_name().unwrap().to_str().unwrap()), &link).unwrap();
    eprintln!("[H1 share] {}", recycle(&link.join(&name)));
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h1_recycling_through_a_link_to_another_disk() {
    for label in ["FCNTFS1G", "FCFAT32", "FCEXFAT"] {
        let (here, there) = (Work::new(LOCAL, "h1b-here"), Work::new(disk(label), "h1b-there"));
        let name = format!("{PREFIX}h1b-{label}.txt");
        std::fs::write(there.0.join(&name), b"precious").unwrap();
        let link = here.0.join("link");
        symlink_dir(crate::windows_fs::shown(&there.0), &link).unwrap();
        eprintln!("[H1 {label}] {}", recycle(&link.join(&name)));
    }
}

struct Subst(char);
impl Subst {
    fn new(letter: char, target: &Path) -> Self {
        let status = std::process::Command::new("subst").arg(format!("{letter}:")).arg(crate::windows_fs::shown(target)).status().unwrap();
        assert!(status.success(), "subst failed");
        Self(letter)
    }
}
impl Drop for Subst {
    fn drop(&mut self) { let _ = std::process::Command::new("subst").arg(format!("{}:", self.0)).arg("/d").status(); }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h2_subst_drive_is_not_silent_destruction() {
    let target = Work::new(LOCAL, "h2-subst");
    let _drive = Subst::new('X', &target.0);
    let name = format!("{PREFIX}h2-subst.txt");
    let listed = PathBuf::from(r"X:\").join(&name);
    std::fs::write(&listed, b"precious").unwrap();
    eprintln!("[H2 subst] {}", recycle(&listed));
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h2_files_near_the_default_bin_limit() {
    for (label, sizes) in [("FCNTFS1G", [50u64, 150, 400, 800]), ("FCFAT32", [50, 150, 400, 800]), ("FCEXFAT", [50, 500, 1500, 3000])] {
        let work = Work::new(disk(label), "h2-size");
        for megabytes in sizes {
            let name = format!("{PREFIX}h2-{label}-{megabytes}.bin");
            let listed = work.0.join(&name);
            let file = std::fs::File::create(&listed).unwrap();
            file.set_len(megabytes << 20).unwrap();
            drop(file);
            eprintln!("[H2 {label} {megabytes} MB] {}", recycle(&crate::windows_fs::shown(&listed)));
            let _ = std::fs::remove_file(&listed);
        }
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h3_probe_how_each_filesystem_answers_a_flush() {
    for (tag, place, _) in targets() {
        let work = Work::new(&place, "h3");
        let file = work.0.join("file");
        std::fs::write(&file, b"x").unwrap();
        for (what, path) in [("file", &file), ("folder", &work.0)] {
            let result = std::fs::OpenOptions::new().write(true).custom_flags(0x02000000).open(path).and_then(|f| f.sync_all());
            eprintln!("[H3 {tag} {what}] {result:?}");
        }
    }
}

fn stream(path: &Path) -> PathBuf {
    let mut with = path.as_os_str().to_owned();
    with.push(":campaign");
    PathBuf::from(with)
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h4_a_move_does_not_silently_discard_a_named_stream() {
    let mut lost = Vec::new();
    for (tag, place, _) in targets() {
        if tag == "same-volume" { continue; }
        let (src, dst) = (Work::new(LOCAL, "h4-src"), Work::new(&place, "h4-dst"));
        let file = src.0.join("with stream.txt");
        std::fs::write(&file, b"main").unwrap();
        std::fs::write(stream(&file), b"author notes").unwrap();
        let result = move_item(&file, &dst.0, &no(false));
        match result {
            Err(e) => {
                assert_eq!(std::fs::read(stream(&file)).unwrap(), b"author notes", "{tag}: {e}");
                eprintln!("[H4 {tag}] refused, source kept: {e}");
            }
            Ok(moved) => {
                let kept = std::fs::read(stream(&moved)).ok();
                eprintln!("[H4 {tag}] moved, stream {}", if kept.is_some() { "kept" } else { "LOST" });
                if kept.as_deref() != Some(&b"author notes"[..]) { lost.push(tag); }
            }
        }
    }
    assert!(lost.is_empty(), "named stream silently lost on {lost:?}");
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h6_recycling_a_folder_with_unaddressable_children() {
    let work = Work::new(LOCAL, "h6");
    let name = format!("{PREFIX}h6-folder");
    let folder = work.sub(&name);
    for child in ["report.", "report", "nul", "trail ", "ordinary.txt"] { std::fs::write(folder.join(child), child).unwrap(); }
    let before = manifest(&folder);
    let verdict = recycle(&crate::windows_fs::shown(&folder));
    eprintln!("[H6] {verdict}");
    if verdict != "in the Recycle Bin" { assert_eq!(manifest(&folder), before, "a refused delete changed the folder"); }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h7_a_source_written_through_an_open_handle_is_not_removed() {
    for (tag, place, _) in targets() {
        if tag == "same-volume" { continue; }
        let (src, dst) = (Work::new(LOCAL, "h7-src"), Work::new(&place, "h7-dst"));
        let file = src.0.join("log.txt");
        std::fs::write(&file, vec![7u8; 70_000]).unwrap();
        let held: Arc<Mutex<Option<std::fs::File>>> = Arc::default();
        let (writer, path) = (held.clone(), file.clone());
        AFTER_PROOF.set(Some(Box::new(move || {
            let mut handle = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            handle.write_all(b"written after the proof, handle still open").unwrap();
            *writer.lock().unwrap() = Some(handle);
        })));
        let result = move_item(&file, &dst.0, &no(false));
        assert!(AFTER_PROOF.take().is_none(), "never reached the gap: {result:?}");
        let survived = std::fs::read(&file).map(|data| data.ends_with(b"handle still open")).unwrap_or(false);
        drop(held.lock().unwrap().take());
        eprintln!("[H7 {tag}] {result:?}, source survived: {survived}");
        assert!(survived, "{tag}: a late write through an open handle was deleted with the source");
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h9_delete_for_good_handles_read_only_items_and_reports_locks() {
    for (tag, place, _) in targets() {
        let work = Work::new(&place, "h9");
        let folder = work.sub("doomed");
        let sub = folder.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let file = sub.join("read-only.txt");
        std::fs::write(&file, b"x").unwrap();
        set_mode(&file, 0o444);
        let lone = work.0.join("lone read-only.txt");
        std::fs::write(&lone, b"x").unwrap();
        set_mode(&lone, 0o444);
        let folder_result = destroy(&folder);
        let file_result = destroy(&lone);
        eprintln!("[H9 {tag}] folder {folder_result:?}, file {file_result:?}");
        assert!(folder_result.is_ok() && !folder.exists(), "{tag}: {folder_result:?}");
        assert!(file_result.is_ok() && !lone.exists(), "{tag}: {file_result:?}");

        let folder = work.sub("locked");
        let held = folder.join("held.txt");
        std::fs::write(&held, b"keep").unwrap();
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&held).unwrap();
        let result = destroy(&folder);
        assert!(result.is_err(), "{tag}: a locked child was not reported");
        drop(lock);
        assert_eq!(std::fs::read(&held).unwrap(), b"keep", "{tag}");
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h11_a_very_long_path_is_handled_honestly() {
    let work = Work::new(LOCAL, "h11");
    let mut dir = work.0.clone();
    for _ in 0..14 { dir.push("d".repeat(24)); }
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!("{PREFIX}h11.txt");
    std::fs::write(dir.join(&name), b"precious").unwrap();
    let listed = crate::windows_fs::shown(&dir).join(&name);
    assert!(listed.as_os_str().len() > 300);
    let verdict = recycle(&listed);
    eprintln!("[H11] {verdict}");
    assert!(!verdict.contains("could not be identified"), "a healthy local disk was declared unknown, which invites a permanent delete: {verdict}");
}

// ---------- operations on every kind of location ----------

fn old_time(path: &Path) {
    let moment = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    let file = std::fs::OpenOptions::new().write(true).access_mode(0x100).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(moment)).unwrap();
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_shapes_copy_and_move_everywhere() {
    for (tag, place, ntfs) in targets() {
        let (src, dst) = (Work::new(LOCAL, "shape-src"), Work::new(&place, "shape-dst"));
        let tree = src.sub("tree");
        for size in [0usize, 1, 4095, 4096, 4097, 65_536, 1 << 20] {
            std::fs::write(tree.join(format!("size {size}")), fill(&mut Rng(size as u64 + 11), size)).unwrap();
        }
        for name in ["Zażółć gęślą jaźń.txt", "日本語のファイル.txt", "emoji 🦀.txt", "a b  c.txt", ".dotfile", "UPPER.TXT"] {
            std::fs::write(tree.join(name), name).unwrap();
        }
        let flagged = tree.join("flagged.txt");
        std::fs::write(&flagged, b"flags").unwrap();
        old_time(&flagged);
        set_mode(&flagged, 0o444);
        let hidden = tree.join("hidden system.txt");
        std::fs::write(&hidden, b"h").unwrap();
        assert!(std::process::Command::new("attrib").args(["+h", "+s"]).arg(crate::windows_fs::shown(&hidden)).status().unwrap().success());
        std::fs::create_dir_all(tree.join("a").join("b").join("c")).unwrap();
        std::fs::create_dir(tree.join("empty")).unwrap();
        let want_flags = std::fs::symlink_metadata(&flagged).unwrap().file_attributes() & 0x7;
        let want_time = std::fs::symlink_metadata(&flagged).unwrap().modified().unwrap();
        let original = manifest(&tree);

        let check = |copy: &Path, what: &str| {
            let now = manifest(copy);
            assert_eq!(now.len(), original.len(), "{tag} {what}: entry count");
            for (rel, item) in &original {
                match (item, now.get(rel)) {
                    (Item::File(h, n, _), Some(Item::File(i, m, _))) => assert!(h == i && n == m, "{tag} {what}: {rel:?} differs"),
                    (a, b) => assert_eq!(Some(a), b, "{tag} {what}: {rel:?}"),
                }
            }
            let md = std::fs::symlink_metadata(copy.join("flagged.txt")).unwrap();
            assert_eq!(md.file_attributes() & 0x7, want_flags, "{tag} {what}: attributes");
            let drift = md.modified().unwrap().duration_since(want_time).or_else(|e| Ok::<_, ()>(e.duration())).unwrap();
            assert!(drift.as_secs() <= 2, "{tag} {what}: modified time moved by {drift:?}");
            assert_ne!(std::fs::symlink_metadata(copy.join("hidden system.txt")).unwrap().file_attributes() & 0x6, 0, "{tag} {what}: hidden flag");
        };

        let copied = copy_item(&tree, &dst.0, &no(false)).unwrap_or_else(|e| panic!("{tag} copy: {e}"));
        check(&copied, "copy");
        assert_eq!(manifest(&tree), original, "{tag}: a copy changed the source");
        no_stage(&dst.0);

        let second = copy_item(&tree, &dst.0, &no(false)).unwrap_or_else(|e| panic!("{tag} second copy: {e}"));
        assert_ne!(second, copied, "{tag}: a collision reused the name");
        check(&copied, "first copy after a collision");

        let moved_to = dst.sub("moved");
        let moved = move_item(&tree, &moved_to, &no(false)).unwrap_or_else(|e| panic!("{tag} move: {e}"));
        check(&moved, "move");
        assert!(std::fs::symlink_metadata(&tree).is_err(), "{tag}: the source stayed");
        no_stage(&moved_to);
        eprintln!("[shapes {tag}] ok (ntfs features: {ntfs})");
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_awkward_names_never_hit_the_wrong_file() {
    let mut names: Vec<std::ffi::OsString> = ["nul", "CON", "com1", "report.", "trail ", "dots...", "report", "aux.txt"].iter().map(Into::into).collect();
    names.push(raw_name());
    for (tag, place, _) in targets() {
        let (src, dst) = (Work::new(LOCAL, "name-src"), Work::new(&place, "name-dst"));
        let sibling = src.0.join("report");
        for (k, name) in names.iter().enumerate() { std::fs::write(src.0.join(name), format!("content {k}")).unwrap(); }
        let (mut copied, mut refused) = (0, Vec::new());
        for (k, name) in names.iter().enumerate() {
            let file = src.0.join(name);
            let want = format!("content {k}");
            match copy_item(&file, &dst.0, &no(false)) {
                Ok(done) => { assert_eq!(std::fs::read_to_string(&done).unwrap(), want, "{tag} {name:?}: copied the wrong file"); copied += 1; }
                Err(e) => refused.push(format!("{name:?}: {e}")),
            }
            assert_eq!(std::fs::read_to_string(&file).unwrap(), want, "{tag} {name:?}: the source changed");
        }
        no_stage(&dst.0);
        let moved_to = dst.sub("moved");
        for (k, name) in names.iter().enumerate() {
            let file = src.0.join(name);
            let want = format!("content {k}");
            match move_item(&file, &moved_to, &no(false)) {
                Ok(done) => { assert_eq!(std::fs::read_to_string(&done).unwrap(), want, "{tag} {name:?}: moved the wrong file"); assert!(std::fs::symlink_metadata(&file).is_err()); }
                Err(_) => assert_eq!(std::fs::read_to_string(&file).unwrap(), want, "{tag} {name:?}: a refused move changed the source"),
            }
        }
        no_stage(&moved_to);
        eprintln!("[names {tag}] copied {copied}, refused {refused:?}");
        let _ = sibling;
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_case_only_rename_and_refused_collisions() {
    for (tag, place, _) in targets() {
        let work = Work::new(&place, "case");
        let (from, to) = (work.0.join("Mixed.txt"), work.0.join("mIXED.txt"));
        std::fs::write(&from, b"one").unwrap();
        rename_no_replace(&from, &to).unwrap_or_else(|e| panic!("{tag}: {e}"));
        let listed: Vec<_> = std::fs::read_dir(&work.0).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(listed, vec![std::ffi::OsString::from("mIXED.txt")], "{tag}");
        let other = work.0.join("other.txt");
        std::fs::write(&other, b"two").unwrap();
        assert!(rename_no_replace(&other, &work.0.join("MIXED.TXT")).is_err(), "{tag}: a rename replaced a file");
        assert_eq!(std::fs::read(&to).unwrap(), b"one", "{tag}");
        assert_eq!(std::fs::read(&other).unwrap(), b"two", "{tag}");
    }
}

fn pattern(path: &Path, bytes: u64) {
    let mut out = std::io::BufWriter::with_capacity(1 << 22, std::fs::File::create_new(path).unwrap());
    let mut block = fill(&mut Rng(99), 1 << 20);
    let mut k = 0u64;
    while k << 20 < bytes {
        block[..8].copy_from_slice(&k.to_le_bytes());
        out.write_all(&block[..(bytes - (k << 20)).min(1 << 20) as usize]).unwrap();
        k += 1;
    }
    out.flush().unwrap();
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_files_over_4_gib() {
    let src = Work::new(LOCAL, "big-src");
    let file = src.0.join("big.bin");
    pattern(&file, 5 << 30);
    let want = hash(&file);
    let fat = Work::new(disk("FCFAT32"), "big-dst");
    let refused = copy_item(&file, &fat.0, &no(false));
    assert!(refused.is_err(), "FAT32 cannot hold this file: {refused:?}");
    eprintln!("[big fat32] {refused:?}");
    no_stage(&fat.0);
    assert!(!fat.0.join("big.bin").exists());
    assert_eq!(hash(&file), want);
    let moved = move_item(&file, &fat.0, &no(false));
    assert!(moved.is_err(), "{moved:?}");
    assert_eq!(hash(&file), want, "the source changed after a refused move");
    no_stage(&fat.0);
    let exfat = Work::new(disk("FCEXFAT"), "big-exfat");
    let started = std::time::Instant::now();
    let copied = copy_item(&file, &exfat.0, &no(false)).unwrap();
    eprintln!("[big exfat] copied in {:?}", started.elapsed());
    assert_eq!(hash(&copied), want);
    std::fs::remove_file(&copied).unwrap();
    let moved = move_item(&file, &exfat.0, &no(false)).unwrap();
    assert_eq!(hash(&moved), want);
    assert!(!file.exists());
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_full_disk_keeps_everything() {
    let src = Work::new(LOCAL, "full-src");
    let single = src.0.join("too big.bin");
    pattern(&single, 3 << 29);
    let tree = src.sub("tree");
    for k in 0..130 { pattern(&tree.join(format!("part {k}")), 10 << 20); }
    let (single_hash, original) = (hash(&single), manifest(&tree));
    let root = disk("FCNTFS1G");
    let work = Work::new(&root, "full-dst");
    for (what, item) in [("single file", &single), ("tree", &tree)] {
        for moving in [false, true] {
            let result = if moving { move_item(item, &work.0, &no(false)) } else { copy_item(item, &work.0, &no(false)) };
            eprintln!("[full {what} moving={moving}] {result:?}");
            assert!(result.is_err(), "{what}: more than the disk holds, yet it succeeded");
            no_stage(&work.0);
            assert!(!work.0.join(item.file_name().unwrap()).exists(), "{what}: a partial item was published");
        }
    }
    assert_eq!(hash(&single), single_hash);
    assert_eq!(manifest(&tree), original);
    assert!(std::fs::read_dir(&work.0).unwrap().next().is_none(), "something stayed on the full disk");
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_cancel_in_the_middle_of_a_large_file() {
    let src = Work::new(LOCAL, "cancel-src");
    let file = src.0.join("large.bin");
    pattern(&file, 1500 << 20);
    let want = hash(&file);
    for (tag, place, _) in targets() {
        let dst = Work::new(&place, "cancel-dst");
        for (moving, delay) in [(false, 40u64), (false, 200), (true, 40), (true, 200)] {
            // On one volume a move is a rename: done before any cancel can arrive.
            if moving && tag == "same-volume" { continue; }
            let cancel = Arc::new(no(false));
            let flag = cancel.clone();
            let stopper = std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(delay)); flag.store(true, Ordering::Relaxed); });
            let result = if moving { move_item(&file, &dst.0, &cancel) } else { copy_item(&file, &dst.0, &cancel) };
            stopper.join().unwrap();
            eprintln!("[cancel {tag} moving={moving} {delay} ms] {result:?}");
            no_stage(&dst.0);
            match &result {
                Err(_) => { assert!(file.exists(), "{tag}: a cancelled move lost the source"); assert_eq!(std::fs::metadata(&file).unwrap().len(), want.1); }
                Ok(done) => { assert_eq!(hash(done), want, "{tag}"); std::fs::remove_file(done).unwrap(); if moving { break; } }
            }
            for entry in std::fs::read_dir(&dst.0).unwrap() { let path = entry.unwrap().path(); if result.is_err() && path.is_file() { assert_eq!(hash(&path), want, "{tag}: a partial file was published"); std::fs::remove_file(path).unwrap(); } }
        }
        if !file.exists() { break; }
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LockFile(file: *mut std::ffi::c_void, low: u32, high: u32, count_low: u32, count_high: u32) -> i32;
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_locked_files_are_reported_and_kept() {
    use std::os::windows::io::AsRawHandle;
    for (tag, place, _) in targets() {
        let (src, dst) = (Work::new(LOCAL, "lock-src"), Work::new(&place, "lock-dst"));
        let tree = src.sub("tree");
        std::fs::write(tree.join("free.txt"), b"free").unwrap();
        let exclusive = tree.join("exclusive.txt");
        std::fs::write(&exclusive, b"exclusive").unwrap();
        let hold = std::fs::OpenOptions::new().read(true).share_mode(0).open(&exclusive).unwrap();
        let original = { let mut m = Manifest::new(); m.insert("free.txt".into(), Item::Dir); m };
        for moving in [false, true] {
            let result = if moving { move_item(&tree, &dst.0, &no(false)) } else { copy_item(&tree, &dst.0, &no(false)) };
            if tag == "same-volume" && moving {
                eprintln!("[lock {tag} rename with a locked child] {result:?}");
                if let Ok(done) = &result { std::fs::rename(done, &tree).unwrap(); }
                continue;
            }
            assert!(result.is_err(), "{tag} moving={moving}: a locked file was skipped silently");
            no_stage(&dst.0);
            assert!(!dst.0.join("tree").exists(), "{tag}: a partial folder was published");
            assert_eq!(std::fs::read(tree.join("free.txt")).unwrap(), b"free", "{tag}");
        }
        drop(hold);
        assert_eq!(std::fs::read(&exclusive).unwrap(), b"exclusive", "{tag}");
        let _ = original;

        let ranged = src.0.join("ranged.bin");
        std::fs::write(&ranged, vec![5u8; 1 << 16]).unwrap();
        let hold = std::fs::OpenOptions::new().read(true).write(true).open(&ranged).unwrap();
        assert_ne!(unsafe { LockFile(hold.as_raw_handle(), 0, 0, 4096, 0) }, 0);
        let result = copy_item(&ranged, &dst.0, &no(false));
        eprintln!("[lock {tag} byte range] {result:?}");
        assert!(result.is_err(), "{tag}: a copy of a locked range claimed success");
        no_stage(&dst.0);
        assert!(!dst.0.join("ranged.bin").exists(), "{tag}");
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_links_are_never_followed() {
    for (tag, place, ntfs) in targets() {
        let (src, dst, outside) = (Work::new(LOCAL, "link-src"), Work::new(&place, "link-dst"), Work::new(LOCAL, "link-outside"));
        let precious = outside.0.join("precious.txt");
        std::fs::write(&precious, b"precious").unwrap();
        let plain_outside = crate::windows_fs::shown(&outside.0);
        let tree = src.sub("tree");
        std::fs::write(tree.join("ordinary.txt"), b"x").unwrap();
        symlink_dir(&plain_outside, tree.join("to folder")).unwrap();
        symlink_file(plain_outside.join("precious.txt"), tree.join("to file")).unwrap();
        symlink_file("missing target", tree.join("dangling")).unwrap();
        let original = manifest(&tree);

        let copied = copy_item(&tree, &dst.0, &no(false));
        eprintln!("[links {tag} copy] {copied:?}");
        no_stage(&dst.0);
        match &copied {
            Ok(done) => assert_eq!(manifest(done).len(), original.len(), "{tag}"),
            // Over SMB creating a link needs a right that Developer Mode does not grant.
            Err(_) => assert!(!ntfs || tag == "share", "{tag}: NTFS can hold links"),
        }
        let moved_to = dst.sub("moved");
        let moved = move_item(&tree, &moved_to, &no(false));
        eprintln!("[links {tag} move] {moved:?}");
        no_stage(&moved_to);
        match moved {
            Ok(done) => {
                assert_eq!(manifest(&done).len(), original.len(), "{tag}");
                assert_eq!(destroy(&done), Ok(None), "{tag}");
            }
            Err(_) => assert_eq!(manifest(&tree), original, "{tag}: a refused move changed the source"),
        }
        assert_eq!(std::fs::read(&precious).unwrap(), b"precious", "{tag}: deleting a link reached its target");

        let junction = src.0.join("junction");
        let made = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(crate::windows_fs::shown(&junction)).arg(&plain_outside).output().unwrap();
        assert!(made.status.success());
        assert!(copy_item(&junction, &dst.0, &no(false)).is_err(), "{tag}: a junction was copied");
        assert_eq!(destroy(&junction), Ok(None), "{tag}");
        assert_eq!(std::fs::read(&precious).unwrap(), b"precious", "{tag}: removing a junction reached its target");
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_folder_into_itself_is_refused() {
    let work = Work::new(LOCAL, "self");
    let tree = work.sub("tree");
    let inner = tree.join("inner");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(inner.join("file"), b"x").unwrap();
    let elsewhere = work.sub("elsewhere");
    let back = elsewhere.join("back");
    symlink_dir(crate::windows_fs::shown(&inner), &back).unwrap();
    let original = manifest(&tree);
    for dest in [&tree, &inner, &back] {
        assert!(copy_item(&tree, dest, &no(false)).is_err(), "{dest:?}");
        assert!(move_item(&tree, dest, &no(false)).is_err(), "{dest:?}");
        assert_eq!(manifest(&tree), original, "{dest:?}");
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_paths_beyond_the_old_limit() {
    for (tag, place, ntfs) in targets() {
        let (src, dst) = (Work::new(LOCAL, "long-src"), Work::new(&place, "long-dst"));
        let tree = src.sub("tree");
        let mut dir = tree.clone();
        for _ in 0..16 { dir.push("d".repeat(30)); }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.txt"), b"deep").unwrap();
        let original = manifest(&tree);
        let copied = copy_item(&tree, &dst.0, &no(false));
        eprintln!("[long {tag} copy] {}", copied.as_ref().map(|_| "ok".to_string()).unwrap_or_else(|e| e.clone()));
        no_stage(&dst.0);
        match &copied {
            Ok(done) => assert_eq!(manifest(done), original.iter().map(|(k, v)| (k.clone(), match v { Item::File(h, n, _) => Item::File(*h, *n, 0), x => x.clone() })).collect::<Manifest>(), "{tag}"),
            Err(e) => assert!(!ntfs, "{tag}: NTFS and shares accept long paths: {e}"),
        }
        let moved_to = dst.sub("moved");
        match move_item(&tree, &moved_to, &no(false)) {
            Ok(done) => { assert_eq!(manifest(&done).len(), original.len(), "{tag}"); assert!(std::fs::symlink_metadata(&tree).is_err()); }
            Err(_) => assert_eq!(manifest(&tree), original, "{tag}: a refused move changed the source"),
        }
        no_stage(&moved_to);
    }
}

fn user_name() -> String { format!("{}\\{}", std::env::var("USERDOMAIN").unwrap(), std::env::var("USERNAME").unwrap()) }

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_h2_a_folder_too_large_for_the_bin_with_an_unreadable_part() {
    let work = Work::new(disk("FCNTFS1G"), "h2-acl");
    let name = format!("{PREFIX}h2-acl");
    let folder = work.sub(&name);
    let locked = folder.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let big = std::fs::File::create(locked.join("large.bin")).unwrap();
    big.set_len(600 << 20).unwrap();
    drop(big);
    let plain = crate::windows_fs::shown(&locked);
    let icacls = |args: &[&str]| assert!(std::process::Command::new("icacls").arg(&plain).args(args).output().unwrap().status.success(), "{args:?}");
    let deny = format!("{}:(OI)(CI)(RX)", user_name());
    icacls(&["/deny", &deny]);
    let src = exact(&crate::windows_fs::shown(&folder)).unwrap();
    let blocked = recycle_blocker(&src);
    icacls(&["/remove:d", &user_name()]);
    eprintln!("[H2 unreadable] {blocked:?}");
    assert!(blocked.is_some(), "a folder over the limit passed because part of it could not be measured, and the Shell would destroy it");
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_sparse_and_compressed_files_keep_their_content() {
    for (tag, place, _) in targets() {
        let (src, dst) = (Work::new(LOCAL, "sparse-src"), Work::new(&place, "sparse-dst"));
        let plain = crate::windows_fs::shown(&src.0);
        let sparse = plain.join("sparse.bin");
        std::fs::write(&sparse, b"head").unwrap();
        assert!(std::process::Command::new("fsutil").args(["sparse", "setflag"]).arg(&sparse).output().unwrap().status.success(), "fsutil");
        let file = std::fs::OpenOptions::new().write(true).open(&sparse).unwrap();
        file.set_len(1 << 29).unwrap();
        drop(file);
        let compressed = plain.join("compressed.bin");
        std::fs::write(&compressed, vec![3u8; 1 << 22]).unwrap();
        assert!(std::process::Command::new("compact").args(["/c"]).arg(&compressed).output().unwrap().status.success(), "compact");
        for file in [&sparse, &compressed] {
            let want = hash(file);
            let result = copy_item(file, &dst.0, &no(false));
            no_stage(&dst.0);
            match result {
                Ok(done) => {
                    assert_eq!(hash(&done), want, "{tag} {file:?}");
                    eprintln!("[sparse {tag}] {:?}: copied, attributes {:#x}", file.file_name().unwrap(), std::fs::metadata(&done).unwrap().file_attributes());
                }
                Err(e) => eprintln!("[sparse {tag}] {:?}: {e}", file.file_name().unwrap()),
            }
            assert_eq!(hash(file), want, "{tag}: the source changed");
        }
    }
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_report_what_happens_to_private_permissions_on_a_move() {
    let (src, dst) = (Work::new(LOCAL, "acl-src"), Work::new(disk("FCNTFS1G"), "acl-dst"));
    let file = src.0.join("private.txt");
    std::fs::write(&file, b"secret").unwrap();
    let plain = crate::windows_fs::shown(&file);
    assert!(std::process::Command::new("icacls").arg(&plain).args(["/inheritance:r", "/grant:r", &format!("{}:F", user_name())]).output().unwrap().status.success());
    let moved = move_item(&file, &dst.0, &no(false)).unwrap();
    assert_eq!(std::fs::read(&moved).unwrap(), b"secret");
    let shown = std::process::Command::new("icacls").arg(crate::windows_fs::shown(&moved)).output().unwrap();
    eprintln!("[acl] {}", String::from_utf8_lossy(&shown.stdout).replace(['\r', '\n'], " "));
}

// ---------- archives ----------

fn zip_of(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn unpack(archive: &Path, dest: &Path) -> Result<crate::archive::Extracted, String> {
    crate::archive::extract(archive, dest, None, &no(false), &mut |_| {})
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_archive_names_windows_cannot_hold_are_left_out() {
    let work = Work::new(LOCAL, "zip-names");
    let (dest, archive) = (work.sub("dest"), work.0.join("names.zip"));
    let skipped = ["nul", "report.", "trail ", "ab:stream.txt", "com1.txt", "sub/con/x.txt", "ok.TXT"];
    let mut entries: Vec<(&str, &[u8])> = vec![("ok.txt", b"first"), ("sub/fine.txt", b"fine")];
    entries.extend(skipped.iter().map(|name| (*name, &b"bad"[..])));
    zip_of(&archive, &entries);
    let done = unpack(&archive, &dest).unwrap();
    eprintln!("[zip names] skipped {}", done.skipped);
    assert_eq!(done.skipped, skipped.len());
    let got = manifest(&done.path);
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(std::fs::read(done.path.join("ok.txt")).unwrap(), b"first", "the case twin replaced the first file");
    assert!(std::fs::symlink_metadata(done.path.join("ab")).is_err(), "an entry wrote a named stream");
    no_stage(&dest);
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_archive_entries_never_leave_the_destination() {
    let work = Work::new(LOCAL, "zip-escape");
    let outer = work.sub("outer");
    let dest = outer.join("dest");
    std::fs::create_dir(&dest).unwrap();
    let archive = work.0.join("escape.zip");
    let cwd_probe = std::env::current_dir().unwrap().join("evil.txt");
    let absolute = crate::windows_fs::shown(&outer).join("evil.txt");
    let absolute = absolute.to_str().unwrap().to_owned();
    let unc = format!(r"\\localhost\C$\FerrousTest\evil.txt");
    for name in ["../evil.txt", r"..\evil.txt", "a/../../evil.txt", r"a\..\..\evil.txt", "/evil.txt", r"\evil.txt", "C:evil.txt", "a:evil.txt", absolute.as_str(), unc.as_str()] {
        zip_of(&archive, &[(name, b"evil"), ("ok.txt", b"fine")]);
        let result = unpack(&archive, &dest);
        eprintln!("[zip escape {name}] {:?}", result.as_ref().map(|d| d.skipped));
        no_stage(&dest);
        for place in [outer.join("evil.txt"), work.0.join("evil.txt"), PathBuf::from(r"C:\FerrousTest\evil.txt"), PathBuf::from(r"C:\evil.txt"), cwd_probe.clone()] {
            assert!(std::fs::symlink_metadata(&place).is_err(), "{name} escaped to {place:?}");
        }
        if result.is_err() { assert!(std::fs::read_dir(&dest).unwrap().next().is_none(), "{name}: a refused archive left something"); }
        else { for entry in std::fs::read_dir(&dest).unwrap() { wipe(&entry.unwrap().path()); } }
    }
}

use crate::win::measure::memory_used_by;

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_decompression_bomb_fills_the_disk_and_nothing_else() {
    let src = Work::new(LOCAL, "bomb");
    let archive = src.0.join("bomb.zip");
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        zip.start_file("zeros.bin", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true)).unwrap();
        let block = vec![0u8; 1 << 20];
        for _ in 0..3072 { zip.write_all(&block).unwrap(); }
        zip.finish().unwrap();
    }
    eprintln!("[bomb] archive is {} KiB", std::fs::metadata(&archive).unwrap().len() >> 10);
    let dest = Work::new(disk("FCNTFS1G"), "bomb-dst");
    let (result, memory) = memory_used_by(|| unpack(&archive, &dest.0));
    eprintln!("[bomb] {:?}, memory {} MB", result.as_ref().map(|d| d.skipped), memory >> 20);
    assert!(result.is_err(), "3 GiB cannot fit on a 1 GiB disk");
    no_stage(&dest.0);
    assert!(std::fs::read_dir(&dest.0).unwrap().next().is_none(), "the bomb left something");
    assert!(memory < 256 << 20, "unpacking used {} MB", memory >> 20);
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_download_mark_survives_unpacking_and_copying() {
    let work = Work::new(LOCAL, "mark");
    let dest = work.sub("dest");
    let archive = work.0.join("downloaded.zip");
    zip_of(&archive, &[("setup.exe", b"MZ"), ("notes.txt", b"n")]);
    let mark = b"[ZoneTransfer]\r\nZoneId=3\r\n";
    let zone = |path: &Path| { let mut p = path.as_os_str().to_owned(); p.push(":Zone.Identifier"); PathBuf::from(p) };
    std::fs::write(zone(&archive), mark).unwrap();
    let done = unpack(&archive, &dest).unwrap();
    assert_eq!(std::fs::read(zone(&done.path.join("setup.exe"))).unwrap(), mark, "an unpacked program lost the download warning");
    let copied = copy_item(&done.path.join("setup.exe"), &dest, &no(false)).unwrap();
    assert_eq!(std::fs::read(zone(&copied)).unwrap(), mark, "a copy lost the download warning");
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_damaged_archives_never_crash() {
    use crate::compress::Kind;
    let rounds: u64 = std::env::var("FERROUS_CAMPAIGN_FUZZ").ok().and_then(|n| n.parse().ok()).unwrap_or(400);
    let work = Work::new(LOCAL, "fuzz");
    let tree = work.sub("tree");
    for k in 0..6 { std::fs::write(tree.join(format!("file {k}.txt")), fill(&mut Rng(k + 1), 3000)).unwrap(); }
    let out = work.sub("packed");
    let mut seeds = Vec::new();
    for (kind, name) in [(Kind::Zip, "s.zip"), (Kind::SevenZ, "s.7z"), (Kind::Tar, "s.tar"), (Kind::TarGz, "s.tar.gz"), (Kind::TarBz2, "s.tar.bz2"), (Kind::TarXz, "s.tar.xz")] {
        let packed = crate::compress::pack(std::slice::from_ref(&tree), &out, name, kind, &no(false), &mut |_| {}).unwrap();
        seeds.push((name, std::fs::read(out.join(packed.name)).unwrap()));
    }
    let (dest, mut panics, mut opened) = (work.sub("dest"), Vec::new(), 0u64);
    for (name, original) in &seeds {
        let mut rng = Rng(0xABCDEF + original.len() as u64);
        let target = work.0.join(name);
        for round in 0..rounds {
            let mut bytes = original.clone();
            match rng.below(3) {
                0 => bytes.truncate(rng.below(bytes.len() as u64) as usize),
                _ => for _ in 0..1 + rng.below(24) { let at = rng.below(bytes.len() as u64) as usize; bytes[at] = rng.next() as u8; },
            }
            std::fs::write(&target, &bytes).unwrap();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unpack(&target, &dest))) {
                Err(_) => panics.push(format!("{name} round {round}")),
                Ok(Ok(_)) => { opened += 1; for entry in std::fs::read_dir(&dest).unwrap() { wipe(&entry.unwrap().path()); } }
                Ok(Err(_)) => assert!(std::fs::read_dir(&dest).unwrap().next().is_none(), "{name} round {round}: a damaged archive left something"),
            }
        }
    }
    eprintln!("[fuzz archives] {} rounds, {opened} still opened, {} panics", rounds * seeds.len() as u64, panics.len());
    assert!(panics.is_empty(), "panics take the whole program down in release builds: {panics:?}");
}

#[test]
#[ignore = "Campaign: writes to C:\\FerrousTest and the test disks"]
fn campaign_many_small_files() {
    let count: usize = std::env::var("FERROUS_CAMPAIGN_SMALL").ok().and_then(|n| n.parse().ok()).unwrap_or(100_000);
    let src = Work::new(LOCAL, "small-src");
    let tree = src.sub("tree");
    let started = std::time::Instant::now();
    for folder in 0..count.div_ceil(1000) {
        let dir = tree.join(format!("folder {folder}"));
        std::fs::create_dir(&dir).unwrap();
        for k in 0..1000.min(count - folder * 1000) { std::fs::write(dir.join(format!("file {k}.txt")), format!("{folder}/{k}")).unwrap(); }
    }
    eprintln!("[small] made {count} files in {:?}", started.elapsed());
    let original = manifest(&tree);
    for (tag, place, _) in targets() {
        if tag == "fat32" || tag == "exfat" { continue; }
        let dst = Work::new(&place, "small-dst");
        let started = std::time::Instant::now();
        let copied = copy_item(&tree, &dst.0, &no(false)).unwrap_or_else(|e| panic!("{tag}: {e}"));
        eprintln!("[small {tag}] copy {:?}", started.elapsed());
        assert_eq!(manifest(&copied).len(), original.len(), "{tag}");
        no_stage(&dst.0);
        let started = std::time::Instant::now();
        assert_eq!(destroy(&copied), Ok(None), "{tag}");
        eprintln!("[small {tag}] delete for good {:?}", started.elapsed());
    }
    assert_eq!(manifest(&tree), original);
}
