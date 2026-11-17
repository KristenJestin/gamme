//! The play position of a piece, shared between the window and the audio thread.
//!
//! The clock is a few atomics: nothing in it allocates, locks or waits, so the audio callback can
//! use it. The window drives it (`play`, `pause`, `seek_ms`, `set_speed`) and reads it
//! (`position_ms`, `measure`); whoever produces the sound advances it by what it renders
//! (`snapshot` then `advance`; a MIDI-out piano, which renders nothing, advances it by the wall
//! clock the same way).
//!
//! Positions are milliseconds of the piece **at its own tempo** (`timeline.rs`), whatever the
//! speed: at 50 % the position advances by 500 ms per real second.
//!
//! The position and a counter of seeks live in one atomic. The audio side reads both, renders,
//! then advances only if no seek came between: a seek is never overwritten by a buffer that was
//! computed before it, and the counter tells the audio side to cut the notes that were sounding.
//!
//! Until the audio side runs, the window moves the clock by the wall clock (`wall_clock.rs`).
//! The hand-over is exclusive: the audio side claims the clock (`claim`) before each buffer, a
//! bit of the same atomic, and the wall clock's `advance_unclaimed` fails once it is set. The
//! one atomic decides between the two, so they never both move the clock. Any output that
//! advances the clock claims it so (a MIDI-out piano too); a new output being made releases it.
//!
//! Practice (`practice.rs`) is the clock's too, so that whoever moves it obeys it within the very
//! buffer, never a frame of the window late:
//!
//! - **Wait**: the clock is made with the moments the player's chords start, one list per hands
//!   he may play ([`Marks`]). With a list chosen ([`Clock::set_wait`]) the position never goes
//!   past the next chord the player owes: it stands there until the window says it was struck
//!   ([`Clock::owed`], [`Clock::release_chord`]). A chord is released for one pass of the position
//!   (one value of the counter of seeks), so that a seek or a loop owes it again.
//! - **Loop**: between measures A and B ([`Clock::set_loop`]), the position reaching B's end goes
//!   on from A's start in the same step, with what was left of it: no gap. The wrap counts as a
//!   seek, which the one that steps the clock knows of ([`Step::seeks`]): the audio side lets the
//!   piece's notes go at the seam itself, at its exact frame.
//!
//! The one that moves the clock asks first where a step goes ([`Clock::step`]), plays what it
//! goes through, then commits it ([`Clock::commit`]): what is played and where the clock goes
//! are the same, even when the window releases a chord in between.

use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU32, AtomicU64,
    Ordering::{AcqRel, Acquire, Relaxed, Release},
};
use std::time::Instant;

use crate::timeline::Timeline;

/// The position takes 40 bits, in microseconds (12 days); the counter of seeks the 23 above;
/// the top bit says the audio side drives the clock.
const POSITION_BITS: u32 = 40;
const POSITION_MASK: u64 = (1 << POSITION_BITS) - 1;
const SEEKS_MASK: u64 = (1 << 23) - 1;
const AUDIO_DRIVES: u64 = 1 << 63;

fn seeks_of(state: u64) -> u64 {
    (state >> POSITION_BITS) & SEEKS_MASK
}

/// How far the position read by the window runs ahead of the last one the audio thread
/// published, when buffers are late. It keeps a cursor smooth between two buffers (5–40 ms) and
/// stops it from running away when the audio stalls.
const EXTRAPOLATION_LIMIT_NS: u64 = 100_000_000;

/// The speed of a run, from 25 % to 100 % of the piece's own tempo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Speed(u8);

impl Speed {
    pub const MIN: Speed = Speed(25);
    pub const FULL: Speed = Speed(100);

    /// `percent`, held between 25 and 100.
    pub fn new(percent: u32) -> Self {
        Speed(percent.clamp(u32::from(Self::MIN.0), u32::from(Self::FULL.0)) as u8)
    }

    pub fn percent(self) -> u32 {
        u32::from(self.0)
    }

    pub fn factor(self) -> f64 {
        f64::from(self.0) / 100.
    }
}

/// Whose chords the clock waits for: the hands the player plays, or nobody's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Off,
    Right,
    Left,
    Both,
}

impl Wait {
    const ALL: [Wait; 4] = [Wait::Off, Wait::Right, Wait::Left, Wait::Both];

