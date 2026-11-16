//! The icons Gamme adds to the kit's: Lucide icons by their file names, embedded here, the kit's
//! own served after them.

use std::borrow::Cow;

use gpui_design_system::{Icon, Icons};
use gpui_kit::{AssetSource, Result, SharedString};

pub const MUSIC: Icon = Icon::Lucide("music");
pub const PAUSE: Icon = Icon::Lucide("pause");
pub const SKIP_BACK: Icon = Icon::Lucide("skip-back");
pub const SKIP_FORWARD: Icon = Icon::Lucide("skip-forward");

gpui_kit::assets::icon_assets!(Lucide, [Music, Pause, SkipBack, SkipForward]);

/// Gamme's icons, then the kit's.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match Lucide.load(path)? {
            Some(found) => Ok(Some(found)),
            None => Icons.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Icons.list(path)?;
        paths.extend(Lucide.list(path)?);
        Ok(paths)
    }
}
