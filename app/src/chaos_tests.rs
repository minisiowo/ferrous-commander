//! Randomised attacks on copy and move. The rule checked after every run: each original
//! entry still exists, byte for byte, in the source or in a complete copy. Failures print
//! their seed, so `FERROUS_CHAOS_SEED=<n>` replays one.
use super::*;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

thread_local! {
    /// Runs in the narrowest gap of a move: everything was proven, nothing is removed yet.
    pub static AFTER_PROOF: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    /// Plays the drive that loses data while reporting success.
    pub static DAMAGE_COPY: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> = const { std::cell::RefCell::new(None) };
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 { self.next() % n }
    fn chance(&mut self, percent: u64) -> bool { self.below(100) < percent }
}

#[derive(Clone, Copy)]
struct Profile { links: bool, raw_names: bool, special: bool }
#[cfg(unix)]
fn full() -> Profile { Profile { links: true, raw_names: true, special: true } }
/// Links need Developer Mode or a privilege. FIFOs do not exist, so here "special" means
/// the read-only attribute and hard links.
#[cfg(windows)]
fn full() -> Profile {
    static CAN_LINK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let links = *CAN_LINK.get_or_init(|| {
        let at = std::env::temp_dir().join(format!("fc_chaos_link_{}", std::process::id()));
        let made = std::os::windows::fs::symlink_file("missing target", &at).is_ok();
        let _ = std::fs::remove_file(&at);
        made
    });
    Profile { links, raw_names: true, special: true }
}
const FAT: Profile = Profile { links: false, raw_names: false, special: false };

#[derive(Clone, Debug, PartialEq)]
enum Item { File(u64, u64, u32), Dir, Link(PathBuf), Fifo }
type Manifest = BTreeMap<PathBuf, Item>;

#[cfg(unix)]
fn mode_of(md: &std::fs::Metadata) -> u32 { md.mode() & 0o777 }
#[cfg(windows)]
fn mode_of(md: &std::fs::Metadata) -> u32 { md.permissions().readonly() as u32 }

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) { std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap(); }
#[cfg(windows)]
fn set_mode(path: &Path, mode: u32) {
    let mut permissions = std::fs::symlink_metadata(path).unwrap().permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn is_fifo(t: std::fs::FileType) -> bool { t.is_fifo() }
#[cfg(windows)]
fn is_fifo(_: std::fs::FileType) -> bool { false }

/// A name that is not valid Unicode.
#[cfg(unix)]
fn raw_name() -> OsString { OsString::from_vec(vec![b'r', 0xff, 0xfe]) }
#[cfg(windows)]
fn raw_name() -> OsString { std::os::windows::ffi::OsStringExt::from_wide(&[b'r' as u16, 0xD800]) }

#[cfg(unix)]
fn dangling(target: &str, at: &Path) { std::os::unix::fs::symlink(target, at).unwrap(); }
#[cfg(windows)]
fn dangling(target: &str, at: &Path) { std::os::windows::fs::symlink_file(target, at).unwrap(); }

fn hash(path: &Path) -> (u64, u64) {
    use std::io::Read;
    let (mut h, mut n, mut buffer) = (0xcbf29ce484222325u64, 0u64, vec![0u8; 1 << 16]);
    let mut file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    loop {
        let k = file.read(&mut buffer).unwrap();
        if k == 0 { return (h, n); }
        for b in &buffer[..k] { h = (h ^ *b as u64).wrapping_mul(0x100000001b3); }
        n += k as u64;
    }
}

fn manifest(root: &Path) -> Manifest {
    let mut out = Manifest::new();
    let mut pending = vec![(root.to_path_buf(), PathBuf::new())];
    while let Some((path, rel)) = pending.pop() {
        let md = std::fs::symlink_metadata(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let t = md.file_type();
        let item = if t.is_symlink() { Item::Link(std::fs::read_link(&path).unwrap()) }
            else if t.is_dir() {
                for child in std::fs::read_dir(&path).unwrap() {
                    let child = child.unwrap();
                    pending.push((child.path(), rel.join(child.file_name())));
                }
                Item::Dir
            } else if is_fifo(t) { Item::Fifo }
            else { let (h, n) = hash(&path); Item::File(h, n, mode_of(&md)) };
        out.insert(rel, item);
    }
    out
}

fn entry_name(rng: &mut Rng, k: u64, p: Profile) -> OsString {
    const NAMES: &[&str] = &["plain", "Zażółć gęślą", "日本語", "with space", "-dash", "--help", "q'uote", "dbl\"q",
        "$(touch pwned)", "a;b", "back`tick", "100%", "new\nline", "tab\there", "emoji 🦀", ".hidden", "a..b", "*star", "?what", "[x]",
        ".ferrous-copy-1-0", "item", "x.copy-errors"];
    let name = NAMES[rng.below(NAMES.len() as u64) as usize];
    let raw = p.raw_names && rng.chance(8);
    let mut out = if raw { raw_name() } else { OsString::from(name) };
    if !raw && (!p.links || cfg!(windows)) {
        // What FAT cannot name at all is covered by real_mounts.
        let clean: String = out.to_string_lossy().chars().filter(|c| !"\"*?<>|\\:\n\t".contains(*c)).collect();
        out = OsString::from(clean.trim_end_matches(['.', ' ']));
    }
    out.push(format!(" {k}"));
    out
}

fn fill(rng: &mut Rng, size: usize) -> Vec<u8> {
    let seed = rng.next();
    (0..size as u64).map(|i| ((i ^ seed).wrapping_mul(2654435761) >> 11) as u8).collect()
}

fn grow(dir: &Path, rng: &mut Rng, depth: u32, p: Profile) {
    let mut files = Vec::new();
    for k in 0..2 + rng.below(6) {
        let path = dir.join(entry_name(rng, k, p));
        match rng.below(if p.special { 12 } else { 8 }) {
            0..=4 => {
                let size = [0, 1, 4095, 4096, 4097, 70_000, 1 << 20][rng.below(7) as usize];
                std::fs::write(&path, fill(rng, size)).unwrap();
                let mode = [0o644, 0o600, 0o444, 0o755][rng.below(4) as usize];
                if p.special { set_mode(&path, mode); }
                files.push(path);
            }
            5..=7 if depth > 0 => { std::fs::create_dir(&path).unwrap(); grow(&path, rng, depth - 1, p); }
            5..=7 => std::fs::create_dir(&path).unwrap(),
            8 if p.links => dangling("missing target", &path),
            9 if p.links => dangling("..", &path),
            #[cfg(unix)]
            10 => {
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o644) }, 0);
            }
            _ => if let Some(file) = files.first() { std::fs::hard_link(file, &path).unwrap(); } else { std::fs::write(&path, b"x").unwrap(); },
        }
    }
}

