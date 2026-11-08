//! Windows Shell file transfers. The receiving application performs the operation;
//! cancelling a drag or cutting to the clipboard never deletes source files.
use std::{ffi::{c_void, OsString}, os::windows::ffi::{OsStrExt, OsStringExt}, path::PathBuf, ptr};

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);
const DATA_OBJECT: Guid = Guid(0x0000010e, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const BHID_DATA_OBJECT: Guid = Guid(0xb8c0bd9f, 0xed24, 0x455c, [0x83, 0xe6, 0xd5, 0x39, 0x0c, 0x4f, 0xe8, 0xc4]);
type Obj = *mut *const Vtable;
#[repr(C)]
struct Vtable {
    query: usize, add_ref: usize,
    release: unsafe extern "system" fn(Obj) -> u32,
    get_data: unsafe extern "system" fn(Obj, *const Format, *mut Medium) -> i32,
    get_data_here: usize, query_data: usize, canonical: usize,
    set_data: unsafe extern "system" fn(Obj, *const Format, *const Medium, i32) -> i32,
}
#[repr(C)]
struct Format { id: u16, target: *mut c_void, aspect: u32, index: i32, medium: u32 }
#[repr(C)]
struct Medium { kind: u32, memory: *mut c_void, release: *mut c_void }
#[repr(C)]
struct ArrayVtable {
    query: usize, add_ref: usize,
    release: unsafe extern "system" fn(*mut *const ArrayVtable) -> u32,
    bind: unsafe extern "system" fn(*mut *const ArrayVtable, *mut c_void, *const Guid, *const Guid, *mut Obj) -> i32,
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHParseDisplayName(name: *const u16, bind: *mut c_void, pidl: *mut *mut c_void, attributes: u32, out: *mut u32) -> i32;
    fn SHCreateShellItemArrayFromIDLists(count: u32, pidls: *const *mut c_void, out: *mut *mut *const ArrayVtable) -> i32;
    fn SHDoDragDrop(window: *mut c_void, data: Obj, source: *mut c_void, allowed: u32, effect: *mut u32) -> i32;
    fn DragQueryFileW(drop: *mut c_void, index: u32, name: *mut u16, len: u32) -> u32;
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn OleInitialize(reserved: *mut c_void) -> i32;
    fn OleUninitialize();
    fn CoTaskMemFree(memory: *mut c_void);
    fn OleSetClipboard(data: Obj) -> i32;
    #[cfg(test)]
    fn OleGetClipboard(data: *mut Obj) -> i32;
    fn OleFlushClipboard() -> i32;
    #[cfg(test)]
    fn ReleaseStgMedium(medium: *mut Medium);
}
#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(window: *mut c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    fn GetAsyncKeyState(key: i32) -> i16;
    fn GetClipboardSequenceNumber() -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, size: usize) -> *mut c_void;
    fn GlobalLock(memory: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(memory: *mut c_void) -> i32;
    fn GlobalFree(memory: *mut c_void) -> *mut c_void;
    fn GlobalSize(memory: *mut c_void) -> usize;
}
fn checked(hr: i32) -> Result<(), String> {
    if hr >= 0 { Ok(()) } else { Err(format!("Windows file transfer failed (0x{:08X})", hr as u32)) }
}
struct Ole;
impl Ole {
    fn new() -> Result<Self, String> {
        checked(unsafe { OleInitialize(ptr::null_mut()) })?;
        Ok(Self)
    }
}
impl Drop for Ole { fn drop(&mut self) { unsafe { OleUninitialize() } } }
struct Pidl(*mut c_void);
impl Drop for Pidl { fn drop(&mut self) { unsafe { CoTaskMemFree(self.0) } } }
struct Data(Obj);
impl Drop for Data { fn drop(&mut self) { if !self.0.is_null() { unsafe { ((**self.0).release)(self.0); } } } }

