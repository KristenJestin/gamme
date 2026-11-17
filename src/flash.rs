//! The judgements as they come (design §5.1): each one flashes on its note and its key, in its
//! grade's colour, and its word rises over the keys and fades. Wrong keys flash on the key only:
//! they have no note.
//!
//! The play screen draws them: on the falling notes and the keyboard (`falling.rs`), on the
//! engraved note (`score_view.rs`). Here is what they are, pure, how long they last, and their
//! look: every colour and size they are drawn with, so that the game's restyle (#29) re-skins
//! them in one place. The colours are the palette's (the hit colour, the done, waiting and failed
//! states), none of their own.
//!
//! A judgement flashes from when it reaches the window: a miss is said some 350 ms after its note
//! (`judge.rs`), so it flashes when the note has gone by.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_design_system::Palette;
use gpui_design_system::color::hex;
use gpui_kit::Hsla;

use crate::judge::{Grade, Judgement};
use crate::theme;

/// How long a judgement shows.
pub const FLASH: Duration = Duration::from_millis(700);
/// How far a word rises while it fades, in pixels: six cells of the falling notes' grid.
pub const RISE: f32 = 24.;
/// The size of a word over the keys, in pixels: the title face (Jersey 10) at twice its grid.
pub const WORD_SIZE: f32 = 20.;
/// The room on either side of a word over the keys, on the ground's colour, in pixels: a cell.
pub const WORD_PAD: f32 = 4.;
/// How opaque the ground under a word over the keys is, at its brightest.
pub const WORD_GROUND: f32 = 0.85;
/// The size of a word over an engraved note, in pixels: the title face (Jersey 10) at its own
/// grid.
pub const SCORE_WORD_SIZE: f32 = 10.;
/// How far a flash spreads around an engraved notehead at its brightest, in pixels.
pub const HALO: f32 = 6.;
/// How much of its grade's colour a key or a note takes at the brightest of a flash.
pub const TINT: f32 = 0.85;
/// How far from the clock, in ms of the piece, a falling note still takes its flash: one the
/// loop brings round again is not the one judged.
pub const NEAR_MS: f64 = 1_000.;
/// Judgements of one grade this close in time are said by one word: a chord.
const TOGETHER: Duration = Duration::from_millis(40);
/// The most kept: more than a frame ever shows.
const KEPT: usize = 64;

/// The colour of each grade, from the palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inks {
    perfect: Hsla,
    good: Hsla,
    late: Hsla,
    missed: Hsla,
    wrong: Hsla,
}

impl Inks {
    /// Perfect is the hit colour, good the done state's, late the waiting state's, a miss faint
    /// (the note is gone), a wrong key the failed state's.
    pub fn of(p: &Palette) -> Self {
        Self {
            perfect: hex(theme::HIT),
            good: p.ok,
            late: p.wait,
            missed: p.faint,
            wrong: p.fail,
        }
    }

    pub fn get(&self, grade: Grade) -> Hsla {
        match grade {
            Grade::Perfect => self.perfect,
            Grade::Good => self.good,
            Grade::Late => self.late,
            Grade::Missed => self.missed,
            Grade::Wrong => self.wrong,
        }
    }
}

/// One judgement on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flash {
    pub grade: Grade,
    pub key: u8,
    /// The note judged, by its place in the timeline; none for a wrong key.
    pub note: Option<usize>,
    /// When it reached the window.
    pub at: Instant,
}

impl Flash {
    /// How far through its life it is at `now`: 0 when it comes, 1 when it is gone.
    pub fn age(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.at).as_secs_f32() / FLASH.as_secs_f32()).min(1.)
    }

    /// How bright it is at `now`: full at first, fading faster toward its end.
    pub fn strength(&self, now: Instant) -> f32 {
        let age = self.age(now);
        1. - age * age
    }

    /// How far its word has risen at `now`, 0 to 1: quickly at first, then slowing.
    pub fn rise(&self, now: Instant) -> f32 {
        let left = 1. - self.age(now);
        1. - left * left
    }

    fn shown(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.at) < FLASH
    }
}

