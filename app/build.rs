/// Puts the icon inside the executable, so Explorer shows it without any extra file.
#[cfg(windows)]
fn embed_windows_icon() {
    let icon = "packaging/windows/ferrous-commander.ico";
    println!("cargo:rerun-if-changed={icon}");
    winresource::WindowsResource::new().set_icon(icon).compile().expect("embed Windows icon");
    // The DLLs this program links against come from System32 only, never from the folder
    // it runs in, which may be Downloads or a pendrive. main() does the same for later loads.
    println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
}

fn main() {
    // cfg(windows) describes the machine building this, which may be checking for Linux.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") { embed_windows_icon(); }
}
