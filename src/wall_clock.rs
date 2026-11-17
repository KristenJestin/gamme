//! The clock moved by the wall clock while no sound moves it: before the built-in piano is ready
//! (the SoundFont downloading), or with no audio output at all. The cursor and the falling notes
//! then go on in silence, at the run's speed.
//!
//! The hand-over to the audio callback is the clock's own (`clock.rs`): the callback claims the
//! clock before its first buffer, and from then on the wall clock's advance fails, in the same
//! atomic operation that would have moved it. The two never move the clock together.

use std::time::Instant;

use crate::clock::Clock;

#[derive(Default)]
pub struct WallClock {
    /// The last tick that moved the clock: none while it is not driving.
    last: Option<Instant>,
    /// What is left, in microseconds, of the ticks so far once their whole microseconds went to
    /// the clock.
    carry: f64,
}

impl WallClock {
    /// Moves `clock` on by the time since the last tick, at its speed, while it plays and no
    /// audio output drives it. A tick that does not drive forgets the last one: a pause, or the
    /// time the piano drove it, is never counted afterwards.
    pub fn tick(&mut self, clock: &Clock) {
        self.tick_at(clock, Instant::now());
    }

    /// [`WallClock::tick`] at `now`: the tests choose their instants.
    pub(crate) fn tick_at(&mut self, clock: &Clock, now: Instant) {
        let snapshot = clock.snapshot();
        if !snapshot.playing || clock.claimed() {
            self.forget();
            return;
        }
        if let Some(last) = self.last {
            let us =
                now.saturating_duration_since(last).as_secs_f64() * 1e6 * snapshot.speed.factor()
                    + self.carry;
            let whole = us.floor();
            if !clock.advance_unclaimed(&snapshot, whole as u64) {
                // The audio took the clock over, or a seek came between: from now on, if at all.
                self.forget();
                self.last = Some(now);
                return;
            }
            self.carry = us - whole;
        }
        self.last = Some(now);
    }

    fn forget(&mut self) {
        self.last = None;
        self.carry = 0.;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::clock::Speed;

    fn position(clock: &Clock) -> u64 {
        clock.snapshot().position_us
    }

    #[test]
    fn a_playing_clock_moves_with_the_time_at_its_speed() {
        let clock = Clock::new(10_000.);
        clock.set_speed(Speed::new(50));
        clock.play();
        let mut wall = WallClock::default();
        let start = Instant::now();
        for tick in 0..=100 {
            wall.tick_at(&clock, start + Duration::from_millis(10 * tick));
        }
        // One second of the wall, half a second of the piece.
        assert_eq!(position(&clock), 500_000);
    }

    #[test]
    fn it_does_not_move_a_paused_clock_nor_one_the_audio_drives() {
        let clock = Clock::new(10_000.);
        let mut wall = WallClock::default();
        let start = Instant::now();
        wall.tick_at(&clock, start);
        wall.tick_at(&clock, start + Duration::from_secs(1));
        assert_eq!(position(&clock), 0, "paused");

        clock.play();
        clock.claim();
        wall.tick_at(&clock, start + Duration::from_secs(2));
        wall.tick_at(&clock, start + Duration::from_secs(3));
        assert_eq!(position(&clock), 0, "the piano drives it");

        // The output lost and made again: driving again counts from its first tick, not from the
        // last one that did not drive.
        clock.release();
        wall.tick_at(&clock, start + Duration::from_secs(4));
        wall.tick_at(&clock, start + Duration::from_millis(4_250));
        assert_eq!(position(&clock), 250_000);
    }

    /// The seam: the audio claims the clock between two ticks. The tick after it moves nothing,
    /// though it measured time of its own.
    #[test]
    fn the_tick_after_the_audio_claims_the_clock_moves_nothing() {
        let clock = Clock::new(10_000.);
        clock.play();
        let mut wall = WallClock::default();
        let start = Instant::now();
        wall.tick_at(&clock, start);
        wall.tick_at(&clock, start + Duration::from_millis(100));
        assert_eq!(position(&clock), 100_000);
        clock.claim();
        let audio = clock.snapshot();
        assert!(clock.advance(&audio, 5_000));
        wall.tick_at(&clock, start + Duration::from_millis(200));
        assert_eq!(position(&clock), 105_000, "only the audio's buffer");
    }

    #[test]
    fn a_seek_between_two_ticks_is_kept() {
        let clock = Clock::new(10_000.);
        clock.play();
        let mut wall = WallClock::default();
        let start = Instant::now();
        wall.tick_at(&clock, start);
        clock.seek_ms(5_000.);
        wall.tick_at(&clock, start + Duration::from_millis(100));
        assert_eq!(position(&clock), 5_100_000);
    }
}
