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
//! - **Loop**: between measures A and B (one span of [`Clock::set_spans`]), the position reaching B's end goes
//!   on from A's start in the same step, with what was left of it: no gap. The wrap counts as a
//!   seek, which the one that steps the clock knows of ([`Step::seeks`]): the audio side lets the
//!   piece's notes go at the seam itself, at its exact frame.
//! - **Spans**: a loop may be several spans of measures apart ([`Spans`], [`Clock::set_spans`]:
//!   Rework's measures 3 and 7). The end of one goes on from the start of the next, the last's
//!   from the first's, each seam a wrap as above. They live in two banks of atomics: the window
//!   writes the one not in use, then says it is (`loop_state`); the audio side reads the one in
//!   use, and again in the rare case a write began meanwhile, never waiting for one.
//!
//! The one that moves the clock asks first where a step goes ([`Clock::step`]), plays what it
//! goes through, then commits it ([`Clock::commit`]): what is played and where the clock goes
//! are the same, even when the window releases a chord in between.

use std::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU32, AtomicU64,
    Ordering::{AcqRel, Acquire, Relaxed, Release},
};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::timeline::Timeline;

/// The most spans a loop has: Rework's measures (`run_end.rs`), each apart.
pub const MAX_SPANS: usize = 4;

/// A loop: up to [`MAX_SPANS`] spans of measures, each its first and last (both included, in the
/// order of play), sorted, apart; none plays on. Spans that touch or overlap are one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spans {
    spans: [(usize, usize); MAX_SPANS],
    len: usize,
}

impl Spans {
    pub const NONE: Spans = Spans {
        spans: [(0, 0); MAX_SPANS],
        len: 0,
    };

    /// The spans of `measures` (first, last, in either order): sorted, joined where they touch,
    /// those past [`MAX_SPANS`] dropped. The window's: it allocates.
    pub fn of(measures: impl IntoIterator<Item = (usize, usize)>) -> Self {
        let mut all: Vec<(usize, usize)> = measures
            .into_iter()
            .map(|(a, b)| (a.min(b), a.max(b)))
            .collect();
        all.sort_unstable();
        let mut spans = Self::NONE;
        for (first, last) in all {
            match spans.len.checked_sub(1).map(|at| &mut spans.spans[at]) {
                Some(previous) if first <= previous.1.saturating_add(1) => {
                    previous.1 = previous.1.max(last);
                }
                _ if spans.len == MAX_SPANS => break,
                _ => {
                    spans.spans[spans.len] = (first, last);
                    spans.len += 1;
                }
            }
        }
        spans
    }

    pub fn as_slice(&self) -> &[(usize, usize)] {
        &self.spans[..self.len]
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The loop's spans as the clock plays them, in µs: each start and end, sorted, none empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Loop {
    spans: [(u64, u64); MAX_SPANS],
    len: usize,
}

impl Loop {
    fn get(&self, span: usize) -> Option<(u64, u64)> {
        self.spans[..self.len].get(span).copied()
    }

    /// The span a position plays on to the end of: the first that ends after it.
    fn span_of(&self, position: u64) -> Option<usize> {
        self.spans[..self.len]
            .iter()
            .position(|&(_, end)| position < end)
    }