fn no_stage(dir: &Path) {
    let left: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().starts_with(".ferrous-copy-")).collect();
    assert!(left.is_empty(), "leftovers: {left:?}");
}

/// The rule: nothing is lost, and a published copy is never partial.
fn nothing_lost(original: &Manifest, src: &Path, dst_dir: &Path, name: &std::ffi::OsStr, what: &str) {
    let target = dst_dir.join(name);
    let copy = std::fs::symlink_metadata(&target).is_ok().then(|| manifest(&target));
    if let Some(copy) = &copy {
        // Modes are not part of a FAT copy, so only content counts on this side.
        let same = copy.len() == original.len() && copy.iter().zip(original).all(|((a, x), (b, y))| a == b && match (x, y) {
            (Item::File(h, n, _), Item::File(i, m, _)) => h == i && n == m,
            _ => x == y,
        });
        assert!(same, "{what}: a partial or wrong copy was published");
    }
    if std::fs::symlink_metadata(src).is_ok() {
        let now = manifest(src);
        for (rel, item) in &now {
            if let Some(before) = original.get(rel) { assert_eq!(item, before, "{what}: {rel:?} changed in the source"); }
        }
        let missing: Vec<_> = original.keys().filter(|rel| !now.contains_key(*rel)).collect();
        assert!(missing.is_empty() || copy.is_some(), "{what}: lost {missing:?}");
    } else {
        assert!(copy.is_some(), "{what}: the source went without a copy");
    }
}

/// Lets a tree with read-only folders be deleted afterwards.
fn wipe(path: &Path) {
    #[cfg(unix)]
    let _ = std::process::Command::new("chmod").args(["-R", "u+rwx", "--"]).arg(path).stderr(std::process::Stdio::null()).status();
    #[cfg(windows)]
    let _ = crate::windows_fs::remove_staged(path);
    let _ = std::fs::remove_dir_all(path);
}

fn seeds(count: u64) -> Vec<u64> {
    match std::env::var("FERROUS_CHAOS_SEED") {
        Ok(seed) => vec![seed.parse().unwrap()],
        Err(_) => (1..=count).map(|k| k.wrapping_mul(0x9E3779B97F4A7C15) | 1).collect(),
    }
}

