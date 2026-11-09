//! Linux properties use lstat; editing pins the selected inode and never follows links.
use std::{fs, os::{fd::AsRawFd, unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}}, path::{Path, PathBuf}, sync::mpsc};
use egui::{Context, RichText};
use crate::chrome::{Icon, Panel};
use crate::fsx::Kind;
use crate::theme::Palette;

pub struct Properties {
    path: PathBuf,
    rx: Option<mpsc::Receiver<Result<Details, String>>>,
    details: Option<Details>,
    mode: String,
    uid: String,
    gid: String,
    message: String,
    applying: bool,
}
struct Details {
    md: fs::Metadata,
    kind: &'static str,
    link: Option<PathBuf>,
    advanced: String,
}
fn inspect(path: &Path) -> Result<Details, String> {
    use std::os::unix::fs::FileTypeExt;
    let md = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let t = md.file_type();
    let kind = if t.is_symlink() { "Symbolic link" } else if t.is_dir() { "Directory" } else if t.is_file() { "Regular file" }
        else if t.is_fifo() { "FIFO / named pipe" } else if t.is_socket() { "Unix socket" }
        else if t.is_block_device() { "Block device" } else if t.is_char_device() { "Character device" } else { "Special file" };
    let link = if t.is_symlink() { fs::read_link(path).ok() } else { None };
    let mut advanced = String::new();
    // No shell expansion; no dereferencing links. Missing optional utilities are explicit.
    for (tool, args) in [("getfacl", vec!["--physical", "--absolute-names", "--numeric", "--"]),
        ("getfattr", vec!["--no-dereference", "--absolute-names", "--dump", "--match=-", "--encoding=hex", "--"]),
        ("lsattr", vec!["-d", "--"])] {
        advanced.push_str(&format!("{tool}:\n"));
        if t.is_symlink() && tool != "getfattr" { advanced.push_str("Not followed (symbolic link).\n\n"); continue; }
        match std::process::Command::new("timeout").args(["3s", tool]).args(args).arg(path).output() {
            Ok(out) => {
                let bytes = [&out.stdout[..], &out.stderr[..]].concat();
                let text = String::from_utf8_lossy(&bytes[..bytes.len().min(32768)]);
                advanced.push_str(if text.is_empty() { "No entries reported." } else { &text });
                if !out.status.success() { advanced.push_str("\nUnavailable, unsupported, access denied or timed out."); }
            }
            Err(e) => advanced.push_str(&format!("Inspection utility unavailable: {e}")),
        }
        advanced.push_str("\n\n");
    }
    Ok(Details { md, kind, link, advanced })
}
fn apply(path: &Path, original: &fs::Metadata, mode: u32, uid: u32, gid: u32) -> Result<(), String> {
    if original.file_type().is_symlink() { return Err("Link permissions are not editable; open its target explicitly.".into()); }
    let file = fs::OpenOptions::new().read(true).custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path).map_err(|e| e.to_string())?;
    let current = file.metadata().map_err(|e| e.to_string())?;
    if current.ino() != original.ino() || current.dev() != original.dev() || current.mode() != original.mode() || current.uid() != original.uid() || current.gid() != original.gid() {
        return Err("The file changed. Close and reopen Properties before applying.".into());
    }
    // /proc/self/fd refers to our pinned inode, including for files with mode 0000.
    let pinned = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
    if uid != current.uid() || gid != current.gid() {
        std::os::unix::fs::chown(&pinned, Some(uid), Some(gid)).map_err(|e| format!("Owner/group unchanged: {e}"))?;
    }
    fs::set_permissions(&pinned, fs::Permissions::from_mode(mode)).map_err(|e| format!("Owner/group may have changed; applying permissions failed: {e}"))
}
impl Properties {
    pub fn new(path: PathBuf, ctx: Context) -> Self {
        let mut this = Self { path, rx: None, details: None, mode: String::new(), uid: String::new(), gid: String::new(), message: String::new(), applying: false };
        this.load(ctx); this
    }
    fn load(&mut self, ctx: Context) {
        let (tx, rx) = mpsc::channel(); self.rx = Some(rx);
        let path = self.path.clone();
        std::thread::spawn(move || { let _ = tx.send(inspect(&path)); ctx.request_repaint(); });
    }
    /// Returns (keep open, listing changed).
    pub fn show(&mut self, ctx: &Context, pal: Palette) -> (bool, bool) {
        let mut changed = false;
        if let Some(result) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.rx = None;
            match result {
                Ok(details) => {
                    self.mode = format!("{:04o}", details.md.mode() & 0o7777);
                    self.uid = details.md.uid().to_string(); self.gid = details.md.gid().to_string();
                    self.details = Some(details);
                    if self.applying { self.message = "Permissions and ownership updated.".into(); changed = true; }
                }
                Err(e) => { self.message = e; changed = self.applying; },
            }
            self.applying = false;
        }
        let name = crate::fsx::display_name(&self.path);
        let ext = self.path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let icon = if self.details.as_ref().is_some_and(|d| d.md.is_dir()) { Icon::Folder(Kind::Folder.color()) } else { Icon::Entry(Kind::from_ext(&ext), &ext) };
        let mut escape = false;
        let mut open = !Panel::new(&name, pal, icon).info("Properties")
            .window(|w| w.id(egui::Id::unique("linux-properties")))
            .fit_screen(ctx, egui::vec2(520.0, 620.0), egui::vec2(420.0, 260.0))
            .show(ctx, |ui| crate::chrome::scrolled_body(ui, false, |ui| {
            escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.label(self.path.display().to_string());
            if self.rx.is_some() { ui.spinner(); ui.label("Reading file information…"); }
            if let Some(d) = &self.details {
                egui::Grid::new("linux-file-details").num_columns(2).show(ui, |ui| {
                    for (key, value) in [
                        ("Type", d.kind.to_string()), ("Size", format!("{} bytes; {} bytes allocated", d.md.len(), d.md.blocks() * 512)),
                        ("Inode / device", format!("{} / {}", d.md.ino(), d.md.dev())), ("Hard links", d.md.nlink().to_string()),
                        ("Modified", crate::fsx::fmt_datetime(d.md.modified().ok())),
                        ("Accessed", crate::fsx::fmt_datetime(d.md.accessed().ok())),
                        ("Created", d.md.created().ok().map(|t| crate::fsx::fmt_datetime(Some(t))).unwrap_or_else(|| "Not reported by filesystem".into())),
                        ("Metadata changed", chrono::DateTime::from_timestamp(d.md.ctime(), d.md.ctime_nsec() as u32).map(|t| t.with_timezone(&chrono::Local).to_string()).unwrap_or_default()),
                    ] { ui.label(key); ui.label(value); ui.end_row(); }
                });
                if let Some(link) = &d.link {
                    ui.label(format!("Link target: {}", link.display()));
                    if !self.path.exists() { ui.colored_label(egui::Color32::YELLOW, "Broken or inaccessible target"); }
                }
                ui.separator();
                ui.label(RichText::new("Access permissions").strong());
                let editable = !d.md.file_type().is_symlink() && self.rx.is_none();
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal(|ui| { ui.label("Octal mode"); ui.add(egui::TextEdit::singleline(&mut self.mode).desired_width(65.0)); });
                    if let Ok(mut mode) = u32::from_str_radix(&self.mode, 8) {
                        let previous = mode;
                        egui::Grid::new("mode-bits").show(ui, |ui| {
                            ui.label(""); ui.label("Read"); ui.label("Write"); ui.label("Execute / enter"); ui.end_row();
                            for (name, shift) in [("Owner", 6), ("Group", 3), ("Others", 0)] {
                                ui.label(name);
                                for bit in [4, 2, 1] { let mask = bit << shift; let mut on = mode & mask != 0; ui.checkbox(&mut on, ""); if on { mode |= mask; } else { mode &= !mask; } }
                                ui.end_row();
                            }
                        });
                        ui.horizontal(|ui| {
                            for (name, mask) in [("Set UID", 0o4000), ("Set GID", 0o2000), ("Sticky", 0o1000)] {
                                let mut on = mode & mask != 0; ui.checkbox(&mut on, name); if on { mode |= mask; } else { mode &= !mask; }
                            }
                        });
                        if mode != previous { self.mode = format!("{mode:04o}"); }
                    }
                    ui.horizontal(|ui| {
                        ui.label("Owner UID"); ui.add(egui::TextEdit::singleline(&mut self.uid).desired_width(80.0));
                        ui.label("Group GID"); ui.add(egui::TextEdit::singleline(&mut self.gid).desired_width(80.0));
                    });
                    ui.label("Applies only to this item. The system enforces your permissions.");
                    ui.label("With an ACL, group mode bits represent its mask; inspect ACL below.");
                    let valid = self.mode.len() >= 3 && self.mode.len() <= 4 && u32::from_str_radix(&self.mode, 8).is_ok_and(|m| m <= 0o7777) && self.uid.parse::<u32>().is_ok_and(|n| n != u32::MAX) && self.gid.parse::<u32>().is_ok_and(|n| n != u32::MAX);
                    if ui.add_enabled(valid, egui::Button::new("Apply permissions and ownership")).clicked() {
                        let (mode, uid, gid) = (u32::from_str_radix(&self.mode, 8).unwrap(), self.uid.parse().unwrap(), self.gid.parse().unwrap());
                        let path = self.path.clone(); let md = d.md.clone(); let ctx = ctx.clone();
                        let (tx, rx) = mpsc::channel(); self.rx = Some(rx);
                        std::thread::spawn(move || {
                            let result = apply(&path, &md, mode, uid, gid).and_then(|()| inspect(&path));
                            let _ = tx.send(result); ctx.request_repaint();
                        });
                        self.message = "Applying changes…".into(); self.applying = true;
                    }
                });
                ui.collapsing("ACL, extended attributes and filesystem flags", |ui| {
                    ui.label("Read-only inspection. Requires acl, attr and e2fsprogs tools; support depends on the filesystem.");
                    egui::ScrollArea::both().max_height(170.0).show(ui, |ui| { ui.monospace(&d.advanced); });
                });
            }
            if !self.message.is_empty() { ui.label(&self.message); }
        }));
        if escape { open = false; }
        (open, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modes_owner_checks_and_symlink_safety() {
        let root = std::env::temp_dir().join(format!("fc_props_{}", std::process::id()));
        fs::create_dir_all(&root).unwrap(); let p = root.join("file"); fs::write(&p, "test").unwrap();
        let md = fs::symlink_metadata(&p).unwrap();
        apply(&p, &md, 0o2750, md.uid(), md.gid()).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o7777, 0o2750);
        assert!(apply(&p, &md, 0o777, md.uid(), md.gid()).is_err(), "stale edits must be refused");
        let link = root.join("link"); std::os::unix::fs::symlink(&p, &link).unwrap();
        let lm = fs::symlink_metadata(&link).unwrap();
        assert!(apply(&link, &lm, 0o777, md.uid(), md.gid()).is_err());
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o7777, 0o2750);
        fs::remove_dir_all(root).unwrap();
    }
}
