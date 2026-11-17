//! The on-screen keyboard: which keys it shows, where each one is, and the keys lit from outside
//! the piece, the ones the player strikes (MIDI in, `midi_in.rs`), drawn in the hit colour.
//!
//! [`Lights`] are atomics, so that a thread reading a MIDI keyboard can light and put out keys
//! without waiting for the window; the falling notes (`falling.rs`) draw the keyboard with them
//! at every frame.

use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use crate::timeline::Timeline;

/// The fewest octaves shown, so that a piece of a few notes still looks like a keyboard.
const MIN_OCTAVES: u8 = 2;

/// How wide a black key is, against a white one.
pub const BLACK_WIDTH: f32 = 0.6;

pub fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// The keys shown: whole octaves, from a C to a B.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keyboard {
    low: u8,
    high: u8,
}

impl Keyboard {
    /// From the C at or below `low` to the B at or above `high`, at least two octaves around
    /// them, within the 128 keys of MIDI.
    pub fn fit(low: u8, high: u8) -> Self {
        let (low, high) = (low.min(high).min(127), high.max(low).min(127));
        let mut first = low - low % 12;
        let mut last = (high - high % 12).saturating_add(11).min(127);
        while last - first + 1 < 12 * MIN_OCTAVES {
            // Widened toward the middle of the keyboard.
            let middle = (u16::from(first) + u16::from(last)) / 2;
            if middle >= 60 && first >= 12 {
                first -= 12;
            } else if last <= 127 - 12 {
                last += 12;
            } else if first >= 12 {
                first -= 12;
            } else {
                break;
            }
        }
        Self {
            low: first,
            high: last,
        }
    }

    /// The keys of `timeline`'s notes; the two octaves around the middle C for a piece with none.
    pub fn for_timeline(timeline: &Timeline) -> Self {
        let keys = timeline.notes().iter().map(|note| note.key);
        match (keys.clone().min(), keys.max()) {
            (Some(low), Some(high)) => Self::fit(low, high),
            _ => Self::fit(48, 71),
        }
    }

    pub fn keys(&self) -> RangeInclusive<u8> {
        self.low..=self.high
    }

    pub fn whites(&self) -> usize {
        self.keys().filter(|&key| !is_black(key)).count()
    }

    /// Where `key` is: its left edge and its width, in widths of a white key from the keyboard's
    /// left edge. A black key sits across the line between the two white keys around it.
    pub fn place(&self, key: u8) -> (f32, f32) {
        let whites_before = (self.low..key).filter(|&k| !is_black(k)).count() as f32;
        if is_black(key) {
            (whites_before - BLACK_WIDTH / 2., BLACK_WIDTH)
        } else {
            (whites_before, 1.)
        }
    }

    pub fn shows(&self, key: u8) -> bool {
        self.keys().contains(&key)
    }
}

/// The keys lit from outside the piece; clones share them.
#[derive(Clone)]
pub struct Lights(Arc<[AtomicBool; 128]>);

impl Default for Lights {
    fn default() -> Self {
        Self(Arc::new(std::array::from_fn(|_| AtomicBool::new(false))))
    }
}

impl Lights {
    pub fn press(&self, key: u8) {
        if let Some(light) = self.0.get(usize::from(key)) {
            light.store(true, Relaxed);
        }
    }

    pub fn release(&self, key: u8) {
        if let Some(light) = self.0.get(usize::from(key)) {
            light.store(false, Relaxed);
        }
    }

    pub fn lit(&self, key: u8) -> bool {
        self.0
            .get(usize::from(key))
            .is_some_and(|light| light.load(Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keyboard_holds_whole_octaves_around_the_notes() {
        let keyboard = Keyboard::fit(50, 79);
        assert_eq!(keyboard.keys(), 48..=83);
        assert_eq!(keyboard.whites(), 21);
        // A few notes still show two octaves.
        assert_eq!(Keyboard::fit(60, 64).keys(), 48..=71);
        assert_eq!(Keyboard::fit(120, 127).keys(), 96..=127);
        assert_eq!(Keyboard::fit(0, 3).keys(), 0..=23);
        assert!(Keyboard::fit(21, 108).shows(21));
    }

    #[test]
    fn black_keys_sit_across_their_white_keys() {
        let keyboard = Keyboard::fit(48, 71);
        assert_eq!(keyboard.place(48), (0., 1.));
        assert_eq!(keyboard.place(50), (1., 1.));
        let (left, width) = keyboard.place(49);
        assert_eq!((left + width / 2., width), (1., BLACK_WIDTH));
        assert_eq!(keyboard.place(60).0, 7.);
    }

    #[test]
    fn keys_are_lit_from_another_thread() {
        let lights = Lights::default();
        let shared = lights.clone();
        std::thread::spawn(move || shared.press(61)).join().unwrap();
        assert!(lights.lit(61));
        assert!(!lights.lit(60));
        lights.release(61);
        assert!(!lights.lit(61));
        // Out of MIDI's range: nothing to light.
        lights.press(200);
        assert!(!lights.lit(200));
    }
}
