//! The icons, in pixel art (#29): grids of 7 x 7 cells drawn by `tools/icons.py`, a cell 2 pixels
//! at the kit's 14. Gamme's own, and, under the same names, the kit's that its screens show
//! (Lucide's: plus, the settings' gear, a select's chevron, a menu's "⋯", a toast's check...):
//! they are served before the kit's, so the kit's components draw them too. The kit's other
//! icons are served after them, as the kit draws them.

use std::borrow::Cow;

use gpui_design_system::{Icon, Icons};
use gpui_kit::{AssetSource, Result, SharedString};

pub const MUSIC: Icon = Icon::Lucide("music");
pub const PAUSE: Icon = Icon::Lucide("pause");
pub const SKIP_BACK: Icon = Icon::Lucide("skip-back");
pub const SKIP_FORWARD: Icon = Icon::Lucide("skip-forward");
pub const STAR: Icon = Icon::Lucide("star");
pub const STAR_FILL: Icon = Icon::Lucide("star-fill");
pub const TROPHY: Icon = Icon::Lucide("trophy");
pub const TARGET: Icon = Icon::Lucide("target");
pub const FLAME: Icon = Icon::Lucide("flame");
pub const REPEAT: Icon = Icon::Lucide("repeat");

macro_rules! pixel {
    ($($name:literal),* $(,)?) => {
        [$((
            concat!("icons/", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/", $name, ".svg")).as_slice(),
        )),*]
    };
}

/// Each pixel icon: its path in the assets, and its file.
const PIXEL: [(&str, &[u8]); 25] = pixel!(
    // The kit's, in place of Lucide's.
    "plus",
    "minus",
    "settings",
    "ellipsis",
    "check",
    "x",
    "chevron-down",
    "chevron-up",
    "chevron-left",
    "chevron-right",
    "chevrons-up-down",
    "play",
    "triangle-alert",
    "trash",
    "rotate-cw",
    // Gamme's own.
    "music",
    "pause",
    "skip-back",
    "skip-forward",
    "star",
    "star-fill",
    "trophy",
    "target",
    "flame",
    "repeat",
);

/// Gamme's pixel icons, then the kit's.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match PIXEL.iter().find(|(at, _)| *at == path) {
            Some((_, file)) => Ok(Some(Cow::Borrowed(*file))),
            None => Icons.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Icons.list(path)?;
        for (at, _) in PIXEL {
            if at.starts_with(path) && !paths.iter().any(|listed| listed.as_ref() == at) {
                paths.push(at.into());
            }
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served(icon: Icon) -> String {
        let path = icon.path();
        let file = Assets
            .load(&path)
            .unwrap()
            .unwrap_or_else(|| panic!("{path}"));
        String::from_utf8(file.into_owned()).unwrap()
    }

    #[test]
    fn every_icon_of_gamme_is_a_pixel_one() {
        for icon in [
            MUSIC,
            PAUSE,
            SKIP_BACK,
            SKIP_FORWARD,
            STAR,
            STAR_FILL,
            TROPHY,
            TARGET,
            FLAME,
            REPEAT,
        ] {
            assert!(served(icon).contains("viewBox=\"0 0 7 7\""), "{icon:?}");
        }
    }

    #[test]
    fn the_kit_s_icons_its_screens_show_are_pixel_ones_by_the_kit_s_own_paths() {
        for icon in [
            Icon::Plus,
            Icon::Minus,
            Icon::Settings,
            Icon::More,
            Icon::Check,
            Icon::Close,
            Icon::Chevron,
            Icon::ChevronUp,
            Icon::Previous,
            Icon::Next,
            Icon::Choose,
            Icon::Play,
            Icon::Warning,
            Icon::Trash,
            Icon::Restart,
        ] {
            assert!(served(icon).contains("crispEdges"), "{icon:?}");
        }
        // The kit's other icons are still there, its own.
        assert!(served(Icon::Folder).contains("stroke"));
    }

    #[test]
    fn every_pixel_icon_is_listed_once() {
        let listed = Assets.list("").unwrap();
        for (at, _) in PIXEL {
            let times = listed.iter().filter(|path| path.as_ref() == at).count();
            assert_eq!(times, 1, "{at}");
        }
    }
}
