//! Windows recommended association handlers. All COM calls stay in one STA helper.
use std::{ffi::c_void, path::Path, ptr};
use super::AppEntry;

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);
type Hr = i32;
#[repr(C)]
struct OpenAsInfo {
    file: *const u16,
    class: *const u16,
    flags: u32,
}
type Obj = *mut *const Vtable;
#[repr(C)]
struct Vtable {
    query: unsafe extern "system" fn(Obj, *const Guid, *mut *mut c_void) -> Hr,
    add_ref: unsafe extern "system" fn(Obj) -> u32,
    release: unsafe extern "system" fn(Obj) -> u32,
    name: unsafe extern "system" fn(Obj, *mut *mut u16) -> Hr,
    ui_name: unsafe extern "system" fn(Obj, *mut *mut u16) -> Hr,
    icon: usize,
    recommended: unsafe extern "system" fn(Obj) -> Hr,
    make_default: usize,
    invoke: unsafe extern "system" fn(Obj, *mut c_void) -> Hr,
}
#[repr(C)]
struct EnumVtable {
    query: usize, add_ref: usize,
    release: unsafe extern "system" fn(*mut *const EnumVtable) -> u32,
    next: unsafe extern "system" fn(*mut *const EnumVtable, u32, *mut Obj, *mut u32) -> Hr,
}
type Item = *mut *const ItemVtable;
#[repr(C)]
struct ItemVtable {
    query: usize, add_ref: usize,
    release: unsafe extern "system" fn(Item) -> u32,
    bind: unsafe extern "system" fn(Item, *mut c_void, *const Guid, *const Guid, *mut *mut c_void) -> Hr,
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHOpenWithDialog(parent: *mut c_void, info: *const OpenAsInfo) -> Hr;
    fn SHAssocEnumHandlers(ext: *const u16, filter: u32, out: *mut *mut *const EnumVtable) -> Hr;
    fn SHCreateItemFromParsingName(name: *const u16, bind: *mut c_void, iid: *const Guid, out: *mut Item) -> Hr;
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, mode: u32) -> Hr;
    fn CoUninitialize();
    fn CoTaskMemFree(p: *mut c_void);
}
struct Com;
impl Com {
    fn new() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(ptr::null_mut(), 2) };
        if hr >= 0 { Ok(Self) } else { Err(format!("Could not initialize Windows application handler (0x{:08X})", hr as u32)) }
    }
}
impl Drop for Com { fn drop(&mut self) { unsafe { CoUninitialize() } } }

pub fn choose_app(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    if !path.is_file() { return Err("The selected file no longer exists".into()); }
    let _com = Com::new()?;
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // OAIF_EXEC opens the chosen file; OAIF_HIDE_REGISTRATION avoids changing
    // defaults on older Windows. Modern Windows ignores registration flags.
    let info = OpenAsInfo { file: file.as_ptr(), class: ptr::null(), flags: 0x04 | 0x20 };
    let hr = unsafe { SHOpenWithDialog(ptr::null_mut(), &info) };
    // HRESULT_FROM_WIN32(ERROR_CANCELLED) is a normal user dismissal.
    if hr >= 0 || hr as u32 == 0x800704c7 { Ok(()) }
    else { Err(format!("Could not show the application chooser (0x{:08X})", hr as u32)) }
}
struct Handler(Obj);
impl Drop for Handler { fn drop(&mut self) { unsafe { ((**self.0).release)(self.0); } } }

