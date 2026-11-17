//! The falling notes: each note drops toward the key that plays it, its length how long it is
//! held, and reaches the keyboard at the moment it is due. Drawn at every frame in one canvas, in
//! pixel art (#29): everything lies on a grid of [`CELL`] pixels, a cell the art's pixel, so that
//! each is a whole number of the screen's pixels at every whole scale.
//!
//! The right hand is the accent, the left the second colour; a note on a black key is darker. A
//! note is outlined in a darker shade of its colour, lit along its left edge, and dithered at its
//! top, where it ends. A key sounds in its hand's colour while the piece plays it, and in the hit
//! colour while it is lit from outside ([`Lights`]: the keys the player strikes); a key that
//! sounds is pressed: it sinks into its front edge.
//!
//! A judgement (`flash.rs`) flashes in its grade's colour on the note it judged, while that note
//! is near the keys, and on its key, and its word rises over the key and fades, a cell at a time:
//! one word for the notes of a chord judged alike.

use std::sync::Arc;
use std::time::Instant;

use gpui_design_system::Palette;
use gpui_design_system::color::{hex, mix};
use gpui_kit::*;

use crate::flash::{self, Flashes, Inks};
use crate::fonts;
use crate::keyboard::{Keyboard, Lights, is_black};
use crate::look::CELL;
use crate::theme;
use crate::timeline::{Hand, Timeline};

/// How much of the piece the falling area shows ahead of now, in ms at its own tempo.
pub const LOOKAHEAD_MS: f64 = 3_000.;

/// How many cells a key's front edge is high; a pressed key sinks by as much.
const LIP: f32 = 2.;

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

    /// The outline of a note of `color`.
    fn outline(color: Hsla) -> Hsla {
        mix(color, hex(theme::NIGHT), 0.55)
    }

    /// The lit left edge of a note, and of a white key at rest.
    fn light(color: Hsla) -> Hsla {
        mix(color, hex(0xFFFFFF), 0.4)
    }
}

/// A rectangle in whole cells, from the canvas's top left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Where things go in a canvas of `width` by `height` pixels, in cells.
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
        let (width, height) = ((width / CELL).floor(), (height / CELL).floor());
        let whites = keyboard.whites().max(1) as f32;
        let white = (width / whites).floor().max(1.);
        let keys = (height * 0.2)
            .clamp(40. / CELL, 120. / CELL)
            .min(height)
            .round();
        Self {
            keyboard,
            left: ((width - white * whites) / 2.).floor().max(0.),
            white,
            area: (height - keys).max(0.),
            keys,
        }
    }

    /// The left edge and the width of `key`.
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
        // A cell apart from its neighbours, and from the note after it on the same key.
        Some(Rect::new(
            x + 1.,
            top,
            (w - 2.).max(1.),
            (bottom - top - 1.).max(2.),
        ))
    }
}

/// The cells of a note of `color` in `rect`: its outline, its body, its lit left edge and the
/// dither of its top two rows. A note too thin for an inside is its outline.
fn note_cells(rect: Rect, color: Hsla) -> Vec<(Rect, Hsla)> {
    let outline = Look::outline(color);
    let mut cells = vec![(rect, outline)];
    if rect.w < 3. || rect.h < 3. {
        return cells;
    }
    let inside = Rect::new(rect.x + 1., rect.y + 1., rect.w - 2., rect.h - 2.);
    cells.push((inside, color));
    cells.push((
        Rect::new(inside.x, inside.y, 1., inside.h),
        Look::light(color),
    ));
    // A checkerboard of the outline over the first two rows: the note's end, frayed.
    for row in 0..2u8.min(inside.h as u8) {
        let y = inside.y + f32::from(row);
        let mut x = inside.x + 1. + f32::from(row % 2);
        while x < inside.x + inside.w {
            cells.push((Rect::new(x, y, 1., 1.), outline));
            x += 2.;
        }
    }
    cells
}

/// `pixels` on the grid: the nearest whole number of cells.
fn on_grid(pixels: f32) -> f32 {
    (pixels / CELL).round() * CELL
}

