//! Gamme's own faces, pixel fonts (README, "Brand"): Jersey 10 for the titles and the buttons,
//! Gamme Dot (DotGothic16 with its "ç" put back on the baseline, `tools/fonts.py`) for the text.
//! Both are bundled, under the SIL Open Font License (the licences are beside the files).
//!
//! A pixel font stays sharp only at its own size or a whole multiple of it, where each of its
//! pixels covers whole pixels of the screen: the sizes below are those.

use std::borrow::Cow;

use gpui_kit::App;

// The names and sizes wait for the pixel components that set them (#29: the kit's components
// cannot take another face yet).
/// The titles and the buttons.
#[allow(dead_code)]
pub const TITLE: &str = "Jersey 10";
/// The interface's text.
#[allow(dead_code)]
pub const TEXT: &str = "Gamme Dot";

/// Jersey 10 draws on 10 pixels to the em.
#[allow(dead_code)]
pub const TITLE_GRID: f32 = 10.;
/// Gamme Dot draws on 16 pixels to the em.
#[allow(dead_code)]
pub const TEXT_GRID: f32 = 16.;

const FILES: [&[u8]; 2] = [
    include_bytes!("../assets/fonts/Jersey10-Regular.ttf"),
    include_bytes!("../assets/fonts/GammeDot-Regular.ttf"),
];

/// Hands the faces to the window's text system, once at start-up.
pub fn register(cx: &mut App) {
    let fonts = FILES.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        eprintln!("gamme: the bundled fonts could not be loaded: {error}");
    }
}

/// `size` in pixels, at the nearest whole multiple of `grid` (one multiple at least).
#[allow(dead_code)]
pub fn snap(size: f32, grid: f32) -> f32 {
    (size / grid).round().max(1.) * grid
}

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
    fn a_size_is_a_whole_multiple_of_the_grid() {
        assert_eq!(snap(16., TEXT_GRID), 16.);
        assert_eq!(snap(13.5, TEXT_GRID), 16.);
        assert_eq!(snap(26., TITLE_GRID), 30.);
        assert_eq!(snap(2., TITLE_GRID), 10.);
    }
}
