//! The falling notes: each note drops toward the key that plays it, its length how long it is
//! held, and reaches the keyboard at the moment it is due. Drawn at every frame in one canvas,
//! as plain rectangles on whole pixels (the pixel-art look of the game parts comes later, #29).
//!
//! The right hand is the accent, the left the second colour; a note on a black key is darker.
//! A key sounds in its hand's colour while the piece plays it, and in the hit colour while it is
//! lit from outside ([`Lights`]: the keys the player strikes).
//!
//! A judgement (`flash.rs`) flashes in its grade's colour on the note it judged, while that note
//! is near the keys, and on its key, and its word rises over the key and fades: one word for the
//! notes of a chord judged alike.

use std::sync::Arc;
use std::time::Instant;

use gpui_design_system::color::{hex, mix};
use gpui_design_system::{Palette, Type as _};
use gpui_kit::*;

use crate::flash::{self, Flashes, Inks};
use crate::keyboard::{Keyboard, Lights, is_black};
use crate::theme;
use crate::timeline::{Hand, Timeline};

/// How much of the piece the falling area shows ahead of now, in ms at its own tempo.
pub const LOOKAHEAD_MS: f64 = 3_000.;

/// The colours of the falling notes and the keyboard.
#[derive(Clone, Copy)]
struct Look {
    ground: Hsla,
    line: Hsla,
    now: Hsla,
    white: Hsla,
    black: Hsla,
    gap: Hsla,
    inks: Inks,
}

impl Look {
    fn of(p: &Palette) -> Self {
        let (paper, night) = (hex(theme::PAPER), hex(theme::NIGHT));
        Self {
            ground: p.card,
            line: p.line,
            now: p.line2,
            white: paper,
            black: night,
            gap: mix(night, paper, 0.35),
            inks: Inks::of(p),
        }
    }

    fn hand(hand: Hand, black: bool) -> Hsla {
        let color = match hand {
            Hand::Right => hex(theme::ACCENT),
            Hand::Left => hex(theme::SECOND),
        };
        match black {
            true => mix(color, hex(theme::NIGHT), 0.3),
            false => color,
        }
    }
}

/// A rectangle in whole pixels, from the canvas's top left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Where things go in a canvas of `width` by `height`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    keyboard: Keyboard,
    /// The keyboard's left edge, centred.
    left: f32,
    /// A white key's width.
    white: f32,
    /// The falling area's height; the keys are under it.
    area: f32,
    keys: f32,
}

impl Layout {
    fn new(keyboard: Keyboard, width: f32, height: f32) -> Self {
        let whites = keyboard.whites().max(1) as f32;
        let white = (width / whites).floor().max(1.);
        let keys = (height * 0.2).clamp(40., 120.).min(height).round();
        Self {
            keyboard,
            left: ((width - white * whites) / 2.).floor().max(0.),
            white,
            area: (height - keys).max(0.),
            keys,
        }
    }

    /// The left edge and the width of `key`, in pixels.
    fn key(&self, key: u8) -> (f32, f32) {
        let (left, width) = self.keyboard.place(key);
        (
            (self.left + left * self.white).round(),
            (width * self.white).round(),
        )
    }

    /// The height of `ms` of the piece in the falling area.
    fn per_ms(&self) -> f32 {
        self.area / LOOKAHEAD_MS as f32
    }

    /// Where a note from `on_ms` to `off_ms` is drawn at `position`, cut to the falling area;
    /// none when it is not in it.
    fn note(&self, key: u8, on_ms: f64, off_ms: f64, position: f64) -> Option<Rect> {
        if off_ms <= position || on_ms > position + LOOKAHEAD_MS || !self.keyboard.shows(key) {
            return None;
        }
        let y = |ms: f64| (self.area - (ms - position) as f32 * self.per_ms()).round();
        let bottom = y(on_ms).min(self.area);
        let top = y(off_ms).max(0.);
        let (x, w) = self.key(key);
        // A pixel apart from its neighbours, and from the note after it on the same key.
        Some(Rect {
            x: x + 1.,
            y: top,
            w: (w - 2.).max(1.),
            h: (bottom - top - 1.).max(2.),
        })
    }
}

