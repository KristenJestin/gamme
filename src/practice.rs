//! Practice (design §6): which hands the player plays, the wait, and the A–B loop.
//!
//! - **Hands**: listen (the computer plays both hands), the right hand, the left hand, or both.
//!   The computer plays the hands the player does not (`piano::Hands`), and never the player's:
//!   the end of a note of his hand is not the computer's either (`piano.rs`).
//! - **Wait**: the clock stops at each chord of the player's hands until he strikes it, the
//!   computer's hand with it (`clock.rs` holds the position; [`Waiter`] judges the keys). A chord
//!   is struck when each of its keys was struck for it and all are down at once; a wrong key
//!   releases nothing, and a key held from before does not count: it is struck again.
//!   A strike counts from [`EARLY_MS`] of real time before the chord (at the speed played), so
//!   that a player a little ahead of the clock is not made to strike twice; before that it is
//!   for nothing. Late is the wait itself.
//! - **Loop**: measures A to B, in the order of play (a repeat plays a measure twice: each time is
//!   its own), chosen where the piece stands, again and again with no gap (`clock.rs` wraps the
//!   position, `piano.rs` lets the piece's notes go at the seam).
//! - **Rework** (`run_end.rs`): the end of a run loops its measures most missed, apart ones one
//!   after the other (the clock's spans), slowed. It takes the A–B loop's place; setting A, B or
//!   clearing the loop ends it.
//!
//! The choice of hands and the wait are kept with a piece of the library; the loop is not.

use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::clock::{Clock, Marks, Spans, Wait};
use crate::player::Player;
use crate::run_end::Plan;
use crate::timeline::{Hand, Timeline};

/// How early, in real time, a key struck before its chord still counts for it.
pub const EARLY_MS: f64 = 200.;

/// The notes of one hand that start within this of the first are one chord: struck together.
const CHORD_SPREAD_MS: f64 = 30.;

/// What the player plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Playing {
    /// Nothing: the computer plays the piece.
    #[default]
    Listen,
    Right,
    Left,
    Both,
}

impl Playing {
    pub const ALL: [Playing; 4] = [
        Playing::Listen,
        Playing::Right,
        Playing::Left,
        Playing::Both,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// The hands the computer plays: right, left.
    fn computer(self) -> (bool, bool) {
        match self {
            Playing::Listen => (true, true),
            Playing::Right => (false, true),
            Playing::Left => (true, false),
            Playing::Both => (false, false),
        }
    }

    /// Whose chords a wait waits for.
    fn wait(self) -> Wait {
        match self {
            Playing::Listen => Wait::Off,
            Playing::Right => Wait::Right,
            Playing::Left => Wait::Left,
            Playing::Both => Wait::Both,
        }
    }

    /// "your right hand", for what the window says while it waits.
    pub fn words(self) -> &'static str {
        match self {
            Playing::Listen => "nobody",
            Playing::Right => "your right hand",
            Playing::Left => "your left hand",
            Playing::Both => "your hands",
        }
    }
}

/// The hands and the wait, as a piece keeps them. What is missing is its default, and what is
/// not known (a later Gamme's) is listening: a piece is never lost for its practice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    #[serde(default, deserialize_with = "playing_or_listen")]
    pub playing: Playing,
    #[serde(default)]
    pub wait: bool,
}

fn playing_or_listen<'de, D: serde::Deserializer<'de>>(read: D) -> Result<Playing, D::Error> {
    let value = serde_json::Value::deserialize(read)?;
    Ok(Playing::deserialize(value).unwrap_or_default())
}

impl Choice {
    /// What the clock waits for: the player's chords when he plays and waits.
    pub fn wait_for(self) -> Wait {
        if self.wait {
            self.playing.wait()
        } else {
            Wait::Off
        }
    }
}

/// The keys the player strikes together, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct Chord {
    pub on_ms: f64,
    /// Sorted, each once.
    pub keys: Vec<u8>,
}

/// The chords of a piece, for each hand the player may play: right, left, both, as
/// [`Wait::list`] orders them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chords([Vec<Chord>; 3]);

