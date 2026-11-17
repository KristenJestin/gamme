//! Gamme's own look, "Nocturne" (README, "Brand"): the design system's palette, light and dark,
//! with the brand's colours in the places the kit has a role for them. What the kit has no role
//! for (the hit colour) is a constant here, waiting for the screen that draws it.

use gpui_design_system::color::{hex, mix, over};
use gpui_design_system::theme::{Palette, Shade};

/// The id of Gamme's own theme, as the kit keeps the user's choice (`appearance.rs`).
pub const ID: &str = "nocturne";

/// The primary: violet.
pub const ACCENT: u32 = 0x7C5CFF;
/// The second colour: cyan.
pub const SECOND: u32 = 0x20B2C8;
/// A note that was hit: pink. The kit's palette has no role for it, so it is not in `nocturne`;
/// the keyboard lights the keys the player strikes with it (`falling.rs`), and a note judged
/// perfect flashes in it (`flash.rs`).
pub const HIT: u32 = 0xFF7AB6;
/// The light shade's page.
pub const PAPER: u32 = 0xF6F5FB;
/// The dark shade's page.
pub const NIGHT: u32 = 0x13132A;

/// The palette of the theme in `shade`. The surfaces are tints of the night (dark) or of the paper
/// (light), the primary is the accent in both, the running state is the second colour. The states
/// the kit owns (waiting, done, failed, a diff) keep the kit's colours, so that they read the same
/// in every application. Every ink is then checked against what it sits on.
pub fn nocturne(shade: Shade) -> Palette {
    let kit = Palette::own(shade);
    let (night, paper) = (hex(NIGHT), hex(PAPER));
    let (accent, second) = (hex(ACCENT), hex(SECOND));
    let black = hex(0x000000);
    let palette = match shade {
        Shade::Dark => {
            let card = mix(night, paper, 0.05);
            let line = mix(night, paper, 0.11);
            Palette {
                body: mix(night, black, 0.4),
                bg: night,
                rail: mix(night, black, 0.2),
                side: mix(night, black, 0.1),
                card,
                card2: mix(night, paper, 0.09),
                line,
                line2: mix(night, paper, 0.17),
                hover: mix(night, paper, 0.07),
                sel: mix(night, accent, 0.28),
                text: paper,
                mute: mix(paper, night, 0.35),
                faint: mix(paper, night, 0.5),
                acc: accent,
                on_acc: night,
                shell: card,
                field: mix(night, black, 0.15),
                field_line: line,
                focus_line: mix(night, accent, 0.6),
                focus_ring: mix(night, accent, 0.25),
                run: second,
                run_bg: over(second, 0.15, night),
                ..kit
            }
        }
        Shade::Light => {
            let card = mix(paper, night, 0.035);
            let line = mix(paper, night, 0.09);
            Palette {
                body: mix(paper, night, 0.08),
                bg: paper,
                rail: mix(paper, night, 0.04),
                side: mix(paper, night, 0.03),
                card,
                card2: mix(paper, night, 0.07),
                line,
                line2: mix(paper, night, 0.15),
                hover: mix(paper, night, 0.055),
                sel: mix(paper, accent, 0.18),
                text: night,
                mute: mix(night, paper, 0.3),
                faint: mix(night, paper, 0.42),
                acc: accent,
                on_acc: night,
                shell: card,
                field: mix(paper, hex(0xffffff), 0.5),
                field_line: mix(paper, night, 0.12),
                focus_line: mix(paper, accent, 0.55),
                focus_ring: mix(paper, accent, 0.22),
                run: mix(second, night, 0.35),
                run_bg: over(second, 0.14, paper),
                ..kit
            }
        }
    };
    palette.with_readable_inks()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_design_system::color::{TEXT_CONTRAST, contrast};

    #[test]
    fn the_primary_is_the_accent_in_both_shades() {
        for shade in [Shade::Light, Shade::Dark] {
            assert_eq!(nocturne(shade).acc, hex(ACCENT), "{shade:?}");
        }
    }

    #[test]
    fn the_pages_are_the_paper_and_the_night() {
        assert_eq!(nocturne(Shade::Light).bg, hex(PAPER));
        assert_eq!(nocturne(Shade::Dark).bg, hex(NIGHT));
    }

    #[test]
    fn every_ink_reads_on_what_it_sits_on_in_both_shades() {
        for shade in [Shade::Light, Shade::Dark] {
            let p = nocturne(shade);
            let pairs = [
                ("text on the page", p.text, p.bg),
                ("text on a card", p.text, p.card),
                ("muted text on a card", p.mute, p.card),
                ("faint text on a quiet pill", p.faint, p.card2),
                ("the ink on the primary", p.on_acc, p.acc),
                ("the running state", p.run, p.run_bg),
                ("a done state", p.ok, p.ok_bg),
                ("a failed state", p.fail, p.fail_bg),
            ];
            for (what, ink, on) in pairs {
                let ratio = contrast(ink, on);
                assert!(ratio >= TEXT_CONTRAST, "{shade:?}: {what} is {ratio:.2}");
            }
        }
    }

    #[test]
    fn the_shades_differ() {
        assert_ne!(nocturne(Shade::Light), nocturne(Shade::Dark));
    }
}
