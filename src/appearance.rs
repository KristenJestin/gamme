//! Where the window's colours come from, and the control that chooses: the kit's own mechanism
//! (`gpui_design_system::appearance`), as the other applications on the kit use it. The colours
//! are the desktop's Omarchy theme, or Gamme's own, Nocturne, light or dark. Gamme keeps the choice
//! in its state folder, and uses it again at the next start.
//!
//! The control is on the settings page (`settings.rs`). The choice stays here, in the state
//! folder, not with the settings in the config folder: it is the kit's appearance state, kept
//! where the other applications on the kit keep theirs.

use std::path::{Path, PathBuf};

use gpui_design_system::appearance::{self, Appearance, Source};
use gpui_design_system::theme::Shade as KitShade;
use gpui_design_system::{Segmented, palette, presence, space};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, IntoElement, ParentElement, Styled, div};
use serde::{Deserialize, Serialize};

use crate::fonts::Pixel as _;
use crate::{config, paths, theme};

/// The id of the kit's Omarchy desktop source.
const OMARCHY: &str = "omarchy";

/// Where the colours come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Colours {
    /// The Omarchy theme of the desktop, followed as it changes. Off Omarchy, or when its theme
    /// cannot be read, the kit shows Gamme's own instead.
    Omarchy,
    /// Gamme's own, Nocturne: what is shown until the user chooses Omarchy's.
    #[default]
    Gamme,
}

/// Gamme's shade: the desktop's theme has its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shade {
    Light,
    #[default]
    Dark,
}

/// The choice, as it is kept: `appearance.json` in the state folder. Absent, or a key missing:
/// Gamme's own colours, and the dark shade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Kept {
    pub colours: Colours,
    pub shade: Shade,
}

impl Kept {
    /// The kit's way of saying it.
    pub fn appearance(self) -> Appearance {
        Appearance {
            source: match self.colours {
                Colours::Omarchy => Source::Desktop(OMARCHY.into()),
                Colours::Gamme => Source::Theme(theme::ID.into()),
            },
            shade: match self.shade {
                Shade::Light => KitShade::Light,
                Shade::Dark => KitShade::Dark,
            },
        }
    }

    /// What the kit shows, as a choice.
    fn of(shown: &Appearance) -> Self {
        Self {
            colours: match shown.source {
                Source::Desktop(_) => Colours::Omarchy,
                Source::Theme(_) => Colours::Gamme,
            },
            shade: match shown.shade {
                KitShade::Light => Shade::Light,
                KitShade::Dark => Shade::Dark,
            },
        }
    }
}

fn file(state: &Path) -> PathBuf {
    state.join("appearance.json")
}

/// The choice kept: the default when there is no file, and why when there is one that cannot be
/// read.
pub fn read(state: &Path) -> Result<Kept, String> {
    let bytes = match std::fs::read(file(state)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Kept::default()),
        Err(error) => return Err(format!("appearance.json: {error}")),
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("appearance.json: {error}"))
}

/// Keeps `kept`: the folder `0700`, the file `0600`, written beside and renamed over the old one,
/// so that it is the old choice or the new one, never half of one.
pub fn save(state: &Path, kept: Kept) -> std::io::Result<()> {
    config::write_private(state, "appearance.json", &serde_json::to_vec(&kept)?)
}

/// Uses the choice kept, once the kit is started. A file that cannot be read is said, and the
/// default is used.
pub fn restore(cx: &mut App) {
    let Some(state) = paths::state() else {
        return;
    };
    match read(&state) {
        Ok(kept) => appearance::appear(kept.appearance(), cx),
        Err(why) => {
            eprintln!("gamme: the kept appearance cannot be read, the default is used: {why}")
        }
    }
}

/// Uses `kept` at once, and keeps it off the main thread.
fn choose(kept: Kept, cx: &mut App) {
    appearance::appear(kept.appearance(), cx);
    let Some(state) = paths::state() else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = save(&state, kept) {
                eprintln!("gamme: the appearance was not kept: {error}");
            }
        })
        .detach();
}