impl Chords {
    pub fn of(timeline: &Timeline) -> Self {
        let of = |keep: &dyn Fn(Hand) -> bool| {
            let mut chords: Vec<Chord> = Vec::new();
            // The timeline's notes are by time.
            for note in timeline.notes().iter().filter(|note| keep(note.hand)) {
                match chords.last_mut() {
                    Some(chord) if note.on_ms - chord.on_ms <= CHORD_SPREAD_MS => {
                        chord.keys.push(note.key);
                    }
                    _ => chords.push(Chord {
                        on_ms: note.on_ms,
                        keys: vec![note.key],
                    }),
                }
            }
            for chord in &mut chords {
                chord.keys.sort_unstable();
                chord.keys.dedup();
            }
            chords
        };
        Self([
            of(&|hand| hand == Hand::Right),
            of(&|hand| hand == Hand::Left),
            of(&|_| true),
        ])
    }

    pub fn get(&self, wait: Wait, index: usize) -> Option<&Chord> {
        self.0.get(wait.list()?)?.get(index)
    }

    /// What the clock of `timeline` is made with: its measures, and where these chords start.
    pub fn marks(&self, timeline: &Timeline) -> Marks {
        Marks {
            measures_ms: (0..timeline.measure_count())
                .filter_map(|index| timeline.measure_start_ms(index))
                .collect(),
            gates_ms: std::array::from_fn(|list| {
                self.0[list].iter().map(|chord| chord.on_ms).collect()
            }),
        }
    }
}

/// Judges the keys the player strikes against the chord the clock waits for, and releases it
/// once it is struck. The window's: it is told each key as it comes from MIDI in.
pub struct Waiter {
    chords: Arc<Chords>,
    down: [bool; 128],
    /// The chord the keys below were struck for: its list, its place, its pass.
    owed: Option<(Wait, usize, u32)>,
    struck: Vec<u8>,
}

impl Waiter {
    pub fn new(chords: Arc<Chords>) -> Self {
        Self {
            chords,
            down: [false; 128],
            owed: None,
            struck: Vec::new(),
        }
    }

    /// [`Waiter::key_at`], the key arrived now.
    #[cfg(test)]
    pub fn key(&mut self, key: u8, velocity: u8, clock: &Clock) -> bool {
        self.key_at(key, velocity, Instant::now(), clock)
    }

    /// A key struck (`velocity` above 0) or let go, arrived at `at`. True when it was the last
    /// one of the chord owed, which the clock is told. How early it is is read from where the
    /// piece stood when it arrived, as the judge reads it (`judge.rs`).
    pub fn key_at(&mut self, key: u8, velocity: u8, at: Instant, clock: &Clock) -> bool {
        let key = key & 127;
        self.down[usize::from(key)] = velocity > 0;
        if velocity == 0 || !clock.playing() {
            return false;
        }
        let Some(owed) = clock.owed_at(at) else {
            return false;
        };
        // Too early: for nothing.
        if owed.ahead_ms > EARLY_MS * clock.speed().factor() {
            return false;
        }
        let Some(chord) = self.chords.get(owed.wait, owed.index) else {
            return false;
        };
        let id = (owed.wait, owed.index, owed.pass);
        if self.owed != Some(id) {
            self.owed = Some(id);
            self.struck.clear();
        }
        // A wrong key releases nothing, and takes nothing away.
        if chord.keys.binary_search(&key).is_err() {
            return false;
        }
        if !self.struck.contains(&key) {
            self.struck.push(key);
        }
        let done = chord
            .keys
            .iter()
            .all(|k| self.struck.contains(k) && self.down[usize::from(*k)]);
        if done {
            clock.release_chord(&owed);
            self.owed = None;
            self.struck.clear();
        }
        done
    }
}

/// The practice of a piece being played: the choice, the loop, and the judge of the wait.
pub struct Practice {
    choice: Choice,
    /// The loop's ends as chosen, measures in the order of play; it plays once both are.
    a: Option<usize>,
    b: Option<usize>,
    /// Rework under way, in the A–B loop's place.
    rework: Option<Plan>,
    /// Whether a MIDI keyboard is listened to: with none, nothing would ever release a wait, and
    /// the clock does not wait, whatever is chosen.
    keyboard: bool,
    waiter: Waiter,
}