    /// Its list in [`Marks::gates_ms`].
    pub fn list(self) -> Option<usize> {
        match self {
            Wait::Off => None,
            Wait::Right => Some(0),
            Wait::Left => Some(1),
            Wait::Both => Some(2),
        }
    }
}

/// What a clock knows of its piece besides its length: where each measure starts, in the order
/// of play, and where each chord the player may owe starts, for each [`Wait`] but `Off` (right,
/// left, both hands), in ms of the piece, sorted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    pub measures_ms: Vec<f64>,
    pub gates_ms: [Vec<f64>; 3],
}

/// The chords of one list: where each starts, and the pass of the position each was released in
/// (the counter of seeks plus one; 0 for none).
struct Gates {
    onsets: Box<[u64]>,
    released: Box<[AtomicU32]>,
}

impl Gates {
    fn new(onsets_ms: &[f64]) -> Self {
        let mut onsets: Vec<u64> = onsets_ms.iter().map(|&ms| micros(ms)).collect();
        onsets.sort_unstable();
        onsets.dedup();
        let released = onsets.iter().map(|_| AtomicU32::new(0)).collect();
        Self {
            onsets: onsets.into(),
            released,
        }
    }

    /// The first chord at or after `from` and before `below` not released in pass `pass`. No
    /// lock, no allocation: the audio side asks it at each buffer.
    fn owed(&self, from: u64, below: u64, pass: u32) -> Option<usize> {
        let mark = pass_mark(pass);
        let mut index = self.onsets.partition_point(|&onset| onset < from);
        while let Some(&onset) = self.onsets.get(index) {
            if onset >= below {
                return None;
            }
            if self.released[index].load(Acquire) != mark {
                return Some(index);
            }
            index += 1;
        }
        None
    }
}

/// What a released chord keeps of the pass it was released in: never 0, which is none.
fn pass_mark(pass: u32) -> u32 {
    (pass & SEEKS_MASK as u32) + 1
}

/// A chord the player owes, as [`Clock::owed`] finds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Owed {
    pub wait: Wait,
    /// Its place in its list of [`Marks::gates_ms`].
    pub index: usize,
    /// The pass of the position it is owed in: the next one when it lies past the seam of the
    /// loop.
    pub pass: u32,
    /// How far ahead of the position it is, in ms of the piece; 0 when the clock stands on it.
    pub ahead_ms: f64,
}

/// Where a step of the clock goes, as [`Clock::step`] says before it is committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The time of the piece it goes through first, `[from, to)`, in µs.
    pub from_us: u64,
    pub to_us: u64,
    /// Past the end of the loop (`to_us`), what it goes through from the loop's start.
    pub wrapped: Option<(u64, u64)>,
    /// How much of the time asked it covers: less when a chord owed holds it.
    pub moved_us: u64,
    /// The counter of seeks once it is committed (one more when it wraps).
    pub seeks: u32,
}

impl Step {
    /// Where the position is once it is committed.
    pub fn position_us(&self) -> u64 {
        self.wrapped.map_or(self.to_us, |(_, to)| to)
    }
}

/// What the audio side reads before it renders a buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    /// How many seeks happened so far (wrapping): a change means the position jumped.
    pub seeks: u32,
    pub position_us: u64,
    pub playing: bool,
    pub speed: Speed,
}

/// A handle on the clock; clones share it.
#[derive(Clone)]
pub struct Clock(Arc<Shared>);

struct Shared {
    /// Seek counter above, position in microseconds below.
    state: AtomicU64,
    playing: AtomicBool,
    speed: AtomicU32,
    end_us: u64,
    /// Where each measure starts, in µs, in the order of play; the first is 0.
    measures: Box<[u64]>,
    /// The player's chords, by [`Wait::list`].
    gates: [Gates; 3],
    /// The [`Wait`] chosen, by its place in [`Wait::ALL`].
    wait: AtomicU8,
    /// The loop, as its first measure above and its last plus one below; 0 for none. One
    /// atomic, so that the audio side never reads the start of one loop and the end of another.
    looped: AtomicU64,
    origin: Instant,
    /// When the position was last set or advanced, in nanoseconds since `origin`: written after
    /// the position (Release), read before it (Acquire).
    stamp_ns: AtomicU64,
    /// The speed (percent) the position had advanced at then: the window runs ahead of it at
    /// that speed, so that a change of speed shows from the next buffer on, never as a jump.
    stamp_speed: AtomicU32,
}