/// The falling notes of `timeline` at `position`, over its keyboard, filling their parent, with
/// the judgements of `flashes` as they stand at `now`; their words in the title face.
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
    div()
        .size_full()
        .font_family(fonts::TITLE)
        .text_size(px(flash::WORD_SIZE))
        .child(notes)
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
    // The grid starts on a whole pixel of the window.
    let origin = point(bounds.origin.x.round(), bounds.origin.y.round());
    let mut quad = |rect: Rect, color: Hsla| {
        let at = point(origin.x + px(rect.x * CELL), origin.y + px(rect.y * CELL));
        let size = size(px(rect.w * CELL), px(rect.h * CELL));
        window.paint_quad(fill(Bounds::new(at, size), color));
    };

    quad(
        Rect::new(0., 0., (width / CELL).ceil(), (height / CELL).ceil()),
        look.ground,
    );
    // A line above each C, and across at each measure to come.
    for key in keyboard.keys().filter(|key| key % 12 == 0) {
        let (x, _) = layout.key(key);
        quad(Rect::new(x, 0., 1., layout.area), look.line);
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
        let across = layout.white * keyboard.whites() as f32;
        quad(Rect::new(layout.left, y, across, 1.), look.line);
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
                for (cell, color) in note_cells(rect, color) {
                    quad(cell, color);
                }
            }
        }
    }
    // Now: a dashed line across, where the notes meet the keys.
    let columns = (width / CELL).ceil();
    let mut x = 0.;
    while x < columns {
        quad(Rect::new(x, layout.area - 1., 2., 1.), look.now);
        x += 4.;
    }

    // The keys: a key struck by the player is the hit colour, one the piece plays its hand's;
    // either is pressed. A key judged takes its grade's colour, fading.
    let color = |key: u8, idle: Hsla| {
        let (color, down) = match (shown.lights.lit(key), sounding[usize::from(key)]) {
            (true, _) => (hex(theme::HIT), true),
            (false, Some(hand)) => (Look::hand(hand, is_black(key)), true),
            (false, None) => (idle, false),
        };
        let color = match shown.flashes.on_key(key, now) {
            Some(f) => mix(color, look.inks.get(f.grade), flash::TINT * f.strength(now)),
            None => color,
        };
        (color, down)
    };
    let (top, keys) = (layout.area, layout.keys);
    let night = hex(theme::NIGHT);
    for key in keyboard.keys().filter(|&key| !is_black(key)) {
        let (x, w) = layout.key(key);
        let (fill, down) = color(key, look.white);
        let sink = if down { LIP } else { 0. };
        quad(Rect::new(x, top, w, keys), look.gap);
        quad(Rect::new(x + 1., top + sink, w - 1., keys - 1. - LIP), fill);
        quad(
            Rect::new(x + 1., top + keys - 1. - LIP + sink, w - 1., LIP - sink),
            mix(fill, night, 0.22),
        );
        if !down {
            quad(
                Rect::new(x + 1., top, 1., keys - 1. - LIP),
                Look::light(fill),
            );
        }
    }
    let black_height = (keys * 0.62).round();
    let paper = hex(theme::PAPER);
    for key in keyboard.keys().filter(|&key| is_black(key)) {
        let (x, w) = layout.key(key);
        let (fill, down) = color(key, look.black);
        quad(Rect::new(x, top, w, black_height), look.gap);
        quad(
            Rect::new(x + 1., top, (w - 2.).max(1.), black_height - 1.),
            fill,
        );
        if !down {
            quad(
                Rect::new(x + 1., top + black_height - 1. - LIP, (w - 2.).max(1.), LIP),
                mix(fill, paper, 0.22),
            );
            quad(
                Rect::new(x + 1., top, 1., black_height - 1. - LIP),
                mix(fill, paper, 0.12),
            );
        }
    }

    // The words, over the keys they say, rising as they fade, a cell at a time; the newest
    // nearest the keys, an older one in its way lifted over it. Each sits on whole cells.
    let style = window.text_style();
    let font_size = px(flash::WORD_SIZE);
    let tall = (flash::WORD_SIZE * 1.2 / CELL).ceil() * CELL;
    let mut said: Vec<flash::Place> = Vec::new();
    for word in shown.flashes.words(now).iter().rev() {
        if !keyboard.shows(word.low) || !keyboard.shows(word.high) {
            continue;
        }
        let (low, _) = layout.key(word.low);
        let (high, high_width) = layout.key(word.high);
        let middle = (low + high + high_width) / 2. * CELL;
        let text = SharedString::from(word.grade.word());
        let run = TextRun {
            color: look.inks.get(word.grade).opacity(word.flash.strength(now)),
            ..style.to_run(text.len())
        };
        let line = window
            .text_system()
            .shape_line(text, font_size, &[run], None);
        let wide = f32::from(line.width());
        let boxed = ((wide + 2. * flash::WORD_PAD) / CELL).ceil() * CELL;
        let left = on_grid((middle - boxed / 2.).clamp(0., (width - boxed).max(0.)));
        let top = on_grid(layout.area * CELL - CELL - tall - flash::RISE * word.flash.rise(now));
        let place = flash::lift(&said, left, boxed, top, tall);
        said.push(place);
        let at = point(origin.x + px(place.left), origin.y + px(place.top.max(0.)));
        // On the ground's colour: a word over the note it judged, tinted alike, still reads.
        let ground = look
            .ground
            .opacity(flash::WORD_GROUND * word.flash.strength(now));
        window.paint_quad(fill(
            Bounds::new(at, size(px(place.wide), px(tall))),
            ground,
        ));
        // A word that cannot be shaped is not said; the flash on the key still shows.
        let text_at = point(at.x + px(((boxed - wide) / 2.).round()), at.y);
        let _ = line.paint(text_at, px(tall), TextAlign::Left, None, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{CELL, LOOKAHEAD_MS, Layout, Rect, note_cells, on_grid};
    use crate::keyboard::Keyboard;
    use gpui_design_system::color::hex;

    /// Two octaves from C3, 14 white keys of 5 cells in 72 cells, over 125 cells of height.
    fn layout() -> Layout {
        Layout::new(Keyboard::fit(48, 71), 290., 500.)
    }

    #[test]
    fn the_keyboard_is_centred_on_whole_cells() {
        let layout = layout();
        assert_eq!(CELL, 4.);
        assert_eq!((layout.white, layout.left), (5., 1.));
        assert_eq!((layout.area, layout.keys), (100., 25.));
        assert_eq!(layout.key(48), (1., 5.));
        // C#3 across the line between C3 and D3 (at 6): from 4.5 cells, rounded.
        assert_eq!(layout.key(49), (5., 3.));
    }

    #[test]
    fn a_note_reaches_the_keys_when_it_is_due() {
        let layout = layout();
        // Due in half the look-ahead: its start is halfway down, it lasts 300 ms (10 cells).
        let half = LOOKAHEAD_MS / 2.;
        let rect = layout.note(60, half, half + 300., 0.).unwrap();
        assert_eq!(rect, Rect::new(37., 40., 3., 9.));
        // Sounding now: its bottom is on the keys, and what has sounded is gone.
        let now = layout.note(60, 0., 900., 300.).unwrap();
        assert_eq!(now.y + now.h + 1., layout.area);
        assert_eq!(now.h, 19.);
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

    #[test]
    fn a_note_is_outlined_lit_and_dithered_inside_itself() {
        let rect = Rect::new(10., 20., 5., 8.);
        let cells = note_cells(rect, hex(0x7C5CFF));
        // Outline, body, lit edge, then the dither: every cell inside the note.
        assert_eq!(cells[0].0, rect);
        assert_eq!(cells[1].0, Rect::new(11., 21., 3., 6.));
        for (cell, _) in &cells {
            assert!(
                cell.x >= rect.x && cell.x + cell.w <= rect.x + rect.w,
                "{cell:?}"
            );
            assert!(
                cell.y >= rect.y && cell.y + cell.h <= rect.y + rect.h,
                "{cell:?}"
            );
            assert_eq!((cell.x.fract(), cell.y.fract()), (0., 0.), "on the grid");
        }
        // Too thin for an inside: only its outline.
        assert_eq!(
            note_cells(Rect::new(0., 0., 2., 9.), hex(0x7C5CFF)).len(),
            1
        );
    }

    #[test]
    fn a_word_rises_a_whole_cell_at_a_time() {
        assert_eq!(on_grid(13.9), 12.);
        assert_eq!(on_grid(14.1), 16.);
        assert_eq!(on_grid(-2.1), -4.);
    }
}