fn handlers(path: &Path) -> Vec<Handler> {
    let Some(ext) = path.extension().filter(|s| !s.is_empty()) else { return Vec::new(); };
    let ext = crate::win::wide(&format!(".{}", ext.to_string_lossy()));
    let mut en = ptr::null_mut();
    let mut out = Vec::new();
    unsafe {
        // ASSOC_FILTER_RECOMMENDED, rather than every installed application.
        let hr = SHAssocEnumHandlers(ext.as_ptr(), 1, &mut en);
        if hr >= 0 && !en.is_null() {
            loop {
                let mut h = ptr::null_mut();
                let mut n = 0;
                let hr = ((**en).next)(en, 1, &mut h, &mut n);
                let handler = (!h.is_null()).then(|| Handler(h));
                if hr != 0 || n == 0 { break; }
                if let Some(h) = handler
                    && ((**h.0).recommended)(h.0) == 0 { out.push(h); }
            }
            ((**en).release)(en);
        }
    }
    out
}
fn name(h: &Handler, display: bool) -> String {
    unsafe {
        let mut p = ptr::null_mut();
        let get = if display { (**h.0).ui_name } else { (**h.0).name };
        let hr = get(h.0, &mut p);
        if p.is_null() { return String::new(); }
        let mut n = 0;
        if hr >= 0 { while *p.add(n) != 0 { n += 1; } }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        CoTaskMemFree(p.cast());
        s
    }
}
pub fn apps_for(path: &Path) -> Vec<AppEntry> {
    let Ok(_com) = Com::new() else { return Vec::new(); };
    let mut apps: Vec<_> = handlers(path).iter().map(|h| AppEntry { name: name(h, true), exec: name(h, false) })
        .filter(|a| !a.name.is_empty() && !a.exec.is_empty()).collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| seen.insert(a.exec.to_lowercase()));
    apps
}
pub fn launch(path: &Path, app: &AppEntry) -> Result<(), String> {
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    if !path.is_file() { return Err("The selected file no longer exists".into()); }
    let _com = Com::new()?;
    let h = handlers(&path).into_iter().find(|h| name(h, false) == app.exec)
        .ok_or_else(|| "Application is no longer available. Choose another app.".to_string())?;
    // Traditional applications accept the file as one argument. Avoid loading
    // their in-process shell extensions or waiting for Explorer's DDE handler.
    // Packaged application identities still need the Shell invocation below.
    let executable = Path::new(&app.exec);
    if executable.is_file() && executable.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
        use std::{os::windows::process::CommandExt, process::{Command, Stdio}};
        return Command::new(executable).arg(&path).creation_flags(0x08000000)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map(|_| ()).map_err(|e| format!("Could not open with {}: {e}", app.name));
    }
    let data = shell_data(&path)?;
    let hr = unsafe { ((**h.0).invoke)(h.0, data.0.cast()) };
    if hr >= 0 { Ok(()) } else { Err(format!("Could not open with {} (0x{:08X})", app.name, hr as u32)) }
}

/// Owns IDataObject; only the IUnknown prefix is used to release it.
fn shell_data(path: &Path) -> Result<Handler, String> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let iid = Guid(0x0000010e, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    unsafe {
        let item_iid = Guid(0x43826d1e, 0xe718, 0x42ee, [0xbc, 0x55, 0xa1, 0xe2, 0x61, 0xc3, 0x7b, 0xfe]);
        let bhid = Guid(0xb8c0bd9f, 0xed24, 0x455c, [0x83, 0xe6, 0xd5, 0x39, 0x0c, 0x4f, 0xe8, 0xc4]); // BHID_DataObject
        let mut item = ptr::null_mut();
        let mut hr = SHCreateItemFromParsingName(path.as_ptr(), ptr::null_mut(), &item_iid, &mut item);
        let mut data = ptr::null_mut();
        if hr >= 0 && !item.is_null() {
            // Let the Shell construct the correct file selection. An absolute PIDL
            // is not a child of a null parent (the old SHCreateDataObject call).
            hr = ((**item).bind)(item, ptr::null_mut(), &bhid, &iid, &mut data);
            ((**item).release)(item);
        }
        if hr >= 0 && !data.is_null() { Ok(Handler(data.cast())) }
        else { Err(format!("Could not prepare the selected file (0x{:08X})", hr as u32)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_only_returns_recommended_named_handlers() {
        let apps = apps_for(Path::new("example.txt"));
        assert!(apps.iter().all(|a| !a.name.is_empty() && !a.exec.is_empty()));
        let _com = Com::new().unwrap();
        for h in handlers(Path::new("example.txt")) {
            assert_eq!(unsafe { ((**h.0).recommended)(h.0) }, 0);
        }
    }

    #[test]
    fn shell_data_handles_unicode_spaces_and_repeated_release() {
        let _com = Com::new().unwrap();
        let dir = std::env::temp_dir().join(format!("fc_openwith_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Zażółć gęślą test.txt");
        std::fs::write(&path, "Open with regression").unwrap();
        for _ in 0..16 {
            let data = shell_data(&path).expect("Shell must bind a real file to IDataObject");
            let mut checked = ptr::null_mut();
            let iid = Guid(0x0000010e, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
            assert_eq!(unsafe { ((**data.0).query)(data.0, &iid, &mut checked) }, 0);
            assert!(!checked.is_null());
            drop(Handler(checked.cast()));
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn missing_files_and_extensionless_types_are_safe() {
        assert!(choose_app(Path::new("not-existing-openwith-file.txt")).unwrap_err().contains("no longer exists"));
        assert!(apps_for(Path::new("no-extension")).is_empty());
        assert!(launch(Path::new("not-existing-openwith-file.txt"), &AppEntry {
            name: "missing".into(), exec: "missing".into(),
        }).unwrap_err().contains("no longer exists"));
    }
}