fn micros(ms: f64) -> u64 {
    // NaN counts as 0.
    (ms * 1e3).max(0.).round() as u64
}

impl Clock {
    /// A stopped clock at 0, for a piece of `duration_ms` with no measures nor chords known.
    #[cfg(test)]
    pub fn new(duration_ms: f64) -> Self {
        Self::with_marks(duration_ms, &Marks::default())
    }

    /// A stopped clock at 0, for a piece of `duration_ms` whose measures and chords are `marks`.
    pub fn with_marks(duration_ms: f64, marks: &Marks) -> Self {
        let end_us = micros(duration_ms).min(POSITION_MASK);
        let mut measures: Vec<u64> = marks
            .measures_ms
            .iter()
            .map(|&ms| micros(ms).min(end_us))
            .collect();
        measures.sort_unstable();
        if measures.first() != Some(&0) {
            measures.insert(0, 0);
        }
        let clock = Self(Arc::new(Shared {
            state: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            speed: AtomicU32::new(Speed::FULL.percent()),
            end_us,
            measures: measures.into(),
            gates: std::array::from_fn(|list| Gates::new(&marks.gates_ms[list])),
            wait: AtomicU8::new(0),
            looped: AtomicU64::new(0),
            origin: Instant::now(),
            stamp_ns: AtomicU64::new(0),
            stamp_speed: AtomicU32::new(Speed::FULL.percent()),
        }));
        clock.stamp();
        clock
    }

    /// Notes that the position was just set (by the window): it stands at the current speed.
    fn stamp(&self) {
        self.0
            .stamp_speed
            .store(self.0.speed.load(Relaxed), Relaxed);
        self.stamp_now();
    }

    fn stamp_now(&self) {
        let ns = self.0.origin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.0.stamp_ns.store(ns, Release);
    }

    // ---- The window drives it.

    /// Starts, or resumes; at the end of the piece, from its start.
    pub fn play(&self) {
        let snapshot = self.snapshot();
        if snapshot.position_us >= self.0.end_us {
            self.seek_ms(0.);
        }
        self.0.playing.store(true, Relaxed);
        self.stamp();
    }

    pub fn pause(&self) {
        self.0.playing.store(false, Relaxed);
    }

    pub fn playing(&self) -> bool {
        self.0.playing.load(Relaxed)
    }

    pub fn speed(&self) -> Speed {
        Speed::new(self.0.speed.load(Relaxed))
    }

    /// Takes effect with the next buffer the audio side renders.
    pub fn set_speed(&self, speed: Speed) {
        self.0.speed.store(speed.percent(), Relaxed);
    }

    /// Moves to `ms`, held between 0 and the end of the piece.
    pub fn seek_ms(&self, ms: f64) {
        let target = micros(ms).min(self.0.end_us);
        let _ = self.0.state.try_update(AcqRel, Acquire, |state| {
            let seeks = seeks_of(state).wrapping_add(1) & SEEKS_MASK;
            Some(state & AUDIO_DRIVES | seeks << POSITION_BITS | target)
        });
        self.stamp();
    }

    /// Moves to the start of measure `index` (0 is the first); false when the piece has no such
    /// measure.
    pub fn seek_measure(&self, timeline: &Timeline, index: usize) -> bool {
        match timeline.measure_start_ms(index) {
            Some(ms) => {
                self.seek_ms(ms);
                true
            }
            None => false,
        }
    }

    /// Waits for the chords of the hands `wait` names from the next step on; `Off` waits for
    /// none.
    pub fn set_wait(&self, wait: Wait) {
        let index = Wait::ALL.iter().position(|&w| w == wait).unwrap_or(0);
        self.0.wait.store(index as u8, Release);
    }

    pub fn wait(&self) -> Wait {
        Wait::ALL
            .get(usize::from(self.0.wait.load(Acquire)))
            .copied()
            .unwrap_or(Wait::Off)
    }

