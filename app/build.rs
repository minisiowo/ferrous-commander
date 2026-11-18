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
    println!("cargo:rerun-if-env-changed=FERROUS_RELEASE_NOTES");
    let notes = std::env::var_os("FERROUS_RELEASE_NOTES").filter(|path| !path.is_empty()).map_or_else(String::new, |path| {
        let path = std::path::PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        std::fs::read_to_string(path).expect("read release notes")
    });
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    std::fs::write(out.join("release-notes.md"), notes).expect("embed release notes");

    // cfg(windows) describes the machine building this, which may be checking for Linux.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") { embed_windows_icon(); }
}
