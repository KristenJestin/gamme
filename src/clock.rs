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

use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64,
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
    /// A stopped clock at 0, for a piece of `duration_ms`.
    pub fn new(duration_ms: f64) -> Self {
        let clock = Self(Arc::new(Shared {
            state: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            speed: AtomicU32::new(Speed::FULL.percent()),
            end_us: micros(duration_ms).min(POSITION_MASK),
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

    /// Moves the position on by `us` of the piece's own tempo (already scaled by the speed),
    /// unless a seek came since `from`. At the end of the piece the clock stops there and
    /// pauses. Returns whether the position moved. The audio side's.
    pub fn advance(&self, from: &Snapshot, us: u64) -> bool {
        self.advance_if(from, us, true)
    }

    /// [`Clock::advance`], for the wall clock: it fails once the audio side claimed the clock,
    /// in the same compare-and-swap that moves the position.
    pub fn advance_unclaimed(&self, from: &Snapshot, us: u64) -> bool {
        self.advance_if(from, us, false)
    }

    fn advance_if(&self, from: &Snapshot, us: u64, claimed_too: bool) -> bool {
        let end = self.0.end_us;
        let moved = self.0.state.try_update(AcqRel, Acquire, |state| {
            if seeks_of(state) as u32 != from.seeks {
                return None;
            }
            if !claimed_too && state & AUDIO_DRIVES != 0 {
                return None;
            }
            let position = ((state & POSITION_MASK) + us).min(end);
            Some(state & !POSITION_MASK | position)
        });
        let Ok(before) = moved else { return false };
        self.0.stamp_speed.store(from.speed.percent(), Relaxed);
        self.stamp_now();
        if (before & POSITION_MASK) + us >= end {
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

    #[test]
    fn the_counter_of_seeks_wraps_under_the_claim() {
        let state = AUDIO_DRIVES | SEEKS_MASK << POSITION_BITS | 5;
        assert_eq!(seeks_of(state), SEEKS_MASK);
        assert_eq!(seeks_of(state).wrapping_add(1) & SEEKS_MASK, 0);
    }
}
