//! Gamme's own faces, pixel fonts (README, "Brand"): Jersey 10 for the titles and the controls,
//! Gamme Dot (DotGothic16 with its "ç" put back on the baseline, `tools/fonts.py`) for the text.
//! Both are bundled, under the SIL Open Font License (the licences are beside the files); the kit
//! draws its components in them (`look.rs`).
//!
//! A pixel font stays sharp only at its own size or a whole multiple of it, where each of its
//! pixels covers whole pixels of the screen: the sizes Gamme sets are those ([`Pixel`]).

use std::borrow::Cow;

use gpui_design_system::{Type as _, typography};
use gpui_kit::{App, Styled, px};

/// The titles, the controls, and the judgements' words over the keys (`falling.rs`).
pub const TITLE: &str = "Jersey 10";
/// The interface's text.
pub const TEXT: &str = "Gamme Dot";

/// Jersey 10 draws on 10 pixels to the em.
pub const TITLE_GRID: f32 = 10.;
/// Gamme Dot draws on 16 pixels to the em.
pub const TEXT_GRID: f32 = 16.;

const FILES: [&[u8]; 2] = [
    include_bytes!("../assets/fonts/Jersey10-Regular.ttf"),
    include_bytes!("../assets/fonts/GammeDot-Regular.ttf"),
];

/// Hands the faces to the window's text system, once at start-up, before the look names them.
pub fn register(cx: &mut App) {
    typography::add_fonts(FILES.iter().map(|bytes| Cow::Borrowed(*bytes)), cx);
}

/// The pixel faces at their own sizes, for the texts Gamme sets itself (the kit sets its own).
pub trait Pixel: Styled + Sized {
    /// The interface's face (Gamme Dot) at its own size, on lines of 24 pixels.
    fn pixel_text(self) -> Self {
        self.ui_text()
            .text_size(px(TEXT_GRID))
            .line_height(px(TEXT_GRID * 1.5))
    }

    /// The title face (Jersey 10) at `times` its own size, on lines a fifth taller.
    fn pixel_title(self, times: f32) -> Self {
        let size = TITLE_GRID * times.round().max(1.);
        self.ui_text()
            .title_text()
            .text_size(px(size))
            .line_height(px((size * 1.2).round()))
    }
}

impl<T: Styled> Pixel for T {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The family a font file names, as its name table holds it for Windows (UTF-16, big end
    /// first): what the text system matches `TITLE` and `TEXT` against.
    fn names(bytes: &[u8], family: &str) -> bool {
        let wide: Vec<u8> = family.encode_utf16().flat_map(u16::to_be_bytes).collect();
        bytes.windows(wide.len()).any(|window| window == wide)
    }

    #[test]
    fn each_file_is_the_family_its_name_says() {
        assert!(names(FILES[0], TITLE));
        assert!(names(FILES[1], TEXT));
    }

    #[test]
    fn gamme_s_texts_are_whole_multiples_of_their_face_s_grid() {
        let size = |mut element: gpui_kit::Div| match element.style().text.font_size {
            Some(gpui_kit::AbsoluteLength::Pixels(size)) => f32::from(size),
            other => panic!("{other:?}"),
        };
        assert_eq!(size(gpui_kit::div().pixel_text()), TEXT_GRID);
        // Rounded to whole times its grid.
        assert_eq!(size(gpui_kit::div().pixel_title(2.4)), 2. * TITLE_GRID);
    }
}
