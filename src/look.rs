//! Gamme's look on the kit (#29, the owner's "C2"): pixel art. Every component of the design
//! system is drawn in it: the pixel fonts (`fonts.rs`), corners cut by one cell, lines a cell
//! wide, a hard shadow a cell under the controls and the overlays (a pressed button moves onto
//! it), and every size on the grid of [`CELL`]s. The falling notes are drawn on the same grid
//! (`falling.rs`). The colours stay the theme's (`theme.rs`, or Omarchy's).

use gpui_design_system::look::Role;
use gpui_design_system::{Corners, Fonts, Look, Shadow};
use gpui_kit::px;

use crate::fonts;

/// One pixel of the art, in pixels of the window.
pub const CELL: f32 = 4.;

/// The look the kit draws Gamme in.
pub fn gamme() -> Look {
    let cell = px(CELL);
    Look {
        fonts: Fonts {
            ui: fonts::TEXT.into(),
            title: Some(fonts::TITLE.into()),
            control: Some(fonts::TITLE.into()),
        },
        corners: Corners::Notch {
            step: cell,
            steps: 1,
        },
        border: cell,
        shadow: Some(Shadow {
            x: px(0.),
            y: cell,
            color: Role::Shadow,
        }),
        grid: Some(cell),
        ..Look::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_of_the_look_is_whole_cells() {
        let look = gamme().resolved();
        let sizes = look.sizes;
        for size in [
            sizes.button,
            sizes.button_padding,
            sizes.icon_button,
            sizes.field,
            sizes.field_padding,
            sizes.pill,
            sizes.segmented,
            sizes.segment_padding,
            sizes.tab,
            sizes.chip,
        ] {
            let cells = f32::from(size) / CELL;
            assert_eq!(cells.fract(), 0., "{size:?}");
        }
        assert_eq!(look.border, px(CELL));
        assert_eq!(look.shadow.map(|shadow| shadow.y), Some(px(CELL)));
    }

    #[test]
    fn the_faces_are_the_pixel_fonts() {
        let look = gamme();
        assert_eq!(look.fonts.ui.as_ref(), fonts::TEXT);
        assert_eq!(look.fonts.title.as_deref(), Some(fonts::TITLE));
        assert_eq!(look.fonts.control.as_deref(), Some(fonts::TITLE));
    }
}