/// A word over the keys: one grade, said for the keys `low` to `high` (a chord's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Word {
    pub grade: Grade,
    pub low: u8,
    pub high: u8,
    /// The first judgement it says: it fades and rises as that one does.
    pub flash: Flash,
}

/// The judgements on screen, oldest first.
#[derive(Clone, Debug, Default)]
pub struct Flashes {
    shown: VecDeque<Flash>,
}

/// The flashes as the transport adds them and the views draw them, on the window's thread.
pub type Shared = Rc<RefCell<Flashes>>;

impl Flashes {
    /// `judgement` reached the window at `at`.
    pub fn add(&mut self, judgement: &Judgement, at: Instant) {
        while self
            .shown
            .front()
            .is_some_and(|flash| !flash.shown(at) || self.shown.len() >= KEPT)
        {
            self.shown.pop_front();
        }
        self.shown.push_back(Flash {
            grade: judgement.grade,
            key: judgement.key,
            note: judgement.note,
            at,
        });
    }

    /// Those still showing at `now`, oldest first.
    pub fn live(&self, now: Instant) -> impl Iterator<Item = &Flash> + '_ {
        self.shown.iter().filter(move |flash| flash.shown(now))
    }

    /// Whether any shows at `now`: frames are wanted until none does.
    pub fn any(&self, now: Instant) -> bool {
        self.live(now).next().is_some()
    }

    /// The latest flash on `key` at `now`.
    pub fn on_key(&self, key: u8, now: Instant) -> Option<Flash> {
        self.live(now)
            .filter(|flash| flash.key == key)
            .last()
            .copied()
    }

    /// The latest flash on the note at `note` in the timeline at `now`.
    pub fn on_note(&self, note: usize, now: Instant) -> Option<Flash> {
        self.live(now)
            .filter(|flash| flash.note == Some(note))
            .last()
            .copied()
    }

    /// The words to say at `now`: one per judgement, but one for the notes of a chord judged
    /// alike.
    pub fn words(&self, now: Instant) -> Vec<Word> {
        let mut words: Vec<Word> = Vec::new();
        for flash in self.live(now) {
            match words.last_mut() {
                Some(word)
                    if word.grade == flash.grade
                        && flash.at.saturating_duration_since(word.flash.at) <= TOGETHER =>
                {
                    word.low = word.low.min(flash.key);
                    word.high = word.high.max(flash.key);
                }
                _ => words.push(Word {
                    grade: flash.grade,
                    low: flash.key,
                    high: flash.key,
                    flash: *flash,
                }),
            }
        }
        words
    }
}

/// Where a word is drawn: its left edge, its top and its width, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub left: f32,
    pub top: f32,
    pub wide: f32,
}