    /// Plays measures `first` to `last` (0 is the first, in the order of play, both included)
    /// again and again; none plays on. A measure the clock does not know is held to the last
    /// one. It does not move the position: the window seeks to A when it stands outside.
    pub fn set_loop(&self, measures: Option<(usize, usize)>) {
        let count = self.0.measures.len();
        let packed = measures.map_or(0, |(first, last)| {
            let (first, last) = (first.min(last), first.max(last));
            let first = first.min(count - 1) as u64;
            let end = (last + 1).min(count) as u64;
            first << 32 | end
        });
        self.0.looped.store(packed, Release);
    }

    /// The loop's first and last measures, when there is one.
    pub fn looped(&self) -> Option<(usize, usize)> {
        let packed = self.0.looped.load(Acquire);
        (packed != 0).then(|| ((packed >> 32) as usize, (packed as u32 as usize) - 1))
    }

    /// The loop's start and end, in µs.
    fn loop_us(&self) -> Option<(u64, u64)> {
        let (first, last) = self.looped()?;
        let start = *self.0.measures.get(first)?;
        let end = self
            .0
            .measures
            .get(last + 1)
            .copied()
            .unwrap_or(self.0.end_us);
        (start < end).then_some((start, end))
    }

    /// Where the position stops going on from `position`, the chords aside: the end of the loop
    /// when it is before it, the end of the piece otherwise.
    fn limit_us(&self, position: u64, looped: Option<(u64, u64)>) -> u64 {
        match looped {
            Some((_, end)) if position < end => end,
            _ => self.0.end_us,
        }
    }

    /// The list of chords waited for now.
    fn gates(&self) -> Option<&Gates> {
        Some(&self.0.gates[self.wait().list()?])
    }

    /// Where the chord owed next stands, from `position` and before `below`, in pass `pass`.
    fn hold_us(&self, position: u64, below: u64, pass: u32) -> Option<u64> {
        let gates = self.gates()?;
        let index = gates.owed(position, below, pass)?;
        Some(gates.onsets[index])
    }

    /// The chord the player owes now: the one the clock stands on, or the next it will. Past the
    /// last one of the loop, the first of its next pass. None while nothing is waited for.
    pub fn owed(&self) -> Option<Owed> {
        let wait = self.wait();
        let gates = self.gates()?;
        let snapshot = self.snapshot();
        let position = snapshot.position_us;
        let looped = self.loop_us();
        let limit = self.limit_us(position, looped);
        // The position the window sees: a buffer ahead of the published one.
        let seen_us = (self.position_ms() * 1e3).max(position as f64);
        if let Some(index) = gates.owed(position, limit, snapshot.seeks) {
            return Some(Owed {
                wait,
                index,
                pass: snapshot.seeks,
                ahead_ms: (gates.onsets[index] as f64 - seen_us).max(0.) / 1e3,
            });
        }
        let (start, end) = looped.filter(|&(_, end)| position < end)?;
        let pass = snapshot.seeks.wrapping_add(1) & SEEKS_MASK as u32;
        let index = gates.owed(start, end, pass)?;
        Some(Owed {
            wait,
            index,
            pass,
            ahead_ms: ((end as f64 - seen_us).max(0.) + (gates.onsets[index] - start) as f64) / 1e3,
        })
    }

    /// The player struck `owed`: the clock goes on past it, in its pass.
    pub fn release_chord(&self, owed: &Owed) {
        let Some(list) = owed.wait.list() else {
            return;
        };
        if let Some(released) = self.0.gates[list].released.get(owed.index) {
            released.store(pass_mark(owed.pass), Release);
        }
    }

    /// Whether the clock plays and stands on a chord the player owes.
    pub fn held(&self) -> bool {
        let snapshot = self.snapshot();
        if !snapshot.playing {
            return false;
        }
        let position = snapshot.position_us;
        let limit = self.limit_us(position, self.loop_us());
        self.hold_us(position, limit, snapshot.seeks) == Some(position)
    }

    // ---- The window reads it.

