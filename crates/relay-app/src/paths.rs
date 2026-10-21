//! Where Relay keeps its files.

use std::path::PathBuf;

/// The directory holding the database and spooled response bodies.
///
/// Follows each platform's convention — `~/.local/share/relay` on Linux,
/// `~/Library/Application Support/relay` on macOS, `%APPDATA%\relay` on
/// Windows — so that backup tools and the OS treat it correctly.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("relay")
}

/// The local database file.
pub fn database_path() -> PathBuf {
    data_dir().join("relay.db")
}

/// Where oversized response bodies are spooled.
///
/// Inside the data directory rather than the system temp directory so that a
/// multi-gigabyte download cannot fill a small `/tmp` partition, and so the
/// files are cleaned up with the rest of Relay's data.
pub fn spool_dir() -> PathBuf {
    data_dir().join("responses")
}

/// Removes spooled response bodies left behind by a previous run.
pub fn clean_spool() -> std::io::Result<()> {
    let dir = spool_dir();
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "body") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_nested_under_the_data_directory() {
        assert!(database_path().starts_with(data_dir()));
        assert!(spool_dir().starts_with(data_dir()));
    }

    #[test]
    fn the_database_is_named_predictably() {
        assert_eq!(
            database_path().file_name().unwrap().to_string_lossy(),
            "relay.db"
        );
    }
}