fn data(paths: &[PathBuf]) -> Result<Data, String> {
    if paths.is_empty() { return Err("No files selected".into()); }
    let mut pidls = Vec::new();
    for path in paths {
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut pidl = ptr::null_mut();
        let hr = unsafe { SHParseDisplayName(name.as_ptr(), ptr::null_mut(), &mut pidl, 0, ptr::null_mut()) };
        let pidl = Pidl(pidl);
        checked(hr)?;
        pidls.push(pidl);
    }
    let pointers: Vec<_> = pidls.iter().map(|p| p.0).collect();
    let mut array = ptr::null_mut();
    // The Shell array accepts absolute PIDLs from different parents (tree selection).
    unsafe {
        checked(SHCreateShellItemArrayFromIDLists(pointers.len() as u32, pointers.as_ptr(), &mut array))?;
        let mut object = ptr::null_mut();
        let hr = ((**array).bind)(array, ptr::null_mut(), &BHID_DATA_OBJECT, &DATA_OBJECT, &mut object);
        ((**array).release)(array);
        checked(hr)?;
        Ok(Data(object))
    }
}
fn effect_format() -> u32 {
    unsafe { RegisterClipboardFormatW(crate::win::wide("Preferred DropEffect").as_ptr()) }
}
impl Data {
    fn set(&self, id: u32, bytes: &[u8]) -> Result<(), String> {
        if id == 0 { return Err("Could not register clipboard format".into()); }
        // SetData takes ownership only on success. The movable memory remains
        // valid after this function returns and is released by the Shell object.
        unsafe {
            let memory = GlobalAlloc(2, bytes.len());
            if memory.is_null() { return Err("Could not allocate clipboard data".into()); }
            let buffer = GlobalLock(memory);
            if buffer.is_null() { GlobalFree(memory); return Err("Could not lock clipboard data".into()); }
            ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast(), bytes.len());
            GlobalUnlock(memory);
            let format = Format { id: id as u16, target: ptr::null_mut(), aspect: 1, index: -1, medium: 1 };
            let medium = Medium { kind: 1, memory, release: ptr::null_mut() };
            let hr = ((**self.0).set_data)(self.0, &format, &medium, 1);
            if hr < 0 { GlobalFree(memory); }
            checked(hr)
        }
    }
}
pub fn write(paths: &[PathBuf], cut: bool) -> Result<(), String> {
    let _ole = Ole::new()?;
    let object = data(paths)?;
    object.set(effect_format(), &(if cut { 2u32 } else { 1u32 }).to_le_bytes())?;
    checked(unsafe { OleSetClipboard(object.0) })?;
    // Render formats now so Explorer can still paste after this app closes.
    checked(unsafe { OleFlushClipboard() })
}
pub fn drag(paths: &[PathBuf]) -> Result<bool, String> {
    let _ole = Ole::new()?;
    let object = data(paths)?;
    let mut effect = 0;
    let hr = unsafe { SHDoDragDrop(ptr::null_mut(), object.0, ptr::null_mut(), 1 | 2 | 4, &mut effect) };
    checked(hr)?;
    Ok(hr == 0x40100 && effect != 0)
}
pub fn has_files() -> bool { unsafe { IsClipboardFormatAvailable(15) != 0 } }
pub fn sequence() -> u32 { unsafe { GetClipboardSequenceNumber() } }
struct Clipboard;
impl Clipboard {
    fn open() -> Result<Self, String> {
        if unsafe { OpenClipboard(ptr::null_mut()) } == 0 {
            Err("Clipboard is busy; try again".into())
        } else { Ok(Self) }
    }
}
impl Drop for Clipboard { fn drop(&mut self) { unsafe { CloseClipboard(); } } }
pub fn read() -> Result<Option<(Vec<PathBuf>, bool)>, String> {
    if !has_files() { return Ok(None); }
    let _clipboard = Clipboard::open()?;
    unsafe {
        let drop = GetClipboardData(15);
        if drop.is_null() { return Err("Could not read clipboard files".into()); }
        let count = DragQueryFileW(drop, u32::MAX, ptr::null_mut(), 0);
        let mut paths = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(drop, i, ptr::null_mut(), 0);
            if len == 0 { continue; }
            let mut name = vec![0u16; len as usize + 1];
            if DragQueryFileW(drop, i, name.as_mut_ptr(), len + 1) != len {
                return Err("Could not read clipboard filename".into());
            }
            paths.push(PathBuf::from(OsString::from_wide(&name[..len as usize])));
        }
        let memory = GetClipboardData(effect_format());
        let mut cut = false;
        if !memory.is_null() && GlobalSize(memory) >= 4 {
            let p = GlobalLock(memory);
            if !p.is_null() { cut = ptr::read_unaligned(p.cast::<u32>()) & 2 != 0; GlobalUnlock(memory); }
        }
        Ok((!paths.is_empty()).then_some((paths, cut)))
    }
}
fn text() -> String {
    let Ok(_clipboard) = Clipboard::open() else { return String::new() };
    unsafe {
        let memory = GetClipboardData(13);
        if memory.is_null() { return String::new(); }
        let p = GlobalLock(memory).cast::<u16>();
        if p.is_null() { return String::new(); }
        let chars = std::slice::from_raw_parts(p, GlobalSize(memory) / 2);
        let end = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
        let text = String::from_utf16_lossy(&chars[..end]).replace("\r\n", "\n");
        GlobalUnlock(memory);
        text
    }
}
/// egui's default Ctrl+V only delivers text/image events, dropping file-only
/// clipboard shortcuts. Translate the keys ourselves, including for text fields.
/// One press is one operation: a held Ctrl+V must not paste the files repeatedly. Raw
/// input carries no repeat flag (egui derives it later), hence `held`. A repeat is
/// dropped, not passed on: egui never saw the press, so it would look like a new one.
/// Shift+Delete cuts only while typing. In the file list it deletes for good.
pub fn shortcuts(input: &mut egui::RawInput, held: &mut std::collections::HashSet<egui::Key>, typing: bool) {
    input.events.retain_mut(|event| {
        match event {
            egui::Event::WindowFocused(false) => held.clear(),
            egui::Event::Key { key, pressed: false, .. } => { held.remove(key); }
            _ => {}
        }
        let egui::Event::Key { key, pressed: true, modifiers: m, .. } = event else { return true };
        let first_press = held.insert(*key);
        let command = if m.command && !m.shift && !m.alt {
            match key { egui::Key::C => 1, egui::Key::X => 2, egui::Key::V => 3, _ => 0 }
        } else if m.shift && !m.ctrl && !m.alt {
            match key { egui::Key::Delete if typing => 2, egui::Key::Insert => 3, _ => 0 }
        } else { 0 };
        if command == 0 { return true; }
        if !first_press { return false; }
        *event = match command {
            1 => egui::Event::Copy,
            2 => egui::Event::Cut,
            _ => egui::Event::Paste(text()),
        };
        true
    });
}
pub fn left_button_down() -> bool { unsafe { GetAsyncKeyState(1) < 0 } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_selection_exposes_real_files_and_move_effect() {
        let _ole = Ole::new().unwrap();
        // The Shell reports long names; %TEMP% may hold an 8.3 alias of the profile folder.
        let root = crate::windows_fs::shown(&std::fs::canonicalize(std::env::temp_dir()).unwrap()).join(format!("fc_data_{}", std::process::id()));
        let folder = root.join("Folder Łódź");
        let nested = root.join("other");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("Zażółć 日本語.txt");
        std::fs::write(&file, b"untouched").unwrap();
        let paths = vec![folder.clone(), file.clone()];
        {
            let object = data(&paths).unwrap();
            for effect in [1u32, 2] {
                object.set(effect_format(), &effect.to_le_bytes()).unwrap();
                for id in [15, effect_format()] {
                    let format = Format { id: id as u16, target: ptr::null_mut(), aspect: 1, index: -1, medium: 1 };
                    let mut medium = Medium { kind: 0, memory: ptr::null_mut(), release: ptr::null_mut() };
                    unsafe {
                        checked(((**object.0).get_data)(object.0, &format, &mut medium)).unwrap();
                        assert_eq!(medium.kind, 1);
                        if id == 15 {
                            assert_eq!(DragQueryFileW(medium.memory, u32::MAX, ptr::null_mut(), 0), 2);
                            for (i, path) in paths.iter().enumerate() {
                                let len = DragQueryFileW(medium.memory, i as u32, ptr::null_mut(), 0);
                                let mut name = vec![0; len as usize + 1];
                                DragQueryFileW(medium.memory, i as u32, name.as_mut_ptr(), len + 1);
                                assert_eq!(PathBuf::from(OsString::from_wide(&name[..len as usize])), *path);
                            }
                        } else {
                            let p = GlobalLock(medium.memory);
                            assert!(!p.is_null());
                            assert_eq!(ptr::read_unaligned(p.cast::<u32>()), effect);
                            GlobalUnlock(medium.memory);
                        }
                        ReleaseStgMedium(&mut medium);
                    }
                }
            }
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"untouched");
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(folder).unwrap();
        std::fs::remove_dir(nested).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    #[ignore = "Uses the Windows clipboard; run explicitly with --ignored --test-threads=1"]
    fn shell_clipboard_interoperates_with_windows() {
        let _ole = Ole::new().unwrap();
        let mut previous = ptr::null_mut();
        checked(unsafe { OleGetClipboard(&mut previous) }).unwrap();
        struct Restore(Data);
        impl Drop for Restore {
            fn drop(&mut self) { unsafe { OleSetClipboard(self.0.0); OleFlushClipboard(); } }
        }
        let _restore = Restore(Data(previous));
        let root = crate::windows_fs::shown(&std::fs::canonicalize(std::env::temp_dir()).unwrap()).join(format!("fc_shell_{}", std::process::id()));
        let folder = root.join("Folder Łódź");
        let nested = root.join("other");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("Zażółć 日本語.txt");
        std::fs::write(&file, b"unchanged").unwrap();
        let paths = vec![folder.clone(), file.clone()];
        for cut in [false, true] {
            write(&paths, cut).unwrap();
            assert_eq!(read().unwrap(), Some((paths.clone(), cut)));
            // Independent Shell consumer: Windows PowerShell FileDropList.
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-STA", "-Command", "[Console]::OutputEncoding = [Text.UTF8Encoding]::new(); @(Get-Clipboard -Format FileDropList | ForEach-Object FullName) | ConvertTo-Json -Compress"])
                .output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let names: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(names.into_iter().map(PathBuf::from).collect::<Vec<_>>(), paths);
            assert!(folder.is_dir());
            assert_eq!(std::fs::read(&file).unwrap(), b"unchanged");
        }
        // Read files published by another application, including a folder.
        let list = paths.iter().map(|p| format!("[void]$p.Add('{}');", p.display().to_string().replace('\'', "''"))).collect::<String>();
        let script = format!("Add-Type -AssemblyName System.Windows.Forms; $p = New-Object System.Collections.Specialized.StringCollection; {list} [System.Windows.Forms.Clipboard]::SetFileDropList($p)");
        assert!(std::process::Command::new("powershell.exe").args(["-NoProfile", "-STA", "-Command", &script]).status().unwrap().success());
        assert_eq!(read().unwrap(), Some((paths.clone(), false)));
        // Plain text must replace the file clipboard, never reuse a stale cut.
        assert!(std::process::Command::new("powershell.exe").args(["-NoProfile", "-STA", "-Command", "Set-Clipboard -Value 'text only'"]).status().unwrap().success());
        assert_eq!(read().unwrap(), None);
        assert_eq!(text(), "text only");
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir(&folder).unwrap();
        std::fs::remove_dir(&nested).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn file_shortcuts_preserve_text_edit_events() {
        let mut input = egui::RawInput::default();
        for key in [egui::Key::C, egui::Key::X] {
            input.events.push(egui::Event::Key { key, physical_key: None,
                pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
        }
        shortcuts(&mut input, &mut Default::default(), false);
        assert!(matches!(input.events[0], egui::Event::Copy));
        assert!(matches!(input.events[1], egui::Event::Cut));
    }

    #[test]
    fn shift_delete_reaches_the_file_list_as_a_key() {
        let event = egui::Event::Key { key: egui::Key::Delete, physical_key: None,
            pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT };
        let mut input = egui::RawInput { events: vec![event], ..Default::default() };
        shortcuts(&mut input, &mut Default::default(), false);
        assert!(matches!(input.events[0], egui::Event::Key { key: egui::Key::Delete, .. }), "must not cut files");
    }

    #[test]
    fn held_shortcuts_fire_once_until_released_or_focus_is_lost() {
        let press = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let release = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers };
        let mut held = Default::default();
        let mut run = |events: Vec<egui::Event>| {
            let mut input = egui::RawInput { events, ..Default::default() };
            shortcuts(&mut input, &mut held, true);
            input.events
        };
        let cut = |events: &[egui::Event]| events.iter().filter(|e| matches!(e, egui::Event::Cut)).count();
        // Windows sends a press per autorepeat tick. Ctrl+X and Shift+Delete both mean Cut.
        for (key, modifiers) in [(egui::Key::X, egui::Modifiers::COMMAND), (egui::Key::Delete, egui::Modifiers::SHIFT)] {
            assert_eq!(cut(&run(vec![press(key, modifiers)])), 1);
            let repeats = run(vec![press(key, modifiers), press(key, modifiers)]);
            assert!(repeats.is_empty(), "must not reach egui as a fresh press: {repeats:?}");
            run(vec![release(key, modifiers)]);
            assert_eq!(cut(&run(vec![press(key, modifiers)])), 1, "released keys work again");
            // Alt+Tab can swallow the release.
            run(vec![egui::Event::WindowFocused(false)]);
            assert_eq!(cut(&run(vec![press(key, modifiers)])), 1, "focus loss forgets held keys");
            run(vec![release(key, modifiers)]);
        }
        let plain = run(vec![press(egui::Key::ArrowDown, egui::Modifiers::NONE), press(egui::Key::ArrowDown, egui::Modifiers::NONE)]);
        assert_eq!(plain.len(), 2, "navigation keeps its autorepeat");
    }
}