    /// Where the piece stands, in ms of its own tempo: what the cursor and the falling notes
    /// are drawn from.
    pub fn position_ms(&self) -> f64 {
        let shared = &self.0;
        // The audio thread writes the position, then its stamp: read the stamp on both sides of
        // the position, and again if it moved (a few tries, then the last read does).
        let (mut position, mut stamp, mut speed) = (0, 0, Speed::FULL.percent());
        for _ in 0..4 {
            let before = shared.stamp_ns.load(Acquire);
            position = shared.state.load(Acquire) & POSITION_MASK;
            speed = shared.stamp_speed.load(Relaxed);
            stamp = shared.stamp_ns.load(Acquire);
            if before == stamp {
                break;
            }
        }
        let mut ms = position as f64 / 1e3;
        if self.playing() && position < shared.end_us {
            let now = shared.origin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            let ahead = now.saturating_sub(stamp).min(EXTRAPOLATION_LIMIT_NS);
            ms += ahead as f64 / 1e6 * Speed::new(speed).factor();
            // Never past what stops the position: a chord owed, the end of the loop.
            let seeks = seeks_of(shared.state.load(Acquire)) as u32;
            let limit = self.limit_us(position, self.loop_us());
            let stop = self.hold_us(position, limit, seeks).unwrap_or(limit);
            ms = ms.min(stop as f64 / 1e3);
        }
        ms.min(shared.end_us as f64 / 1e3)
    }

    /// The measure the position falls in (0 is the first).
    pub fn measure(&self, timeline: &Timeline) -> usize {
        timeline.measure_at(self.position_ms())
    }

    // ---- The audio side advances it.

    pub fn snapshot(&self) -> Snapshot {
        let state = self.0.state.load(Acquire);
        Snapshot {
            seeks: seeks_of(state) as u32,
            position_us: state & POSITION_MASK,
            playing: self.playing(),
            speed: self.speed(),
        }
    }

    /// Says the audio side drives the clock from now on: the wall clock no longer moves it.
    /// Called before each buffer is rendered, before its snapshot.
    pub fn claim(&self) {
        self.0.state.fetch_or(AUDIO_DRIVES, AcqRel);
    }

    /// No audio side drives the clock any more (a new output is being made): the wall clock may.
    pub fn release(&self) {
        self.0.state.fetch_and(!AUDIO_DRIVES, AcqRel);
    }

    /// Whether the audio side drives the clock.
    pub fn claimed(&self) -> bool {
        self.0.state.load(Acquire) & AUDIO_DRIVES != 0
    }

    /// Where moving on by `us` of the piece's own tempo (already scaled by the speed) from
    /// `from` goes: no further than the chord owed next, nor than the end of the piece; past the
    /// end of the loop, on from its start. Nothing moves until it is committed. No lock, no
    /// allocation.
    pub fn step(&self, from: &Snapshot, us: u64) -> Step {
        let position = from.position_us;
        let looped = self.loop_us();
        let limit = self.limit_us(position, looped);
        let target = position.saturating_add(us);
        let held = |at: u64, below: u64, pass: u32, to: u64| {
            self.hold_us(at, below, pass)
                .map_or(to, |hold| to.min(hold))
        };
        let first = held(position, limit, from.seeks, target.min(limit));
        let wrap = looped.filter(|&(_, end)| position < end && first == end && target >= end);
        match wrap {
            Some((start, end)) => {
                let length = end - start;
                let pass = from.seeks.wrapping_add(1) & SEEKS_MASK as u32;
                let to = held(start, end, pass, start + (target - end) % length);
                Step {
                    from_us: position,
                    to_us: end,
                    wrapped: Some((start, to)),
                    moved_us: (end - position) + (to - start),
                    seeks: pass,
                }
            }
            None => {
                let to = first.min(self.0.end_us);
                Step {
                    from_us: position,
                    to_us: to,
                    wrapped: None,
                    moved_us: to.saturating_sub(position),
                    seeks: from.seeks,
                }
            }
        }
    }

    /// Puts the position where `step` (asked from `from`) goes, unless a seek came since. At the
    /// end of the piece the clock stops there and pauses. Returns whether it was committed. The
    /// audio side's.
    pub fn commit(&self, from: &Snapshot, step: &Step) -> bool {
        self.commit_if(from, step, true)
    }

    /// Moves the position on by `us`, as [`Clock::step`] then [`Clock::commit`] do.
    #[cfg(test)]
    pub fn advance(&self, from: &Snapshot, us: u64) -> bool {
        self.commit_if(from, &self.step(from, us), true)
    }

    /// [`Clock::advance`], for the wall clock: it fails once the audio side claimed the clock,
    /// in the same compare-and-swap that moves the position.
    pub fn advance_unclaimed(&self, from: &Snapshot, us: u64) -> bool {
        self.commit_if(from, &self.step(from, us), false)
    }

