//! Startup announcements for builds that ship with release notes.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
pub const URL: &str = concat!(
    "https://github.com/minisiowo/ferrous-commander/releases/tag/v",
    env!("CARGO_PKG_VERSION")
);
pub const NOTES: &str = include_str!(concat!(env!("OUT_DIR"), "/release-notes.md"));

pub fn pending() -> Option<PathBuf> {
    unread_marker(&crate::settings::config_directory()?, VERSION, NOTES)
}

fn unread_marker(config: &Path, version: &str, notes: &str) -> Option<PathBuf> {
    if notes.trim().is_empty() {
        return None;
    }
    let marker = config.join("seen-releases").join(version);
    (!marker.is_file()).then_some(marker)
}

pub fn mark_seen(marker: &Path) -> io::Result<()> {
    fs::create_dir_all(marker.parent().expect("release marker directory"))?;
    // Creating the file atomically also lets two app instances dismiss the same release.
    match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && marker.is_file() => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[test]
fn acknowledging_a_release_only_hides_that_version() {
    let config = std::env::temp_dir().join(format!("fc_release_notes_{}", std::process::id()));
    let notes = "- A new feature";
    assert_eq!(unread_marker(&config, "v1.0.0", ""), None);
    let marker = unread_marker(&config, "v1.0.0", notes).unwrap();
    mark_seen(&marker).unwrap();
    mark_seen(&marker).unwrap();
    assert_eq!(unread_marker(&config, "v1.0.0", notes), None);
    assert!(unread_marker(&config, "v1.1.0", notes).is_some());
    fs::remove_file(marker).unwrap();
    fs::remove_dir(config.join("seen-releases")).unwrap();
    fs::remove_dir(config).unwrap();
}