struct Arena { src: PathBuf, dst: PathBuf }
impl Arena {
    fn new(there: &Path, tag: &str, seed: u64) -> Self {
        let id = format!("fc_chaos_{tag}_{}_{seed:x}", std::process::id());
        let (src, dst) = (std::env::temp_dir().join(&id), there.join(&id));
        for d in [&src, &dst] { wipe(d); std::fs::create_dir_all(d).unwrap(); }
        Self { src, dst }
    }
}
impl Drop for Arena {
    fn drop(&mut self) {
        wipe(&self.src);
        wipe(&self.dst);
        #[cfg(windows)]
        if let Some(parent) = self.dst.parent() { let _ = std::fs::remove_dir(parent); }
    }
}

/// The same disk through its administrative share: a real network location, and another
/// volume as far as Windows is concerned, with nothing to plug in. Skipped where it is off.
#[cfg(windows)]
fn other_filesystem() -> Option<PathBuf> {
    let tmp = std::env::temp_dir();
    let (drive, rest) = tmp.to_str()?.split_once(":\\")?;
    let there = PathBuf::from(format!(r"\\localhost\{drive}$\{rest}")).join("fc_chaos_share");
    std::fs::create_dir_all(&there).ok()?;
    Some(there)
}

#[cfg(unix)]
fn other_filesystem() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let (tmp, shm) = (std::fs::metadata(std::env::temp_dir()).ok()?, std::fs::metadata("/dev/shm").ok()?);
    (tmp.dev() != shm.dev()).then(|| "/dev/shm".into())
}

fn rounds(there: &Path, tag: &str, count: u64, p: Profile) {
    for seed in seeds(count) {
        let what = format!("{tag} seed {seed}");
        let (mut rng, arena) = (Rng(seed), Arena::new(there, tag, seed));
        let fresh = |rng: &mut Rng, k: u32| {
            let tree = arena.src.join(format!("tree {k}"));
            std::fs::create_dir(&tree).unwrap();
            grow(&tree, rng, 3, p);
            let original = manifest(&tree);
            (tree.file_name().unwrap().to_os_string(), tree, original)
        };

        // Undisturbed, a copy works, matches and leaves the source alone.
        let (name, tree, original) = fresh(&mut rng, 0);
        copy_item(&tree, &arena.dst, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{what}: copy: {e}"));
        nothing_lost(&original, &tree, &arena.dst, &name, &what);
        assert_eq!(manifest(&tree), original, "{what}: copying touched the source");
        no_stage(&arena.dst);

        // So does a move.
        let (name, tree, original) = fresh(&mut rng, 1);
        move_item(&tree, &arena.dst, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{what}: move: {e}"));
        assert!(std::fs::symlink_metadata(&tree).is_err(), "{what}: the source stayed");
        nothing_lost(&original, &tree, &arena.dst, &name, &what);
        no_stage(&arena.dst);

        // Cancelled at a random moment, including before and after the work.
        let (name, tree, original) = fresh(&mut rng, 2);
        let cancel = Arc::new(AtomicBool::new(false));
        let delay = rng.below(4000);
        let flag = cancel.clone();
        let stopper = std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_micros(delay)); flag.store(true, Ordering::Relaxed); });
        let result = move_item(&tree, &arena.dst, &cancel);
        stopper.join().unwrap();
        nothing_lost(&original, &tree, &arena.dst, &name, &format!("{what}, cancelled after {delay} us: {result:?}"));
        no_stage(&arena.dst);

        // Someone keeps writing into the source for the whole move, so that every phase is
        // hit, including the gap between checking the source and removing it.
        let (name, tree, _) = fresh(&mut rng, 3);
        let (stop, folder) = (Arc::new(AtomicBool::new(false)), tree.clone());
        let flag = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut written = Vec::new();
            // Paced: a slow drive keeps this going for a long time, and inodes run out.
            while !flag.load(Ordering::Relaxed) && written.len() < 2000 {
                let file = format!("written during the move {}", written.len());
                if std::fs::write(folder.join(&file), b"precious").is_ok() { written.push(file); }
                std::thread::sleep(std::time::Duration::from_micros(300));
            }
            written
        });
        let result = move_item(&tree, &arena.dst, &AtomicBool::new(false));
        stop.store(true, Ordering::Relaxed);
        let written = writer.join().unwrap();
        assert!(!written.is_empty(), "{what}: the writer never ran, so this proved nothing");
        for file in &written {
            let kept = std::fs::read(tree.join(file)).or_else(|_| std::fs::read(arena.dst.join(&name).join(file)));
            assert_eq!(kept.ok().as_deref(), Some(&b"precious"[..]), "{what}: {file} vanished ({result:?})");
        }
        no_stage(&arena.dst);

        // The same, in the gap no timing can hit: new items at every depth, and an edit.
        let (_, tree, _) = fresh(&mut rng, 4);
        let (mut folders, mut files) = (vec![tree.clone()], Vec::new());
        let mut k = 0;
        while k < folders.len() {
            for child in std::fs::read_dir(&folders[k]).unwrap() {
                let child = child.unwrap();
                let t = child.file_type().unwrap();
                if t.is_dir() { folders.push(child.path()); } else if t.is_file() { files.push(child.path()); }
            }
            k += 1;
        }
        let folder = folders[rng.below(folders.len() as u64) as usize].clone();
        let edited = (!files.is_empty()).then(|| files[rng.below(files.len() as u64) as usize].clone());
        let edit = edited.clone();
        let late = folder.join("arrived in the gap");
        let file = late.clone();
        AFTER_PROOF.set(Some(Box::new(move || {
            std::fs::write(file, b"precious").unwrap();
            if let Some(edit) = edit {
                set_mode(&edit, 0o644);
                std::fs::OpenOptions::new().append(true).open(edit).unwrap().write_all(b" edited later").unwrap();
            }
        })));
        let result = move_item(&tree, &arena.dst, &AtomicBool::new(false));
        assert!(AFTER_PROOF.take().is_none(), "{what}: the move never reached the gap: {result:?}");
        assert_eq!(std::fs::read(&late).ok().as_deref(), Some(&b"precious"[..]), "{what}: a new file was removed unseen");
        if let Some(edited) = edited {
            let text = std::fs::read(&edited).unwrap_or_else(|e| panic!("{what}: an edited file was removed: {e}"));
            assert!(text.ends_with(b" edited later"), "{what}");
        }
    }
}