    fn commit_if(&self, from: &Snapshot, step: &Step, claimed_too: bool) -> bool {
        let end = self.0.end_us;
        let position = step.position_us().min(end);
        let moved = self.0.state.try_update(AcqRel, Acquire, |state| {
            if seeks_of(state) as u32 != from.seeks {
                return None;
            }
            if !claimed_too && state & AUDIO_DRIVES != 0 {
                return None;
            }
            let seeks = u64::from(step.seeks) & SEEKS_MASK;
            Some(state & AUDIO_DRIVES | seeks << POSITION_BITS | position)
        });
        if moved.is_err() {
            return false;
        }
        self.0.stamp_speed.store(from.speed.percent(), Relaxed);
        self.stamp_now();
        if position >= end {
            self.0.playing.store(false, Relaxed);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// What a render does: advances by `elapsed` of real time at the current speed.
    trait Tick {
        fn tick(&self, elapsed: Duration);
    }

    impl Tick for Clock {
        fn tick(&self, elapsed: Duration) {
            let snapshot = self.snapshot();
            if snapshot.playing {
                let us = elapsed.as_secs_f64() * 1e6 * snapshot.speed.factor();
                self.advance(&snapshot, us.round() as u64);
            }
        }
    }

    fn seconds(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn speed_is_held_between_25_and_100_percent() {
        assert_eq!(Speed::new(0), Speed::MIN);
        assert_eq!(Speed::new(50).factor(), 0.5);
        assert_eq!(Speed::new(250), Speed::FULL);
    }

    #[test]
    fn a_stopped_clock_stays_where_it_is() {
        let clock = Clock::new(10_000.);
        clock.tick(seconds(1.));
        assert_eq!(clock.snapshot().position_us, 0);
    }

    #[test]
    fn the_position_advances_with_the_time_at_full_speed() {
        let clock = Clock::new(10_000.);
        clock.play();
        clock.tick(seconds(1.5));
        assert_eq!(clock.snapshot().position_us, 1_500_000);
    }

    #[test]
    fn at_half_speed_a_second_covers_half_a_second_of_the_piece() {
        let clock = Clock::new(10_000.);
        clock.set_speed(Speed::new(50));
        clock.play();
        for _ in 0..100 {
            clock.tick(seconds(0.01));
        }
        assert_eq!(clock.snapshot().position_us, 500_000);
        clock.set_speed(Speed::new(25));
        clock.tick(seconds(2.));
        assert_eq!(clock.snapshot().position_us, 1_000_000);
    }

    #[test]
    fn pausing_stops_it_and_playing_goes_on_from_there() {
        let clock = Clock::new(10_000.);
        clock.play();
        clock.tick(seconds(1.));
        clock.pause();
        clock.tick(seconds(1.));
        assert_eq!(clock.snapshot().position_us, 1_000_000);
        clock.play();
        clock.tick(seconds(1.));
        assert_eq!(clock.snapshot().position_us, 2_000_000);
    }

    #[test]
    fn a_seek_lands_where_asked_within_the_piece() {
        let clock = Clock::new(10_000.);
        clock.seek_ms(2500.);
        assert_eq!(clock.snapshot().position_us, 2_500_000);
        clock.seek_ms(99_000.);
        assert_eq!(clock.snapshot().position_us, 10_000_000);
        clock.seek_ms(-3.);
        assert_eq!(clock.snapshot().position_us, 0);
        clock.seek_ms(f64::NAN);
        assert_eq!(clock.snapshot().position_us, 0);
    }

    #[test]
    fn a_seek_by_measure_lands_on_the_measure_start() {
        let timeline = Timeline::new(vec![], vec![0., 2000., 4100.], 6000.);
        let clock = Clock::new(timeline.duration_ms());
        assert!(clock.seek_measure(&timeline, 2));
        assert_eq!(clock.snapshot().position_us, 4_100_000);
        assert_eq!(clock.measure(&timeline), 2);
        assert!(!clock.seek_measure(&timeline, 3));
        assert_eq!(clock.snapshot().position_us, 4_100_000);
    }

    #[test]
    fn a_buffer_computed_before_a_seek_does_not_overwrite_it() {
        let clock = Clock::new(10_000.);
        clock.play();
        let before = clock.snapshot();
        clock.seek_ms(4000.);
        assert!(!clock.advance(&before, 10_000));
        assert_eq!(clock.snapshot().position_us, 4_000_000);
        let after = clock.snapshot();
        assert_ne!(after.seeks, before.seeks);
        assert!(clock.advance(&after, 10_000));
        assert_eq!(clock.snapshot().position_us, 4_010_000);
    }

    #[test]
    fn the_clock_stops_at_the_end_and_plays_again_from_the_start() {
        let clock = Clock::new(1000.);
        clock.play();
        clock.tick(seconds(5.));
        assert_eq!(clock.snapshot().position_us, 1_000_000);
        assert!(!clock.playing());
        clock.play();
        assert!(clock.playing());
        assert_eq!(clock.snapshot().position_us, 0);
    }

    #[test]
    fn a_change_of_speed_does_not_make_the_position_the_window_reads_jump() {
        let clock = Clock::new(10_000.);
        clock.play();
        std::thread::sleep(Duration::from_millis(30));
        let before = clock.position_ms();
        assert!(before >= 29., "{before}");
        clock.set_speed(Speed::new(25));
        let after = clock.position_ms();
        assert!((after - before).abs() < 5., "{before} then {after}");
    }

    #[test]
    fn the_window_reads_the_position_set_by_the_other_side() {
        let clock = Clock::new(10_000.);
        let audio = clock.clone();
        clock.seek_ms(1234.);
        assert!((clock.position_ms() - 1234.).abs() < 1e-9);
        // Paused, nothing is added to what was published.
        let snapshot = audio.snapshot();
        audio.advance(&snapshot, 1000);
        assert!((clock.position_ms() - 1235.).abs() < 1e-9);
    }

    #[test]
    fn once_the_audio_claims_the_clock_the_wall_clock_never_moves_it() {
        let clock = Clock::new(10_000.);
        clock.play();
        let wall = clock.snapshot();
        assert!(clock.advance_unclaimed(&wall, 100_000));
        assert!(!clock.claimed());

        // The seam: the wall clock read the clock, the audio claims it before the wall clock
        // advances. The wall clock's advance fails; the audio's goes through.
        let wall = clock.snapshot();
        clock.claim();
        let audio = clock.snapshot();
        assert!(!clock.advance_unclaimed(&wall, 50_000));
        assert!(clock.advance(&audio, 10_000));
        assert_eq!(clock.snapshot().position_us, 110_000);

        // A seek keeps the claim, and the counter of seeks works as before.
        clock.seek_ms(2_000.);
        assert!(clock.claimed());
        let after = clock.snapshot();
        assert_ne!(after.seeks, audio.seeks);
        assert!(!clock.advance_unclaimed(&after, 1));
        assert_eq!(clock.snapshot().position_us, 2_000_000);

        // A new output being made gives the clock back to the wall clock.
        clock.release();
        let free = clock.snapshot();
        assert!(clock.advance_unclaimed(&free, 1_000));
        assert_eq!(clock.snapshot().position_us, 2_001_000);
    }

    fn four_measures(gates: [Vec<f64>; 3]) -> Clock {
        Clock::with_marks(
            4000.,
            &Marks {
                measures_ms: vec![0., 1000., 2000., 3000.],
                gates_ms: gates,
            },
        )
    }

    #[test]
    fn a_loop_goes_on_from_its_start_with_what_was_left_and_counts_as_a_seek() {
        let clock = four_measures(Default::default());
        clock.set_loop(Some((2, 1)));
        assert_eq!(clock.looped(), Some((1, 2)), "its ends in order");
        clock.seek_ms(2900.);
        clock.play();
        let before = clock.snapshot();
        let step = clock.step(&before, 300_000);
        assert_eq!((step.from_us, step.to_us), (2_900_000, 3_000_000));
        assert_eq!(step.wrapped, Some((1_000_000, 1_200_000)));
        assert_eq!(step.moved_us, 300_000);
        assert!(clock.commit(&before, &step));
        let after = clock.snapshot();
        assert_eq!(after.position_us, 1_200_000);
        assert_ne!(after.seeks, before.seeks);
        assert_eq!(after.seeks, step.seeks);
        assert!(clock.playing());

        // A loop on the last measure goes round at the end of the piece instead of stopping.
        clock.set_loop(Some((3, 3)));
        clock.seek_ms(3900.);
        clock.tick(seconds(0.2));
        assert_eq!(clock.snapshot().position_us, 3_100_000);
        assert!(clock.playing());
        // The window does not see past the seam while it waits for the next buffer.
        clock.set_loop(Some((1, 1)));
        clock.seek_ms(1999.);
        std::thread::sleep(Duration::from_millis(10));
        assert!(clock.position_ms() <= 2000., "{}", clock.position_ms());

        // Measures it does not know are held to its last one; none plays on.
        clock.set_loop(Some((7, 99)));
        assert_eq!(clock.looped(), Some((3, 3)));
        clock.set_loop(None);
        assert_eq!(clock.looped(), None);
        clock.seek_ms(3900.);
        clock.tick(seconds(0.2));
        assert!(!clock.playing(), "the end of the piece");
    }

    #[test]
    fn the_wall_clock_goes_round_the_loop_too() {
        let clock = four_measures([vec![1500.], vec![], vec![]]);
        clock.set_loop(Some((1, 1)));
        clock.seek_ms(1000.);
        clock.play();
        let before = clock.snapshot().seeks;
        let mut wall = crate::wall_clock::WallClock::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        // A tick each 100 ms: the first one starts it, the tenth reaches B's end, the eleventh
        // is 100 ms into the next pass.
        for tick in 0..=11 {
            wall.tick_at(&clock, at(100 * tick));
        }
        assert_eq!(clock.snapshot().position_us, 1_100_000);
        assert_ne!(clock.snapshot().seeks, before, "the wrap counts as a seek");
        assert!(clock.playing());

        // The wait holds the wall clock as it does the audio.
        clock.set_wait(Wait::Right);
        for tick in 12..=20 {
            wall.tick_at(&clock, at(100 * tick));
        }
        assert_eq!(clock.snapshot().position_us, 1_500_000);
        assert!(clock.held());
    }

    #[test]
    fn a_chord_owed_holds_the_clock_until_it_is_released_and_a_seek_owes_it_again() {
        let clock = four_measures([vec![1000.], vec![], vec![1000.]]);
        clock.play();
        // No wait: it plays through.
        clock.tick(seconds(1.5));
        assert_eq!(clock.snapshot().position_us, 1_500_000);

        clock.seek_ms(0.);
        clock.set_wait(Wait::Right);
        assert_eq!(clock.wait(), Wait::Right);
        clock.tick(seconds(0.9));
        let owed = clock.owed().unwrap();
        assert_eq!((owed.wait, owed.index), (Wait::Right, 0));
        assert!((owed.ahead_ms - 100.).abs() < 5., "{}", owed.ahead_ms);
        assert!(!clock.held());
        clock.tick(seconds(1.));
        assert_eq!(clock.snapshot().position_us, 1_000_000);
        assert!(clock.held());
        assert!(clock.playing(), "waiting is not a pause");
        std::thread::sleep(Duration::from_millis(10));
        assert!(clock.position_ms() <= 1000.);
        let owed = clock.owed().unwrap();
        assert_eq!(owed.ahead_ms, 0.);

        clock.release_chord(&owed);
        assert!(!clock.held());
        clock.tick(seconds(0.5));
        assert_eq!(clock.snapshot().position_us, 1_500_000);
        assert_eq!(clock.owed(), None, "no chord left");

        // A seek before it owes it again.
        clock.seek_ms(500.);
        clock.tick(seconds(1.));
        assert_eq!(clock.snapshot().position_us, 1_000_000);
        // The left hand has no chord there.
        clock.set_wait(Wait::Left);
        clock.tick(seconds(0.5));
        assert_eq!(clock.snapshot().position_us, 1_500_000);
        clock.set_wait(Wait::Off);
        assert_eq!(clock.owed(), None);
    }

    #[test]
    fn the_counter_of_seeks_wraps_under_the_claim() {
        let state = AUDIO_DRIVES | SEEKS_MASK << POSITION_BITS | 5;
        assert_eq!(seeks_of(state), SEEKS_MASK);
        assert_eq!(seeks_of(state).wrapping_add(1) & SEEKS_MASK, 0);
    }
}
