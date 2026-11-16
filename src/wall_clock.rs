//! The clock moved by the wall clock while no sound moves it: before the built-in piano is ready
//! (the SoundFont downloading), or with no audio output at all. The cursor and the falling notes
//! then go on in silence, at the run's speed, and the audio callback takes over from wherever the
//! clock is once the piano sounds.

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
    /// Moves `clock` on by the time since the last tick, at its speed, when it plays and `drive`
    /// says nothing else moves it. A tick that does not drive forgets the last one: a pause, or
    /// the time the piano drove it, is never counted afterwards.
    pub fn tick(&mut self, clock: &Clock, drive: bool) {
        self.tick_at(clock, drive, Instant::now());
    }

    fn tick_at(&mut self, clock: &Clock, drive: bool, now: Instant) {
        let snapshot = clock.snapshot();
        if !drive || !snapshot.playing {
            self.last = None;
            self.carry = 0.;
            return;
        }
        if let Some(last) = self.last {
            let us =
                now.saturating_duration_since(last).as_secs_f64() * 1e6 * snapshot.speed.factor()
                    + self.carry;
            let whole = us.floor();
            self.carry = us - whole;
            clock.advance(&snapshot, whole as u64);
        }
        self.last = Some(now);
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
            wall.tick_at(&clock, true, start + Duration::from_millis(10 * tick));
        }
        // One second of the wall, half a second of the piece.
        assert_eq!(position(&clock), 500_000);
    }

    #[test]
    fn it_does_not_move_a_paused_clock_nor_one_the_sound_drives() {
        let clock = Clock::new(10_000.);
        let mut wall = WallClock::default();
        let start = Instant::now();
        wall.tick_at(&clock, true, start);
        wall.tick_at(&clock, true, start + Duration::from_secs(1));
        assert_eq!(position(&clock), 0, "paused");

        clock.play();
        wall.tick_at(&clock, false, start + Duration::from_secs(2));
        wall.tick_at(&clock, false, start + Duration::from_secs(3));
        assert_eq!(position(&clock), 0, "the piano drives it");

        // Driving again counts from its first tick, not from the last one that did not drive.
        wall.tick_at(&clock, true, start + Duration::from_secs(4));
        wall.tick_at(&clock, true, start + Duration::from_millis(4_250));
        assert_eq!(position(&clock), 250_000);
    }

    #[test]
    fn a_seek_between_two_ticks_is_kept() {
        let clock = Clock::new(10_000.);
        clock.play();
        let mut wall = WallClock::default();
        let start = Instant::now();
        wall.tick_at(&clock, true, start);
        clock.seek_ms(5_000.);
        wall.tick_at(&clock, true, start + Duration::from_millis(100));
        assert_eq!(position(&clock), 5_100_000);
    }
}