impl Practice {
    /// With no keyboard known yet ([`Practice::set_keyboard`]).
    pub fn new(choice: Choice, chords: Arc<Chords>) -> Self {
        Self {
            choice,
            a: None,
            b: None,
            rework: None,
            keyboard: false,
            waiter: Waiter::new(chords),
        }
    }

    /// The same choice, loop and keyboard, for the player that takes `player`'s place (the hands
    /// of a MIDI file's parts changed).
    pub fn carried(&self, chords: Arc<Chords>) -> Self {
        Self {
            a: self.a,
            b: self.b,
            rework: self.rework.clone(),
            keyboard: self.keyboard,
            ..Self::new(self.choice, chords)
        }
    }

    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// Whether the wait can be had: the player plays a hand, on a keyboard listened to.
    pub fn can_wait(&self) -> bool {
        self.keyboard && self.choice.playing != Playing::Listen
    }

    /// Whether a keyboard is listened to.
    pub fn keyboard(&self) -> bool {
        self.keyboard
    }

    /// What the clock waits for: the choice, while there is a keyboard to release it.
    fn wait_now(&self) -> Wait {
        if self.keyboard {
            self.choice.wait_for()
        } else {
            Wait::Off
        }
    }

    /// A keyboard is listened to, or none any more; true when that changed.
    pub fn set_keyboard(&mut self, keyboard: bool, player: &Player) -> bool {
        let changed = self.keyboard != keyboard;
        if changed {
            self.keyboard = keyboard;
            self.apply(player);
        }
        changed
    }

    /// The loop's first and last measures, once both are chosen.
    pub fn looped(&self) -> Option<(usize, usize)> {
        let (a, b) = self.a.zip(self.b)?;
        Some((a.min(b), a.max(b)))
    }

    /// Rework under way.
    pub fn reworking(&self) -> Option<&Plan> {
        self.rework.as_ref()
    }

    /// What the clock loops: Rework's spans, or the A–B loop.
    pub fn spans(&self) -> Spans {
        match &self.rework {
            Some(plan) => plan.spans,
            None => Spans::of(self.looped()),
        }
    }

    /// The loop's start, while its end is not chosen yet.
    pub fn half_loop(&self) -> Option<usize> {
        match (self.a, self.b) {
            (Some(a), None) | (None, Some(a)) => Some(a),
            _ => None,
        }
    }

    /// Puts the choice and the loop on `player`'s output and clock.
    pub fn apply(&self, player: &Player) {
        let (right, left) = self.choice.playing.computer();
        player.hands.set(right, left);
        player.clock.set_wait(self.wait_now());
        player.clock.set_spans(self.spans());
    }

    pub fn set_playing(&mut self, playing: Playing, player: &Player) {
        self.choice.playing = playing;
        self.apply(player);
    }

    /// False, and nothing done, for a wait that cannot be had ([`Practice::can_wait`]); it can
    /// always be ended.
    pub fn set_wait(&mut self, wait: bool, player: &Player) -> bool {
        if wait && !self.can_wait() {
            return false;
        }
        self.choice.wait = wait;
        self.apply(player);
        true
    }

    /// The loop starts at the measure the piece stands in; Rework ends.
    pub fn set_a(&mut self, player: &Player) {
        self.a = Some(player.clock.measure(&player.timeline));
        self.rework = None;
        self.loop_changed(player);
    }

    /// The loop ends with the measure the piece stands in; Rework ends.
    pub fn set_b(&mut self, player: &Player) {
        self.b = Some(player.clock.measure(&player.timeline));
        self.rework = None;
        self.loop_changed(player);
    }

    pub fn clear_loop(&mut self, player: &Player) {
        self.a = None;
        self.b = None;
        self.rework = None;
        self.loop_changed(player);
    }

    /// Loops `plan`'s measures at its speed, from the first of them, in the A–B loop's place.
    /// The speed stays when Rework ends (A, B, Clear, Play again): the player sets it back, as
    /// any speed he chose.
    pub fn rework(&mut self, plan: Plan, player: &Player) {
        self.a = None;
        self.b = None;
        player.set_speed(plan.speed);
        self.rework = Some(plan);
        self.loop_changed(player);
    }