#[test]
fn random_trees_survive_copy_cancel_and_writes_across_filesystems() {
    let Some(there) = other_filesystem() else { eprintln!("Skipped: no second filesystem"); return };
    // A round through a share takes seconds. FERROUS_CHAOS_ROUNDS asks for the full forty.
    let count = std::env::var("FERROUS_CHAOS_ROUNDS").ok().and_then(|n| n.parse().ok());
    rounds(&there, "cross", count.unwrap_or(if cfg!(windows) { 8 } else { 40 }), full());
}

/// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_mount_chaos -- --ignored --nocapture
#[test]
#[ignore = "Writes to the mounted filesystems named in FERROUS_FS_TARGETS"]
fn real_mount_chaos() {
    let targets = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
    let profile = if std::env::var_os("FERROUS_CHAOS_FULL").is_some() { full() } else { FAT };
    for target in std::env::split_paths(&targets) { rounds(&target, "real", 12, profile); }
}

/// Whatever the cause, a copy that differs from its source never gets the destination name,
/// on any removable drive. The local case is the control: there the check is not made.
/// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_mount_damaged -- --ignored
#[test]
#[ignore = "Writes to the mounted filesystems named in FERROUS_FS_TARGETS"]
fn real_mount_damaged_copy_is_never_published() {
    let targets = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
    for target in std::env::split_paths(&targets) { damaged_copy_is_refused(&target); }
}

#[cfg(windows)]
#[test]
fn damaged_copy_to_a_network_location_is_never_published() {
    let Some(there) = other_filesystem() else { eprintln!("Skipped: no second filesystem"); return };
    damaged_copy_is_refused(&there);
}

fn damaged_copy_is_refused(target: &Path) {
    let arena = Arena::new(target, "dmg", 1);
    let file = arena.src.join("data.bin");
    let data = fill(&mut Rng(7), 3 << 20);
    std::fs::write(&file, &data).unwrap();
    DAMAGE_COPY.set(Some(Box::new(|payload: &Path| {
        // Same length, zeros in the middle: what exFAT did.
        let mut copy = std::fs::read(payload).unwrap();
        copy[1 << 20..(1 << 20) + 32768].fill(0);
        std::fs::write(payload, copy).unwrap();
    })));
    let result = copy_item(&file, &arena.dst, &AtomicBool::new(false));
    assert!(DAMAGE_COPY.take().is_none(), "the check never ran");
    assert!(result.unwrap_err().contains("does not match"));
    assert!(!arena.dst.join("data.bin").exists());
    assert_eq!(std::fs::read(&file).unwrap(), data);
    no_stage(&arena.dst);
}

