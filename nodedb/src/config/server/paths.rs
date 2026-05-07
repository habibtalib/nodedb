// SPDX-License-Identifier: BUSL-1.1

//! Platform-aware default paths used by the server configuration.
//!
//! These resolve the user-data directory and home directory according to
//! standard OS conventions. Used as `#[serde(default = "...")]` callbacks
//! for [`super::section::ServerSection::data_dir`].

use std::path::PathBuf;

/// Default data directory following platform conventions.
///
/// - Linux: `$XDG_DATA_HOME/nodedb` or `~/.local/share/nodedb`
/// - macOS: `~/Library/Application Support/nodedb`
/// - Windows: `%LOCALAPPDATA%\nodedb\data`
///
/// Falls back to `./nodedb-data` if the home directory cannot be determined.
pub(super) fn default_data_dir() -> PathBuf {
    if let Some(dir) = platform_data_dir() {
        dir.join("nodedb")
    } else {
        PathBuf::from("nodedb-data")
    }
}

fn platform_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg));
        }
        home_dir().map(|h| h.join(".local").join("share"))
    }

    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library").join("Application Support"))
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA")
            && !local.is_empty()
        {
            return Some(PathBuf::from(local));
        }
        home_dir().map(|h| h.join("AppData").join("Local"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        home_dir().map(|h| h.join(".local").join("share"))
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}