/// The control: Omarchy or Gamme, and for Gamme, light or dark. It reads what the kit shows on
/// every frame, so a choice made elsewhere shows here too.
pub fn control(cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let shown = Kept::of(&appearance::shown(cx));
    let refused = appearance::refused(OMARCHY, cx);
    let row = |label: &'static str, control: Segmented| {
        div()
            .flex()
            .items_center()
            .gap(space::MD)
            .child(div().pixel_text().child(label))
            .child(control)
    };
    let colours = Segmented::new(
        "appearance-colours",
        "Colours",
        [("omarchy", "Omarchy"), ("gamme", "Gamme")],
        match shown.colours {
            Colours::Omarchy => 0,
            Colours::Gamme => 1,
        },
    )
    .when(refused.is_some(), |colours| colours.unavailable(0))
    .on_choose(move |index, _, cx| {
        let colours = if index == 0 {
            Colours::Omarchy
        } else {
            Colours::Gamme
        };
        choose(Kept { colours, ..shown }, cx);
    });
    let shade = Segmented::new(
        "appearance-shade",
        "Shade",
        [("light", "Light"), ("dark", "Dark")],
        match shown.shade {
            Shade::Light => 0,
            Shade::Dark => 1,
        },
    )
    .on_choose(move |index, _, cx| {
        let shade = if index == 0 {
            Shade::Light
        } else {
            Shade::Dark
        };
        choose(
            Kept {
                colours: Colours::Gamme,
                shade,
            },
            cx,
        );
    });
    let hint =
        refused.unwrap_or_else(|| "Omarchy follows your desktop's theme as it changes.".into());
    div()
        .flex()
        .flex_col()
        .gap(space::SM)
        .child(row("Colours", colours))
        .child(div().pixel_text().text_color(p.faint).child(hint))
        .child(
            presence(
                "appearance-shade-block",
                shown.colours == Colours::Gamme,
                row("Shade", shade),
            )
            .gap(space::SM),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own in the temporary one, removed on drop: never the real state.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("gamme-appearance-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn nothing_kept_is_gamme_and_dark() {
        let state = Temp::new("default");
        let kept = read(&state.0).unwrap();
        assert_eq!(kept, Kept::default());
        assert_eq!(kept.colours, Colours::Gamme);
        assert_eq!(kept.shade, Shade::Dark);
        assert_eq!(
            kept.appearance(),
            Appearance {
                source: Source::Theme(theme::ID.into()),
                shade: KitShade::Dark
            }
        );
    }

    #[test]
    fn every_choice_is_kept_and_read_again() {
        let state = Temp::new("roundtrip");
        for colours in [Colours::Omarchy, Colours::Gamme] {
            for shade in [Shade::Light, Shade::Dark] {
                let kept = Kept { colours, shade };
                save(&state.0, kept).unwrap();
                assert_eq!(read(&state.0).unwrap(), kept);
                assert_eq!(Kept::of(&kept.appearance()), kept);
            }
        }
    }

    #[test]
    fn a_new_choice_replaces_the_last() {
        let state = Temp::new("replace");
        save(
            &state.0,
            Kept {
                colours: Colours::Omarchy,
                shade: Shade::Dark,
            },
        )
        .unwrap();
        let gamme = Kept {
            colours: Colours::Gamme,
            shade: Shade::Light,
        };
        save(&state.0, gamme).unwrap();
        assert_eq!(read(&state.0).unwrap(), gamme);
        let leftovers: Vec<_> = std::fs::read_dir(&state.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["appearance.json"], "no temporary file is left");
    }

    #[test]
    fn the_folder_is_0700_and_the_file_0600() {
        let state = Temp::new("modes");
        save(&state.0, Kept::default()).unwrap();
        assert_eq!(mode(&state.0), 0o700);
        assert_eq!(mode(&file(&state.0)), 0o600);
    }

    #[test]
    fn the_kit_is_told_gamme_by_its_theme() {
        let kept = Kept {
            colours: Colours::Gamme,
            shade: Shade::Light,
        };
        assert_eq!(
            kept.appearance(),
            Appearance {
                source: Source::Theme(theme::ID.into()),
                shade: KitShade::Light
            }
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_is_a_reason_and_a_missing_key_is_its_default() {
        let state = Temp::new("unreadable");
        std::fs::create_dir_all(&state.0).unwrap();
        for text in ["not json", r#"{"colours":"neon"}"#, r#"{"colour":"gamme"}"#] {
            std::fs::write(file(&state.0), text).unwrap();
            let why = read(&state.0).unwrap_err();
            assert!(why.starts_with("appearance.json: "), "{text}: {why}");
        }
        // A key missing is that key's default: the colours here, the shade there.
        std::fs::write(file(&state.0), r#"{"colours":"omarchy"}"#).unwrap();
        assert_eq!(
            read(&state.0).unwrap(),
            Kept {
                colours: Colours::Omarchy,
                shade: Shade::Dark
            }
        );
        std::fs::write(file(&state.0), r#"{"shade":"light"}"#).unwrap();
        assert_eq!(
            read(&state.0).unwrap(),
            Kept {
                colours: Colours::Gamme,
                shade: Shade::Light
            }
        );
    }
}