/// Jobs side by side on one drive, some cancelled. FERROUS_CHAOS_BLIND skips the app's
/// read-back, so a damaged copy would reach the check at the end of this test.
/// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_mount_parallel -- --ignored --nocapture
#[test]
#[ignore = "Writes to the mounted filesystems named in FERROUS_FS_TARGETS"]
fn real_mount_parallel_jobs() {
    let targets = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
    let blind = std::env::var_os("FERROUS_CHAOS_BLIND").is_some();
    for target in std::env::split_paths(&targets) {
        for seed in seeds(10) {
            let mut rng = Rng(seed);
            let arena = Arena::new(&target, "par", seed);
            let (src, dst) = (arena.src.clone(), arena.dst.clone());
            let mut trees = Vec::new();
            for k in 0..4 {
                let tree = src.join(format!("tree {k}"));
                std::fs::create_dir(&tree).unwrap();
                for f in 0..6 {
                    let size = 200_000 + rng.below(800_000) as usize;
                    let sub = tree.join(format!("sub {f}"));
                    std::fs::create_dir(&sub).unwrap();
                    std::fs::write(sub.join("data"), fill(&mut rng, size)).unwrap();
                }
                trees.push(tree);
            }
            let threads: Vec<_> = trees.iter().enumerate().map(|(k, tree)| {
                let (tree, dst, delay) = (tree.clone(), dst.clone(), 200 + rng.below(3000));
                std::thread::spawn(move || {
                    // Half are cancelled along the way, and free their folders.
                    let cancel = Arc::new(AtomicBool::new(false));
                    if k % 2 == 1 {
                        let flag = cancel.clone();
                        std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_micros(delay)); flag.store(true, Ordering::Relaxed); });
                    }
                    if blind { copy_checked(&tree, &dst, &cancel, |_| Ok(())) } else { copy_item(&tree, &dst, &cancel) }
                })
            }).collect();
            for (k, thread) in threads.into_iter().enumerate() {
                let result = thread.join().unwrap();
                if let Err(e) = &result { assert!(e.contains("Cancelled"), "seed {seed}: {e}"); }
                if result.is_ok() || k % 2 == 0 {
                    let copy = result.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
                    #[cfg(target_os = "linux")]
                    crate::linux_fs::sync(&dst).unwrap();
                    proven::verify(&trees[k], &copy, &AtomicBool::new(false))
                        .unwrap_or_else(|e| panic!("seed {seed}: job {k} reported success over: {e}"));
                }
            }
        }
    }
}

/// Run by `killed_at_any_moment`, in a process of its own.
#[test]
#[ignore = "Helper for killed_at_any_moment"]
fn victim_of_kill() {
    let (Ok(src), Ok(dst)) = (std::env::var("FERROUS_VICTIM_SRC"), std::env::var("FERROUS_VICTIM_DST")) else { return };
    let _ = move_item(Path::new(&src), Path::new(&dst), &AtomicBool::new(false));
}

#[test]
fn killed_at_any_moment() {
    let Some(there) = other_filesystem() else { eprintln!("Skipped: no second filesystem"); return };
    for seed in seeds(25) {
        let mut rng = Rng(seed);
        let arena = Arena::new(&there, "kill", seed);
        let tree = arena.src.join("tree");
        std::fs::create_dir(&tree).unwrap();
        for k in 0..60 {
            let size = 1000 + rng.below(300_000) as usize;
            std::fs::write(tree.join(format!("file {k}")), fill(&mut rng, size)).unwrap();
            if k % 10 == 0 { std::fs::create_dir(tree.join(format!("sub {k}"))).unwrap(); std::fs::write(tree.join(format!("sub {k}/inner")), fill(&mut rng, 5000)).unwrap(); }
        }
        let original = manifest(&tree);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ops::chaos::victim_of_kill", "--ignored", "--test-threads=1"])
            .env("FERROUS_VICTIM_SRC", &tree).env("FERROUS_VICTIM_DST", &arena.dst)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let delay = 2 + rng.below(90);
        std::thread::sleep(std::time::Duration::from_millis(delay));
        let _ = child.kill(); // SIGKILL or TerminateProcess: no cleanup of any kind runs.
        let _ = child.wait();
        // cp may outlive its parent. Let it finish, as it would for a real user.
        std::thread::sleep(std::time::Duration::from_millis(60));
        let name = tree.file_name().unwrap().to_os_string();
        nothing_lost(&original, &tree, &arena.dst, &name, &format!("killed after {delay} ms, seed {seed}"));
    }
}