    /// The span that goes on from the end of `span`: the next, the first after the last.
    fn after(&self, span: usize) -> Option<(u64, u64)> {
        self.get((span + 1) % self.len.max(1))
    }
}

/// The bank of spans in use at `state`'s generation (see [`Shared::loop_state`]).
fn bank_of(state: u64) -> usize {
    ((state >> 8) / 2 % 2) as usize
}

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

/// Whether the counter of seeks went from `from` to `to` by one wrap of the loop and nothing else
/// (a seek adds two).
pub fn one_wrap(from: u32, to: u32) -> bool {
    to == from.wrapping_add(1) & SEEKS_MASK as u32
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
    /// Past the end of a span of the loop (`to_us`), what it goes through from the next one's
    /// start.
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
    /// The loop's spans, each as its first measure above and its last plus one below, in two
    /// banks: the one in use, and the one the next loop is written in.
    banks: [[AtomicU64; MAX_SPANS]; 2],
    /// A generation above, the number of spans of the bank in use below (8 bits). The window
    /// writes a loop in three moves: one more on the generation (odd: the other bank is being
    /// written), that bank, then one more again with its count (even: it is in use). A reader
    /// reads the bank of the generation it saw ([`bank_of`]); it read one loop whole if the
    /// generation went up by one at most meanwhile, as the bank it read is written again only
    /// two writes later.
    loop_state: AtomicU64,
    /// The window's writes of the loop, one at a time; the audio side never takes it.
    loop_writer: Mutex<()>,
    origin: Instant,
    /// When the position was last set or advanced, in nanoseconds since `origin`: written after
    /// the position (Release), read before it (Acquire).
    stamp_ns: AtomicU64,
    /// The speed (percent) the position had advanced at then: the window runs ahead of it at
    /// that speed, so that a change of speed shows from the next buffer on, never as a jump.
    stamp_speed: AtomicU32,
    /// When the window last set the position (a seek, a play), as `stamp_ns`: a key is never
    /// placed back before it ([`Clock::position_at`]).
    set_ns: AtomicU64,
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
            banks: Default::default(),
            loop_state: AtomicU64::new(0),
            loop_writer: Mutex::new(()),
            origin: Instant::now(),
            stamp_ns: AtomicU64::new(0),
            stamp_speed: AtomicU32::new(Speed::FULL.percent()),
            set_ns: AtomicU64::new(0),
        }));
        clock.stamp();
        clock
    }

    /// Notes that the position was just set (by the window): it stands at the current speed.
    fn stamp(&self) {
        self.0
            .stamp_speed
            .store(self.0.speed.load(Relaxed), Relaxed);
        let ns = self.stamp_now();
        self.0.set_ns.store(ns, Release);
    }

    fn stamp_now(&self) -> u64 {
        let ns = self.0.origin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.0.stamp_ns.store(ns, Release);
        ns
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
            // Two: one more is the pass after the loop's next wrap, which a chord may be released
            // for already (struck just before the seam); a seek is never that pass.
            let seeks = seeks_of(state).wrapping_add(2) & SEEKS_MASK;
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
    #[cfg(test)]
    pub fn set_loop(&self, measures: Option<(usize, usize)>) {
        self.set_spans(Spans::of(measures));
    }

    /// Plays `spans` one after the other, again and again (see the module); none plays on.
    /// Measures it does not know are held to the last one. The window's, never the audio side's.
    pub fn set_spans(&self, spans: Spans) {
        let count = self.0.measures.len();
        let held = Spans::of(
            spans
                .as_slice()
                .iter()
                .map(|&(first, last)| (first.min(count - 1), last.min(count - 1))),
        );
        let _one = self
            .0
            .loop_writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = self.0.loop_state.load(Acquire);
        // Writes come one at a time: the generation is even here. Odd: the other bank is written.
        let writing = (state >> 8) | 1;
        self.0
            .loop_state
            .store(writing << 8 | (state & 0xff), Release);
        let bank = &self.0.banks[bank_of((writing + 1) << 8)];
        for (slot, &(first, last)) in bank.iter().zip(held.as_slice()) {
            slot.store((first as u64) << 32 | (last as u64 + 1), Release);
        }
        self.0
            .loop_state
            .store((writing + 1) << 8 | held.len as u64, Release);
    }

    /// The loop's spans of measures; none when there is no loop.
    pub fn looped(&self) -> Spans {
        // A write takes nanoseconds and they come at the player's pace: a second try is already
        // rare. Bounded all the same, the audio side never spins: past them, no loop for a read.
        for _ in 0..64 {
            let mut spans = Spans::NONE;
            let before = self.0.loop_state.load(Acquire);
            let bank = &self.0.banks[bank_of(before)];
            let len = ((before & 0xff) as usize).min(MAX_SPANS);
            for (span, slot) in spans.spans.iter_mut().zip(bank).take(len) {
                let packed = slot.load(Acquire);
                *span = (
                    (packed >> 32) as usize,
                    (packed as u32 as usize).saturating_sub(1),
                );
            }
            spans.len = len;
            let after = self.0.loop_state.load(Acquire);
            if (after >> 8).wrapping_sub(before >> 8) <= 1 {
                return spans;
            }
        }
        Spans::NONE
    }

    /// The loop's spans in µs, those of no length left out. No lock, no allocation.
    fn loop_us(&self) -> Loop {
        let mut looped = Loop::default();
        for &(first, last) in self.looped().as_slice() {
            let Some(&start) = self.0.measures.get(first) else {
                continue;
            };
            let end = self
                .0
                .measures
                .get(last + 1)
                .copied()
                .unwrap_or(self.0.end_us);
            if start < end {
                looped.spans[looped.len] = (start, end);
                looped.len += 1;
            }
        }
        looped
    }

    /// Where the position stops going on from `position`, the chords aside: the end of the span
    /// of the loop it plays on to, the end of the piece past the loop's last.
    fn limit_us(&self, position: u64, looped: &Loop) -> u64 {
        looped
            .span_of(position)
            .and_then(|span| looped.get(span))
            .map_or(self.0.end_us, |(_, end)| end)
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
    /// last one of the loop's span, the first of its next pass. None while nothing is waited for.
    #[cfg(test)]
    pub fn owed(&self) -> Option<Owed> {
        self.owed_seen(None)
    }

    /// [`Clock::owed`], how far ahead it is read from where the piece stood at `at` (a key's
    /// arrival, [`Clock::position_at`]): what the judge reads too, so that the two agree on a key
    /// struck early.
    pub fn owed_at(&self, at: Instant) -> Option<Owed> {
        self.owed_seen(Some(self.position_at(at).1))
    }

    fn owed_seen(&self, seen_ms: Option<f64>) -> Option<Owed> {
        let wait = self.wait();
        let gates = self.gates()?;
        let snapshot = self.snapshot();
        let position = snapshot.position_us;
        let looped = self.loop_us();
        let limit = self.limit_us(position, &looped);
        // The position the window sees (a buffer ahead of the published one), or the one asked.
        let seen_us = match seen_ms {
            Some(ms) => ms * 1e3,
            None => (self.position_ms() * 1e3).max(position as f64),
        };
        if let Some(index) = gates.owed(position, limit, snapshot.seeks) {
            return Some(Owed {
                wait,
                index,
                pass: snapshot.seeks,
                ahead_ms: (gates.onsets[index] as f64 - seen_us).max(0.) / 1e3,
            });
        }
        let (start, end) = looped.after(looped.span_of(position)?)?;
        let pass = snapshot.seeks.wrapping_add(1) & SEEKS_MASK as u32;
        let index = gates.owed(start, end, pass)?;
        Some(Owed {
            wait,
            index,
            pass,
            ahead_ms: ((limit as f64 - seen_us).max(0.) + (gates.onsets[index] - start) as f64)
                / 1e3,
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
        let limit = self.limit_us(position, &self.loop_us());
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
            let limit = self.limit_us(position, &self.loop_us());
            let stop = self.hold_us(position, limit, seeks).unwrap_or(limit);
            ms = ms.min(stop as f64 / 1e3);
        }
        ms.min(shared.end_us as f64 / 1e3)
    }

    /// Where the piece stood at `at` (a key's arrival), in ms of its own tempo, and the counter of
    /// seeks it is read against: the position published last, moved on or back by the real time
    /// between its stamp and `at`, at the speed it advanced at, never past what stops it, nor back
    /// before the window last set it (a seek, a play: before that was another line). Paused, the
    /// position itself.
    ///
    /// Read back across the loop's seam (a key struck just before it, read just after), it lies
    /// before the loop's start: on the same line as the pass before, which is what the judge
    /// (`judge.rs`) needs.
    pub fn position_at(&self, at: Instant) -> (u32, f64) {
        let shared = &self.0;
        let (mut state, mut stamp, mut speed, mut set) = (0, 0, Speed::FULL.percent(), 0);
        for _ in 0..4 {
            let before = shared.stamp_ns.load(Acquire);
            state = shared.state.load(Acquire);
            speed = shared.stamp_speed.load(Relaxed);
            set = shared.set_ns.load(Acquire);
            stamp = shared.stamp_ns.load(Acquire);
            if before == stamp {
                break;
            }
        }
        let seeks = seeks_of(state) as u32;
        let position = state & POSITION_MASK;
        let mut ms = position as f64 / 1e3;
        if self.playing() && position < shared.end_us {
            let at_ns = match at.checked_duration_since(shared.origin) {
                Some(since) => since.as_nanos().min(i64::MAX as u128) as i64,
                None => {
                    -(shared
                        .origin
                        .duration_since(at)
                        .as_nanos()
                        .min(i64::MAX as u128) as i64)
                }
            };
            let stamp = stamp.min(i64::MAX as u64) as i64;
            let set = set.min(i64::MAX as u64) as i64;
            let ahead_ns = (at_ns - stamp)
                .min(EXTRAPOLATION_LIMIT_NS as i64)
                .max(set.min(stamp) - stamp);
            ms = (ms + ahead_ns as f64 / 1e6 * Speed::new(speed).factor()).max(0.);
            let limit = self.limit_us(position, &self.loop_us());
            let stop = self.hold_us(position, limit, seeks).unwrap_or(limit);
            ms = ms.min(stop as f64 / 1e3);
        }
        (seeks, ms.min(shared.end_us as f64 / 1e3))
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
    /// end of a span of the loop, on from the start of the next (the first after the last).
    /// Nothing moves until it is committed. No lock, no allocation.
    pub fn step(&self, from: &Snapshot, us: u64) -> Step {
        let position = from.position_us;
        let looped = self.loop_us();
        let limit = self.limit_us(position, &looped);
        let target = position.saturating_add(us);
        let held = |at: u64, below: u64, pass: u32, to: u64| {
            self.hold_us(at, below, pass)
                .map_or(to, |hold| to.min(hold))
        };
        let first = held(position, limit, from.seeks, target.min(limit));
        let wrap = looped
            .span_of(position)
            .filter(|_| first == limit && target >= limit)
            .and_then(|span| looped.after(span));
        match wrap {
            Some((start, end)) => {
                // A step longer than the next span (one shorter than a buffer) goes round in it,
                // never past its end into what the loop leaves out.
                let length = end - start;
                let pass = from.seeks.wrapping_add(1) & SEEKS_MASK as u32;
                let to = held(start, end, pass, start + (target - limit) % length);
                Step {
                    from_us: position,
                    to_us: limit,
                    wrapped: Some((start, to)),
                    moved_us: (limit - position) + (to - start),
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
    fn a_key_is_placed_where_the_piece_stood_when_it_arrived() {
        // Instants read from the clock's own stamp: no real time between two reads counts.
        let stamped = |clock: &Clock, ms: i64| {
            let stamp = clock.0.origin + Duration::from_nanos(clock.0.stamp_ns.load(Acquire));
            let by = Duration::from_millis(ms.unsigned_abs());
            if ms < 0 { stamp - by } else { stamp + by }
        };
        let near = |got: f64, want: f64| assert!((got - want).abs() < 1e-6, "{got} for {want}");
        let clock = Clock::new(10_000.);
        // Paused: where the piece stands, whenever the key came; with the counter of seeks.
        clock.seek_ms(1500.);
        assert_eq!(
            clock.position_at(stamped(&clock, -300)),
            (clock.snapshot().seeks, 1500.)
        );
        // Real time behind the last buffer to go back into, from the clock's start.
        std::thread::sleep(Duration::from_millis(150));
        clock.play();
        clock.tick(seconds(1.));
        // As if the play were long ago: nothing bounds the way back.
        clock.0.set_ns.store(0, Release);
        near(clock.position_at(stamped(&clock, 0)).1, 2500.);
        near(clock.position_at(stamped(&clock, 40)).1, 2540.);
        // A key that arrived 100 ms before the last buffer: 100 ms back.
        near(clock.position_at(stamped(&clock, -100)).1, 2400.);
        // At half speed, 100 ms of real time are 50 of the piece.
        clock.set_speed(Speed::new(50));
        clock.tick(seconds(0.2));
        near(clock.position_at(stamped(&clock, -100)).1, 2550.);
        // Never past a chord owed.
        let marks = Marks {
            measures_ms: vec![],
            gates_ms: [vec![1000.], vec![], vec![1000.]],
        };
        let held = Clock::with_marks(10_000., &marks);
        held.set_wait(Wait::Right);
        held.play();
        held.tick(seconds(2.));
        assert_eq!(held.position_at(Instant::now()).1, 1000.);
    }

    #[test]
    fn a_key_is_never_placed_back_before_the_position_was_set() {
        let clock = Clock::new(10_000.);
        // Real time behind the last buffer, from the clock's start.
        std::thread::sleep(Duration::from_millis(150));
        clock.seek_ms(3000.);
        clock.play();
        // The seek and the play 30 ms before the last buffer, which moved the piece 30 ms on.
        let snapshot = clock.snapshot();
        clock.advance(&snapshot, 30_000);
        let stamp = clock.0.stamp_ns.load(Acquire);
        clock.0.set_ns.store(stamp - 30_000_000, Release);
        let at = clock.0.origin + Duration::from_nanos(stamp) - Duration::from_millis(100);
        let (_, ms) = clock.position_at(at);
        assert!((ms - 3000.).abs() < 1e-6, "{ms}");
    }

    #[test]
    fn how_early_a_key_is_for_the_chord_owed_is_read_when_it_arrived() {
        let marks = Marks {
            measures_ms: vec![],
            gates_ms: [vec![1000.], vec![], vec![1000.]],
        };
        let clock = Clock::with_marks(10_000., &marks);
        std::thread::sleep(Duration::from_millis(150));
        clock.set_wait(Wait::Right);
        clock.play();
        clock.tick(seconds(0.79));
        clock.0.set_ns.store(0, Release);
        let stamp = clock.0.origin + Duration::from_nanos(clock.0.stamp_ns.load(Acquire));
        let ahead = |at| clock.owed_at(at).unwrap().ahead_ms;
        assert!((ahead(stamp) - 210.).abs() < 1e-6);
        // Arrived 100 ms before the last buffer: 100 ms earlier, though read after it.
        assert!((ahead(stamp - Duration::from_millis(100)) - 310.).abs() < 1e-6);
    }

    #[test]
    fn one_wrap_is_one_more_on_the_counter_and_a_seek_two() {
        assert!(one_wrap(4, 5));
        assert!(!one_wrap(4, 6));
        assert!(!one_wrap(4, 4));
        assert!(one_wrap(SEEKS_MASK as u32, 0));
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
        assert_eq!(clock.looped().as_slice(), [(1, 2)], "its ends in order");
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
        assert_eq!(clock.looped().as_slice(), [(3, 3)]);
        clock.set_loop(None);
        assert!(clock.looped().is_empty());
        clock.seek_ms(3900.);
        clock.tick(seconds(0.2));
        assert!(!clock.playing(), "the end of the piece");
    }

    #[test]
    fn spans_are_sorted_joined_where_they_touch_and_held_to_the_most() {
        assert_eq!(Spans::of([(6, 6), (2, 2)]).as_slice(), [(2, 2), (6, 6)]);
        assert_eq!(
            Spans::of([(3, 2), (4, 4), (9, 7)]).as_slice(),
            [(2, 4), (7, 9)]
        );
        assert_eq!(Spans::of([(1, 5), (2, 3)]).as_slice(), [(1, 5)]);
        let many = Spans::of((0..10).map(|m| (m * 2, m * 2)));
        assert_eq!(many.as_slice().len(), MAX_SPANS);
        assert_eq!(
            many.as_slice()[MAX_SPANS - 1],
            (6, 6),
            "the first ones kept"
        );
        assert!(Spans::of(None).is_empty());
    }

    /// Measures 2 and 4 of five measures of a second (1 s to 2 s, 3 s to 4 s).
    fn two_spans(gates: [Vec<f64>; 3]) -> Clock {
        let clock = Clock::with_marks(
            5000.,
            &Marks {
                measures_ms: vec![0., 1000., 2000., 3000., 4000.],
                gates_ms: gates,
            },
        );
        clock.set_spans(Spans::of([(3, 3), (1, 1)]));
        clock
    }

    #[test]
    fn a_loop_of_two_spans_goes_from_the_end_of_each_to_the_start_of_the_next() {
        let clock = two_spans(Default::default());
        assert_eq!(clock.looped().as_slice(), [(1, 1), (3, 3)]);
        clock.seek_ms(1900.);
        clock.play();
        // From the first span's end on into the second's start, with what was left: no gap.
        let before = clock.snapshot();
        let step = clock.step(&before, 300_000);
        assert_eq!((step.from_us, step.to_us), (1_900_000, 2_000_000));
        assert_eq!(step.wrapped, Some((3_000_000, 3_200_000)));
        assert_eq!(step.moved_us, 300_000);
        assert_eq!(step.seeks, before.seeks + 1, "one wrap");
        assert!(clock.commit(&before, &step));
        // From the last span's end back to the first's.
        clock.tick(seconds(0.9));
        assert_eq!(clock.snapshot().position_us, 1_100_000);
        assert_eq!(clock.snapshot().seeks, before.seeks + 2);
        assert!(clock.playing());
        // The window never sees what the loop leaves out while it waits for the next buffer.
        clock.seek_ms(1999.);
        std::thread::sleep(Duration::from_millis(10));
        assert!(clock.position_ms() <= 2000., "{}", clock.position_ms());
        // Before the first span, it plays on to that span's end; between the two, to the second's;
        // after the last, to the end of the piece.
        clock.seek_ms(500.);
        clock.tick(seconds(1.6));
        assert_eq!(clock.snapshot().position_us, 3_100_000);
        clock.seek_ms(2500.);
        clock.tick(seconds(1.6));
        assert_eq!(clock.snapshot().position_us, 1_100_000);
        clock.seek_ms(4500.);
        clock.tick(seconds(1.));
        assert!(!clock.playing(), "the end of the piece");
    }

    #[test]
    fn the_loop_ending_with_the_piece_goes_on_into_its_next_span() {
        let clock = two_spans(Default::default());
        clock.set_spans(Spans::of([(1, 1), (4, 4)]));
        clock.seek_ms(4900.);
        clock.play();
        clock.tick(seconds(0.2));
        assert_eq!(clock.snapshot().position_us, 1_100_000);
        assert!(clock.playing());
    }

    #[test]
    fn past_the_last_chord_of_a_span_the_first_of_the_next_is_owed_in_the_next_pass() {
        let clock = two_spans([vec![1000., 2500., 3200.], vec![], vec![]]);
        clock.set_wait(Wait::Right);
        clock.seek_ms(1950.);
        clock.play();
        // Not the chord at 2.5 s, which the loop leaves out: the one at 3.2 s, after the seam.
        let owed = clock.owed().unwrap();
        assert_eq!(owed.index, 2);
        assert_eq!(owed.pass, clock.snapshot().seeks + 1);
        assert!((owed.ahead_ms - 250.).abs() < 5., "{}", owed.ahead_ms);
        // It holds the clock there once it wrapped.
        clock.tick(seconds(0.5));
        assert_eq!(clock.snapshot().position_us, 3_200_000);
        assert!(clock.held());
    }

    #[test]
    fn a_loop_is_always_read_whole_while_the_window_changes_it() {
        let clock = Clock::with_marks(
            10_000.,
            &Marks {
                measures_ms: (0..10).map(|m| f64::from(m) * 1000.).collect(),
                gates_ms: Default::default(),
            },
        );
        let a = Spans::of([(1, 1), (3, 4), (8, 8)]);
        let b = Spans::of([(2, 2), (6, 6)]);
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (clock, stop) = (clock.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut flip = false;
                while !stop.load(Relaxed) {
                    clock.set_spans(if flip { a } else { b });
                    clock.set_loop(None);
                    flip = !flip;
                }
            })
        };
        // Short: a few tens of milliseconds of a writer that never pauses.
        for _ in 0..50_000 {
            let seen = clock.looped();
            assert!(seen == a || seen == b || seen.is_empty(), "{seen:?}");
        }
        stop.store(true, Relaxed);
        writer.join().unwrap();
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
    fn a_chord_released_for_the_next_pass_is_owed_again_after_a_seek() {
        let clock = four_measures([vec![1000.], vec![], vec![]]);
        clock.set_loop(Some((1, 1)));
        clock.set_wait(Wait::Right);
        clock.seek_ms(1950.);
        clock.play();
        // Just before the seam, the chord at A's start of the next pass.
        let owed = clock.owed().unwrap();
        assert_eq!(owed.pass, clock.snapshot().seeks + 1);
        clock.release_chord(&owed);
        // A seek instead of the wrap: the chord is owed in the pass the seek starts.
        clock.seek_ms(1000.);
        clock.tick(seconds(0.1));
        assert_eq!(clock.snapshot().position_us, 1_000_000);
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