/// A word `wide` and `tall` at `left` and `top`, lifted a line at a time over the words `said`
/// it would cross: drawn newest first, the newest stays by its note and the older rise over it.
pub fn lift(said: &[Place], left: f32, wide: f32, mut top: f32, tall: f32) -> Place {
    let crosses = |other: &&Place, top: f32| {
        left < other.left + other.wide
            && other.left < left + wide
            && top < other.top + tall
            && other.top < top + tall
    };
    while said.iter().any(|other| crosses(&other, top)) {
        top = said
            .iter()
            .filter(|other| crosses(other, top))
            .map(|other| other.top - tall)
            .fold(top, f32::min);
    }
    Place { left, top, wide }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_in_the_way_of_another_is_lifted_over_it() {
        let first = lift(&[], 100., 40., 300., 18.);
        assert_eq!(first.top, 300.);
        // Over the key next to it, a little higher: lifted over the first.
        let second = lift(&[first], 120., 40., 295., 18.);
        assert_eq!(second.top, 282.);
        // Over the first two: over both.
        let third = lift(&[first, second], 110., 40., 300., 18.);
        assert_eq!(third.top, 264.);
        // Apart from them: where it is.
        let apart = lift(&[first, second], 200., 40., 300., 18.);
        assert_eq!(
            apart,
            Place {
                left: 200.,
                top: 300.,
                wide: 40.
            }
        );
    }

    fn judged(grade: Grade, key: u8, note: Option<usize>) -> Judgement {
        Judgement {
            grade,
            key,
            note,
            measure: 0,
            pass: 0,
            offset_ms: None,
            timed: true,
            speed: 1.,
            at_ms: 0.,
        }
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_flash_fades_and_rises_over_its_life() {
        let start = Instant::now();
        let flash = Flash {
            grade: Grade::Good,
            key: 60,
            note: Some(0),
            at: start,
        };
        assert_eq!(flash.strength(start), 1.);
        assert_eq!(flash.rise(start), 0.);
        let half = start + FLASH / 2;
        assert!((flash.strength(half) - 0.75).abs() < 1e-3);
        assert!((flash.rise(half) - 0.75).abs() < 1e-3);
        let end = start + FLASH;
        assert_eq!((flash.strength(end), flash.rise(end)), (0., 1.));
        assert!(!flash.shown(end));
    }

    #[test]
    fn a_note_and_a_key_show_their_latest_judgement_until_it_fades() {
        let start = Instant::now();
        let mut flashes = Flashes::default();
        flashes.add(&judged(Grade::Late, 60, Some(3)), start);
        flashes.add(&judged(Grade::Wrong, 60, None), start + 100 * MS);
        let now = start + 200 * MS;
        assert_eq!(flashes.on_note(3, now).unwrap().grade, Grade::Late);
        assert_eq!(flashes.on_key(60, now).unwrap().grade, Grade::Wrong);
        assert_eq!(flashes.on_key(62, now), None);
        // The wrong key has no note.
        assert_eq!(flashes.on_note(0, now), None);
        assert!(flashes.any(now));
        let gone = start + 100 * MS + FLASH;
        assert!(!flashes.any(gone));
        assert_eq!(flashes.on_key(60, gone), None);
    }

    #[test]
    fn a_chord_judged_alike_is_one_word() {
        let start = Instant::now();
        let mut flashes = Flashes::default();
        flashes.add(&judged(Grade::Perfect, 64, Some(1)), start);
        flashes.add(&judged(Grade::Perfect, 60, Some(0)), start + 10 * MS);
        flashes.add(&judged(Grade::Good, 67, Some(2)), start + 20 * MS);
        // A second perfect chord, later: a word of its own.
        flashes.add(&judged(Grade::Perfect, 72, Some(3)), start + 300 * MS);
        let words = flashes.words(start + 300 * MS);
        let said: Vec<(Grade, u8, u8)> = words.iter().map(|w| (w.grade, w.low, w.high)).collect();
        assert_eq!(
            said,
            [
                (Grade::Perfect, 60, 64),
                (Grade::Good, 67, 67),
                (Grade::Perfect, 72, 72)
            ]
        );
        assert_eq!(words[0].flash.at, start);
    }

    #[test]
    fn what_has_faded_is_let_go_and_never_too_many_are_kept() {
        let start = Instant::now();
        let mut flashes = Flashes::default();
        flashes.add(&judged(Grade::Missed, 50, Some(0)), start);
        flashes.add(&judged(Grade::Perfect, 52, Some(1)), start + FLASH);
        assert_eq!(flashes.shown.len(), 1);
        for n in 0..200 {
            flashes.add(&judged(Grade::Good, 60, Some(n)), start + FLASH);
        }
        assert_eq!(flashes.shown.len(), KEPT);
    }

    #[test]
    fn each_grade_has_a_colour_of_the_palette() {
        let p = crate::theme::nocturne(gpui_design_system::theme::Shade::Dark);
        let inks = Inks::of(&p);
        assert_eq!(inks.get(Grade::Perfect), hex(theme::HIT));
        assert_eq!(inks.get(Grade::Good), p.ok);
        assert_eq!(inks.get(Grade::Late), p.wait);
        assert_eq!(inks.get(Grade::Missed), p.faint);
        assert_eq!(inks.get(Grade::Wrong), p.fail);
    }
}