/// The falling notes of `timeline` at `position`, over its keyboard, filling their parent, with
/// the judgements of `flashes` as they stand at `now`; their words in the kit's face.
pub fn falling(
    timeline: Arc<Timeline>,
    keyboard: Keyboard,
    position: f64,
    lights: Lights,
    flashes: Flashes,
    now: Instant,
    p: &Palette,
) -> impl IntoElement {
    let look = Look::of(p);
    let notes = canvas(
        |_, _, _| {},
        move |bounds, (), window, cx| {
            let shown = Shown {
                lights: &lights,
                flashes: &flashes,
                now,
            };
            paint(
                &timeline, keyboard, position, &shown, look, bounds, window, cx,
            );
        },
    )
    .size_full();
    // The canvas paints its words in the text style around it.
    div().size_full().ui_text().semibold().child(notes)
}

/// What is lit and judged from outside the piece, at the frame's moment.
struct Shown<'a> {
    lights: &'a Lights,
    flashes: &'a Flashes,
    now: Instant,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one canvas's paint: what it draws, and where"
)]
fn paint(
    timeline: &Timeline,
    keyboard: Keyboard,
    position: f64,
    shown: &Shown,
    look: Look,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let now = shown.now;
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let layout = Layout::new(keyboard, width, height);
    let origin = bounds.origin;
    let mut quad = |rect: Rect, color: Hsla| {
        let at = point(origin.x + px(rect.x), origin.y + px(rect.y));
        window.paint_quad(fill(Bounds::new(at, size(px(rect.w), px(rect.h))), color));
    };

    quad(
        Rect {
            x: 0.,
            y: 0.,
            w: width,
            h: height,
        },
        look.ground,
    );
    // A line above each C, and across at each measure to come.
    for key in keyboard.keys().filter(|key| key % 12 == 0) {
        let (x, _) = layout.key(key);
        quad(
            Rect {
                x,
                y: 0.,
                w: 1.,
                h: layout.area,
            },
            look.line,
        );
    }
    let first = timeline.measure_at(position);
    for measure in first + 1.. {
        let Some(at) = timeline.measure_start_ms(measure) else {
            break;
        };
        if at > position + LOOKAHEAD_MS {
            break;
        }
        let y = (layout.area - (at - position) as f32 * layout.per_ms()).round();
        quad(
            Rect {
                x: layout.left,
                y,
                w: layout.white * keyboard.whites() as f32,
                h: 1.,
            },
            look.line,
        );
    }

    // The notes on white keys first: those on black keys lie over them, as their keys do.
    let coming = &timeline.notes()[..timeline
        .notes()
        .partition_point(|note| note.on_ms <= position + LOOKAHEAD_MS)];
    let mut sounding: [Option<Hand>; 128] = [None; 128];
    for black in [false, true] {
        for (index, note) in coming
            .iter()
            .enumerate()
            .filter(|(_, note)| is_black(note.key) == black)
        {
            if note.on_ms <= position && note.off_ms > position {
                sounding[usize::from(note.key)] = Some(note.hand);
            }
            if let Some(rect) = layout.note(note.key, note.on_ms, note.off_ms, position) {
                let color = Look::hand(note.hand, black);
                let color = match shown.flashes.on_note(index, now) {
                    Some(f) if (note.on_ms - position).abs() <= flash::NEAR_MS => {
                        mix(color, look.inks.get(f.grade), flash::TINT * f.strength(now))
                    }
                    _ => color,
                };
                quad(rect, color);
            }
        }
    }
    quad(
        Rect {
            x: 0.,
            y: layout.area - 2.,
            w: width,
            h: 2.,
        },
        look.now,
    );

    // The keys: a key struck by the player is the hit colour, one the piece plays its hand's;
    // a key judged takes its grade's colour, fading.
    let color = |key: u8, idle: Hsla| {
        let color = match (shown.lights.lit(key), sounding[usize::from(key)]) {
            (true, _) => hex(theme::HIT),
            (false, Some(hand)) => Look::hand(hand, is_black(key)),
            (false, None) => idle,
        };
        match shown.flashes.on_key(key, now) {
            Some(f) => mix(color, look.inks.get(f.grade), flash::TINT * f.strength(now)),
            None => color,
        }
    };
    let top = layout.area;
    for key in keyboard.keys().filter(|&key| !is_black(key)) {
        let (x, w) = layout.key(key);
        quad(
            Rect {
                x,
                y: top,
                w,
                h: layout.keys,
            },
            look.gap,
        );
        quad(
            Rect {
                x: x + 1.,
                y: top,
                w: (w - 1.).max(1.),
                h: layout.keys - 1.,
            },
            color(key, look.white),
        );
    }
    let black_height = (layout.keys * 0.62).round();
    for key in keyboard.keys().filter(|&key| is_black(key)) {
        let (x, w) = layout.key(key);
        quad(
            Rect {
                x,
                y: top,
                w,
                h: black_height,
            },
            look.gap,
        );
        quad(
            Rect {
                x: x + 1.,
                y: top,
                w: (w - 2.).max(1.),
                h: black_height - 1.,
            },
            color(key, look.black),
        );
    }

    // The words, over the keys they say, rising as they fade; the newest nearest the keys, an
    // older one in its way lifted over it.
    let style = window.text_style();
    let font_size = px(flash::WORD_SIZE);
    let tall = (flash::WORD_SIZE * 1.4).round();
    let mut said: Vec<flash::Place> = Vec::new();
    for word in shown.flashes.words(now).iter().rev() {
        if !keyboard.shows(word.low) || !keyboard.shows(word.high) {
            continue;
        }
        let (low, _) = layout.key(word.low);
        let (high, high_width) = layout.key(word.high);
        let middle = (low + high + high_width) / 2.;
        let text = SharedString::from(word.grade.word());
        let run = TextRun {
            color: look.inks.get(word.grade).opacity(word.flash.strength(now)),
            ..style.to_run(text.len())
        };
        let line = window
            .text_system()
            .shape_line(text, font_size, &[run], None);
        let wide = f32::from(line.width());
        let left = (middle - wide / 2.).clamp(0., (width - wide).max(0.));
        let top = layout.area - 4. - tall - flash::RISE * word.flash.rise(now);
        let pad = flash::WORD_PAD;
        let place = flash::lift(&said, left - pad, wide + 2. * pad, top, tall);
        said.push(place);
        let at = point(
            origin.x + px(place.left.round()),
            origin.y + px(place.top.round().max(0.)),
        );
        // On the ground's colour: a word over the note it judged, tinted alike, still reads.
        let ground = look
            .ground
            .opacity(flash::WORD_GROUND * word.flash.strength(now));
        window.paint_quad(fill(
            Bounds::new(at, size(px(place.wide.round()), px(tall))),
            ground,
        ));
        // A word that cannot be shaped is not said; the flash on the key still shows.
        let text_at = point(at.x + px(pad), at.y);
        let _ = line.paint(text_at, px(tall), TextAlign::Left, None, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{LOOKAHEAD_MS, Layout, Rect};
    use crate::keyboard::Keyboard;

    /// Two octaves from C3, 14 white keys of 20 px in 290 px, over 400 px of height.
    fn layout() -> Layout {
        Layout::new(Keyboard::fit(48, 71), 290., 500.)
    }

    #[test]
    fn the_keyboard_is_centred_on_whole_pixels() {
        let layout = layout();
        assert_eq!((layout.white, layout.left), (20., 5.));
        assert_eq!((layout.area, layout.keys), (400., 100.));
        assert_eq!(layout.key(48), (5., 20.));
        assert_eq!(layout.key(49), (19., 12.));
    }

    #[test]
    fn a_note_reaches_the_keys_when_it_is_due() {
        let layout = layout();
        // Due in half the look-ahead: its start is halfway down, it lasts 300 ms (40 px).
        let half = LOOKAHEAD_MS / 2.;
        let rect = layout.note(60, half, half + 300., 0.).unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 146.,
                y: 160.,
                w: 18.,
                h: 39.
            }
        );
        // Sounding now: its bottom is on the keys, and what has sounded is gone.
        let now = layout.note(60, 0., 750., 375.).unwrap();
        assert_eq!(now.y + now.h + 1., layout.area);
        assert_eq!(now.h, 49.);
    }

    #[test]
    fn notes_out_of_the_area_or_the_keyboard_are_not_drawn() {
        let layout = layout();
        assert_eq!(layout.note(60, 0., 100., 100.), None, "it has ended");
        assert_eq!(layout.note(60, LOOKAHEAD_MS + 1., 9e3, 0.), None, "not yet");
        assert_eq!(layout.note(90, 0., 100., 0.), None, "off the keyboard");
        // A long note is cut at the top of the area.
        assert_eq!(layout.note(60, 0., 1e6, 0.).unwrap().y, 0.);
    }
}
