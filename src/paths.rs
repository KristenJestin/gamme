//! Where Gamme keeps what is its own, by the XDG rules: a relative or unset variable falls back
//! to its default.

use std::path::PathBuf;

fn folder(variable: &str, default: &str) -> Option<PathBuf> {
    let set = std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    set.or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(default)))
}

/// `$XDG_STATE_HOME/gamme`: what Gamme remembers between runs (the update channel, for now).
pub fn state() -> Option<PathBuf> {
    Some(folder("XDG_STATE_HOME", ".local/state")?.join("gamme"))
}

/// `$XDG_DATA_HOME/gamme/library`: the pieces the player imported, each in a folder of its own.
pub fn library() -> Option<PathBuf> {
    Some(data_home()?.join("gamme").join("library"))
}

/// `$XDG_CACHE_HOME/gamme`: what Gamme can make again (Verovio's resources).
pub fn cache() -> Option<PathBuf> {
    Some(folder("XDG_CACHE_HOME", ".cache")?.join("gamme"))
}

/// `$XDG_DATA_HOME/gamme`: what Gamme downloads and keeps (the SoundFonts, for now).
pub fn data() -> Option<PathBuf> {
    Some(data_home()?.join("gamme"))
}

/// `$XDG_DATA_HOME`: where the launcher entry and the icon go, beside other programs' own.
pub fn data_home() -> Option<PathBuf> {
    folder("XDG_DATA_HOME", ".local/share")
}