    /// A loop made while the piece stands outside it starts at its first span's start.
    fn loop_changed(&mut self, player: &Player) {
        self.apply(player);
        let spans = self.spans();
        let Some(&(first, _)) = spans.as_slice().first() else {
            return;
        };
        let timeline = &player.timeline;
        let at = player.clock.position_ms();
        let inside = spans.as_slice().iter().any(|&(first, last)| {
            let start = timeline.measure_start_ms(first).unwrap_or(0.);
            let end = timeline
                .measure_start_ms(last + 1)
                .unwrap_or_else(|| timeline.duration_ms());
            start <= at && at < end
        });
        if !inside {
            player.seek_measure(first);
        }
    }

    /// A key from MIDI in, arrived at `at`; true when it released the chord the clock waited for.
    pub fn key(&mut self, key: u8, velocity: u8, at: Instant, clock: &Clock) -> bool {
        self.waiter.key_at(key, velocity, at, clock)
    }
}

/// What the loop control says: its measures as the score names them (`names`, in the order of
/// play; empty when the piece has no score), and where they are in the order of play when a
/// repeat makes the names differ from it.
pub fn loop_text(
    looped: Option<(usize, usize)>,
    half: Option<usize>,
    names: &[impl AsRef<str>],
) -> String {
    let name = |index: usize| match names.get(index) {
        Some(name) => name.as_ref().to_owned(),
        None => (index + 1).to_string(),
    };
    match (looped, half) {
        (Some((a, b)), _) => {
            let shown = format!("Loop {}\u{2013}{}", name(a), name(b));
            let counted =
                names.len() > b && (name(a), name(b)) != ((a + 1).to_string(), (b + 1).to_string());
            if counted {
                format!("{shown} (measures {}\u{2013}{} as played)", a + 1, b + 1)
            } else {
                shown
            }
        }
        (None, Some(a)) => format!("Loop from {}: its end?", name(a)),
        (None, None) => "No loop".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::piano::{self, Hands, Renderer, Sink};
    use crate::timeline::Note;

    #[derive(Debug, PartialEq)]
    enum Call {
        On(u8, usize),
        Off(u8, usize),
        Release(usize),
    }

    /// Records what the computer plays, and at which frame of the whole run.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<Call>,
        frames: usize,
    }

    impl Sink for Recorder {
        fn note_on(&mut self, key: u8, _: u8) {
            self.calls.push(Call::On(key, self.frames));
        }
        fn note_off(&mut self, key: u8) {
            self.calls.push(Call::Off(key, self.frames));
        }
        fn release_all(&mut self) {
            self.calls.push(Call::Release(self.frames));
        }
        fn live_on(&mut self, _: u8, _: u8) {}
        fn live_off(&mut self, _: u8) {}
        fn live_pedal(&mut self, _: u8) {}
        fn render(&mut self, left: &mut [f32], _: &mut [f32]) {
            self.frames += left.len();
        }
    }

    /// 1000 frames a second: a frame is a millisecond.
    const RATE: u32 = 1000;

    fn note(key: u8, on_ms: f64, off_ms: f64, hand: Hand) -> Note {
        Note {
            key,
            velocity: 90,
            on_ms,
            off_ms,
            hand,
            part: 0,
            measure: 0,
            element: None,
        }
    }

    struct Rig {
        renderer: Renderer<Recorder>,
        clock: Clock,
        hands: Hands,
        chords: Arc<Chords>,
    }

    fn rig(notes: Vec<Note>, measures: Vec<f64>, duration_ms: f64) -> Rig {
        let timeline = Timeline::new(notes, measures, duration_ms);
        let chords = Arc::new(Chords::of(&timeline));
        let clock = Clock::with_marks(timeline.duration_ms(), &chords.marks(&timeline));
        let hands = Hands::both();
        let renderer = Renderer::new(
            Recorder::default(),
            piano::events(&timeline),
            clock.clone(),
            hands.clone(),
            RATE,
        );
        Rig {
            renderer,
            clock,
            hands,
            chords,
        }
    }

    impl Rig {
        fn run(&mut self, buffers: usize, frames: usize) {
            let (mut l, mut r) = (vec![0.; frames], vec![0.; frames]);
            for _ in 0..buffers {
                self.renderer.render(&mut l, &mut r);
            }
        }

        fn calls(&self) -> &[Call] {
            &self.renderer.sink().calls
        }

        fn position_ms(&self) -> u64 {
            self.clock.snapshot().position_us / 1000
        }

        /// What `practice` does for "the right hand, with the wait".
        fn right_hand_waits(&self) {
            self.hands.set(false, true);
            self.clock.set_wait(Wait::Right);
        }
    }

    /// The left hand on each half second, the right hand's chord (E and G) at 1 s and its C at 2 s.
    fn two_hands() -> Rig {
        rig(
            vec![
                note(48, 0., 400., Hand::Left),
                note(50, 500., 900., Hand::Left),
                note(52, 1000., 1400., Hand::Left),
                note(53, 1500., 1900., Hand::Left),
                note(64, 1000., 1500., Hand::Right),
                note(67, 1000., 1500., Hand::Right),
                note(72, 2000., 2400., Hand::Right),
            ],
            vec![0., 1000., 2000., 3000.],
            4000.,
        )
    }

    /// The issue's check: right hand with the wait, the clock holds until the right chord is
    /// struck, the left hand played by the computer.
    #[test]
    fn with_the_right_hand_waiting_the_clock_holds_until_its_chord_while_the_left_hand_plays() {
        let mut rig = two_hands();
        rig.right_hand_waits();
        let mut waiter = Waiter::new(rig.chords.clone());
        rig.clock.play();
        // A second and a half of sound: the clock stops at the chord, at 1 s.
        rig.run(15, 100);
        assert_eq!(rig.position_ms(), 1000);
        assert!(rig.clock.held());
        assert!(rig.clock.playing());
        // The left hand played up to the chord; its note on the chord waits with it.
        assert_eq!(
            rig.calls(),
            [
                Call::On(48, 0),
                Call::Off(48, 400),
                Call::On(50, 500),
                Call::Off(50, 900),
            ]
        );
        // The window sees no further than the chord either.
        assert!(rig.clock.position_ms() <= 1000.);

        // A wrong key, then one of the two: still waiting.
        assert!(!waiter.key(60, 90, &rig.clock));
        assert!(!waiter.key(64, 90, &rig.clock));
        rig.run(3, 100);
        assert_eq!(rig.position_ms(), 1000);
        assert_eq!(rig.calls().len(), 4);

        // The chord whole: the clock goes on, the left hand with it, from the next buffer.
        assert!(waiter.key(67, 90, &rig.clock));
        assert!(!rig.clock.held());
        rig.run(6, 100);
        assert_eq!(
            rig.calls()[4..],
            [Call::On(52, 1800), Call::Off(52, 2200), Call::On(53, 2300)]
        );
        assert_eq!(rig.position_ms(), 1600);
        // The player's notes are never the computer's.
        assert!(
            !rig.calls()
                .iter()
                .any(|call| matches!(call, Call::On(64 | 67 | 72, _) | Call::Off(64 | 67 | 72, _)))
        );
    }

    #[test]
    fn a_hand_taken_by_the_player_mid_note_is_let_go_by_the_computer_and_struck_no_more() {
        let mut rig = two_hands();
        rig.clock.play();
        // Listening: the computer strikes the chord.
        rig.run(12, 100);
        assert!(rig.calls().contains(&Call::On(64, 1000)));
        // The player takes the right hand while the chord sounds: its end is still sent.
        rig.hands.set(false, true);
        rig.run(10, 100);
        assert!(rig.calls().contains(&Call::Off(64, 1500)));
        assert!(rig.calls().contains(&Call::Off(67, 1500)));
        assert!(
            !rig.calls()
                .iter()
                .any(|call| matches!(call, Call::On(72, _)))
        );
    }

    #[test]
    fn a_note_of_the_computer_that_ends_on_the_chord_owed_does_not_ring_through_the_wait() {
        let mut rig = rig(
            vec![
                note(48, 500., 1000., Hand::Left),
                note(64, 1000., 1500., Hand::Right),
            ],
            vec![0.],
            2000.,
        );
        rig.right_hand_waits();
        rig.clock.play();
        rig.run(15, 100);
        assert!(rig.clock.held());
        // Its end is the chord's instant: played as the clock stands there, not after the wait.
        assert_eq!(rig.calls(), [Call::On(48, 500), Call::Off(48, 1000)]);
        // Ended once: the chord struck does not end it again.
        let mut waiter = Waiter::new(rig.chords.clone());
        assert!(waiter.key(64, 90, &rig.clock));
        rig.run(3, 100);
        assert_eq!(rig.calls().len(), 2);
    }

    #[test]
    fn a_pause_while_waiting_lets_the_computers_notes_go() {
        let mut rig = rig(
            vec![
                note(48, 0., 3000., Hand::Left),
                note(64, 1000., 1500., Hand::Right),
            ],
            vec![0.],
            4000.,
        );
        rig.right_hand_waits();
        rig.clock.play();
        rig.run(15, 100);
        assert!(rig.clock.held());
        rig.clock.pause();
        rig.run(1, 100);
        assert_eq!(rig.calls(), [Call::On(48, 0), Call::Release(1500)]);
        assert!(!rig.clock.held(), "a pause is not a wait");
    }

    #[test]
    fn a_key_held_from_before_is_struck_again_and_a_wrong_one_releases_nothing() {
        let mut rig = two_hands();
        rig.right_hand_waits();
        let mut waiter = Waiter::new(rig.chords.clone());
        rig.clock.play();
        // 64 struck long before the chord, and held.
        assert!(!waiter.key(64, 90, &rig.clock));
        rig.run(12, 100);
        assert!(rig.clock.held());
        assert!(!waiter.key(67, 90, &rig.clock), "64 was not struck for it");
        // 67 let go, 64 struck: 67 is not down any more.
        assert!(!waiter.key(67, 0, &rig.clock));
        assert!(!waiter.key(64, 90, &rig.clock));
        assert!(!waiter.key(65, 90, &rig.clock));
        assert!(rig.clock.held());
        assert!(waiter.key(67, 90, &rig.clock));
        assert!(!rig.clock.held());
    }

    #[test]
    fn a_chord_struck_a_little_early_counts_and_the_clock_does_not_stop() {
        let mut rig = two_hands();
        rig.right_hand_waits();
        let mut waiter = Waiter::new(rig.chords.clone());
        rig.clock.play();
        // 500 ms before the chord: too early, for nothing.
        rig.run(5, 100);
        assert!(!waiter.key(64, 90, &rig.clock));
        assert!(!waiter.key(64, 0, &rig.clock));
        // 150 ms before it (EARLY_MS is 200): it counts.
        rig.run(35, 10);
        assert_eq!(rig.position_ms(), 850);
        assert!(!waiter.key(64, 90, &rig.clock));
        assert!(waiter.key(67, 90, &rig.clock));
        rig.run(4, 100);
        assert_eq!(rig.position_ms(), 1250);
        assert!(rig.calls().contains(&Call::On(52, 1000)));
    }

    #[test]
    fn without_the_wait_or_listening_the_clock_never_stops() {
        let mut rig = two_hands();
        rig.hands.set(false, true);
        rig.clock.play();
        rig.run(25, 100);
        assert_eq!(rig.position_ms(), 2500);
        assert!(rig.clock.owed().is_none());
    }

    /// Ten measures of a second, each with its own note from its first instant to its end.
    fn ten_measures() -> Rig {
        let measures: Vec<f64> = (0..10).map(|m| f64::from(m) * 1000.).collect();
        let notes = (0..10u8)
            .map(|m| {
                let on = f64::from(m) * 1000.;
                note(60 + m, on, on + 1000., Hand::Right)
            })
            .collect();
        rig(notes, measures, 10_000.)
    }

    /// The issue's check: a loop on measures 5 to 8 starts again at 5 with no gap, nothing of
    /// measure 9 heard, nothing struck twice, the notes of 8 let go at the seam.
    #[test]
    fn a_loop_on_measures_5_to_8_starts_again_at_5_without_a_gap() {
        let mut rig = ten_measures();
        rig.clock.set_loop(Some((4, 7)));
        // Measure 8, as the window puts it when the loop is made.
        rig.clock.seek_ms(7000.);
        rig.clock.play();
        // 300 frames a buffer: the seam (8 s) falls inside the fourth one, at frame 1000. The
        // first buffer follows the seek (a release with nothing sounding).
        rig.run(4, 300);
        assert_eq!(
            rig.calls(),
            [
                Call::Release(0),
                Call::On(67, 0),
                Call::Release(1000),
                Call::On(64, 1000),
            ]
        );
        assert_eq!(rig.position_ms(), 4200);
        assert_eq!(rig.renderer.sink().frames, 1200);

        // Round again: 5, 6, 7, 8, then 5 at the next seam, 4 s of sound later.
        rig.run(14, 300);
        assert_eq!(
            rig.calls()[4..],
            [
                Call::Off(64, 2000),
                Call::On(65, 2000),
                Call::Off(65, 3000),
                Call::On(66, 3000),
                Call::Off(66, 4000),
                Call::On(67, 4000),
                Call::Release(5000),
                Call::On(64, 5000),
            ]
        );
        assert_eq!(rig.position_ms(), 4400);
        assert!(rig.clock.playing());
    }

    /// The issue's check, as the piano plays it: Rework of measures 3 and 7 plays 3, 7, 3, 7 with
    /// no gap at either seam, nothing of what lies between heard, the notes of each let go at its
    /// seam, nothing struck twice.
    #[test]
    fn rework_of_measures_3_and_7_plays_one_then_the_other_without_a_gap() {
        let mut rig = ten_measures();
        rig.clock.set_spans(Spans::of([(2, 2), (6, 6)]));
        rig.clock.seek_ms(2000.);
        rig.clock.play();
        // 300 frames a buffer: the seams fall inside buffers, at frames 1000, 2000, 3000.
        rig.run(12, 300);
        assert_eq!(
            rig.calls(),
            [
                Call::Release(0),
                Call::On(62, 0),
                Call::Release(1000),
                Call::On(66, 1000),
                Call::Release(2000),
                Call::On(62, 2000),
                Call::Release(3000),
                Call::On(66, 3000),
            ]
        );
        assert_eq!(rig.renderer.sink().frames, 3600, "every frame rendered");
        assert_eq!(rig.position_ms(), 6600);
        assert!(rig.clock.playing());
    }

    #[test]
    fn a_chord_owed_at_the_loops_start_is_owed_again_at_each_pass() {
        let mut rig = ten_measures();
        rig.clock.set_loop(Some((4, 5)));
        rig.clock.set_wait(Wait::Right);
        rig.hands.set(false, false);
        let mut waiter = Waiter::new(rig.chords.clone());
        rig.clock.seek_ms(4000.);
        rig.clock.play();
        rig.run(1, 100);
        assert_eq!(rig.position_ms(), 4000);
        assert!(waiter.key(64, 90, &rig.clock));
        waiter.key(64, 0, &rig.clock);
        rig.run(10, 100);
        assert_eq!(rig.position_ms(), 5000, "held by the chord of measure 6");
        assert!(waiter.key(65, 90, &rig.clock));
        waiter.key(65, 0, &rig.clock);
        rig.run(20, 100);
        // Round to 5 again, where the same chord is owed again.
        assert_eq!(rig.position_ms(), 4000);
        assert!(rig.clock.held());
    }

    #[test]
    fn the_first_chord_of_the_next_pass_may_be_struck_just_before_the_seam() {
        let mut rig = ten_measures();
        rig.clock.set_loop(Some((4, 4)));
        rig.clock.set_wait(Wait::Right);
        let mut waiter = Waiter::new(rig.chords.clone());
        rig.clock.seek_ms(4000.);
        rig.clock.play();
        assert!(waiter.key(64, 90, &rig.clock));
        waiter.key(64, 0, &rig.clock);
        rig.run(9, 100);
        assert_eq!(rig.position_ms(), 4900);
        // 100 ms before the seam, the chord at A's start of the next pass.
        assert!(waiter.key(64, 90, &rig.clock));
        rig.run(3, 100);
        assert_eq!(rig.position_ms(), 4200, "no stop at the seam");
    }

    #[test]
    fn chords_are_the_notes_of_a_hand_that_start_together() {
        let timeline = Timeline::new(
            vec![
                note(60, 0., 500., Hand::Right),
                note(64, 10., 500., Hand::Right),
                note(48, 0., 500., Hand::Left),
                note(67, 500., 900., Hand::Right),
                note(67, 505., 900., Hand::Right),
            ],
            vec![0.],
            1000.,
        );
        let chords = Chords::of(&timeline);
        let keys = |wait, index| chords.get(wait, index).map(|c| c.keys.clone());
        assert_eq!(keys(Wait::Right, 0), Some(vec![60, 64]));
        assert_eq!(keys(Wait::Right, 1), Some(vec![67]));
        assert_eq!(keys(Wait::Right, 2), None);
        assert_eq!(keys(Wait::Left, 0), Some(vec![48]));
        assert_eq!(keys(Wait::Both, 0), Some(vec![48, 60, 64]));
        assert_eq!(keys(Wait::Off, 0), None);
        let marks = chords.marks(&timeline);
        assert_eq!(marks.gates_ms[0], [0., 500.]);
        assert_eq!(marks.measures_ms, [0.]);
    }

    #[test]
    fn the_choice_says_what_the_computer_plays_and_what_is_waited_for() {
        assert_eq!(Playing::Listen.computer(), (true, true));
        assert_eq!(Playing::Right.computer(), (false, true));
        assert_eq!(Playing::Both.computer(), (false, false));
        let choice = Choice {
            playing: Playing::Left,
            wait: true,
        };
        assert_eq!(choice.wait_for(), Wait::Left);
        let listening = Choice {
            playing: Playing::Listen,
            wait: true,
        };
        assert_eq!(listening.wait_for(), Wait::Off);
        assert_eq!(Playing::Both.next(), Playing::Listen);
    }

    #[test]
    fn the_wait_needs_a_hand_played_and_a_keyboard() {
        let choice = Choice {
            playing: Playing::Right,
            wait: true,
        };
        let mut practice = Practice::new(choice, Arc::default());
        // No keyboard listened to: nothing would release it, the clock does not wait.
        assert!(!practice.can_wait());
        assert_eq!(practice.wait_now(), Wait::Off);
        practice.keyboard = true;
        assert!(practice.can_wait());
        assert_eq!(practice.wait_now(), Wait::Right);
        practice.choice.playing = Playing::Listen;
        assert!(!practice.can_wait());
        assert_eq!(practice.wait_now(), Wait::Off);
        // Carried to the next player, the keyboard is still known.
        practice.choice.playing = Playing::Left;
        assert_eq!(practice.carried(Arc::default()).wait_now(), Wait::Left);
    }

    #[test]
    fn a_choice_kept_with_missing_or_unknown_words_reads_as_its_defaults() {
        let read = |json: &str| serde_json::from_str::<Choice>(json).unwrap();
        assert_eq!(read("{}"), Choice::default());
        assert_eq!(
            read(r#"{"playing": "left"}"#),
            Choice {
                playing: Playing::Left,
                wait: false,
            }
        );
        assert_eq!(
            read(r#"{"playing": "feet", "wait": true}"#),
            Choice {
                playing: Playing::Listen,
                wait: true,
            }
        );
        assert_eq!(read(r#"{"playing": 3}"#).playing, Playing::Listen);
        let kept = serde_json::to_string(&Choice {
            playing: Playing::Both,
            wait: true,
        })
        .unwrap();
        assert_eq!(kept, r#"{"playing":"both","wait":true}"#);
    }

    #[test]
    fn the_loop_is_named_as_the_score_names_its_measures() {
        let plain: [&str; 0] = [];
        assert_eq!(loop_text(Some((4, 7)), None, &plain), "Loop 5\u{2013}8");
        assert_eq!(loop_text(None, Some(4), &plain), "Loop from 5: its end?");
        assert_eq!(loop_text(None, None, &plain), "No loop");
        // A repeat of measures 1 to 4: the second time through, 5 to 8 as played are 1 to 4.
        let names = ["1", "2", "3", "4", "1", "2", "3", "4", "5"];
        assert_eq!(
            loop_text(Some((4, 7)), None, &names),
            "Loop 1\u{2013}4 (measures 5\u{2013}8 as played)"
        );
        assert_eq!(loop_text(Some((0, 3)), None, &names), "Loop 1\u{2013}4");
    }
}
