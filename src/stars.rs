//! The stars a run earns (design §5.3), pure: what the end of a run shows (`run_end.rs`), and what
//! the progress of a piece keeps (#20) from the same [`Feat`].
//!
//! - **One** star at 50 % speed, **two** at 75 %, **three** at 100 % with the aid gone (the
//!   falling notes not shown: the score alone); each needs at least [`ACCURACY`] % accuracy with
//!   the wait off. Design §11 lists these as defaults to confirm.
//! - **The speed** a run counts at is the slowest it was played at: a run slowed for its hard
//!   passage earns the stars of that speed.
//! - **The wait off**: a run with any note judged under the wait earns none, as its accuracy
//!   leaves those notes out (`tally.rs`).
//! - **The whole piece**: a run earns stars only when each note the player owes was judged, struck
//!   or missed, once at least: a run started near the end, or a seek over a passage, earns none.
//!   The design says stars are per piece; this is how a run stands for the piece.
//! - **A MIDI piece** has no score, so its aid never goes: two stars at most (design §7).

use std::collections::HashSet;

use crate::judge::Judgement;
use crate::tally::Tally;

/// The accuracy each star needs, in percent.
pub const ACCURACY: u32 = 90;

/// The most stars a run earns.
pub const MOST: u8 = 3;

/// The speed each star needs, in percent: the first, the second, the third.
pub const SPEEDS: [u32; 3] = [50, 75, 100];

/// What a run did, as the stars read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feat {
    /// The slowest speed it was played at, in percent.
    pub speed: u32,
    /// The share of its timed notes perfect or good, in percent, rounded down; none when nothing
    /// was timed.
    pub accuracy: Option<u32>,
    /// Whether a note was judged under the wait.
    pub waited: bool,
    /// Whether every note owed was judged at least once.
    pub whole: bool,
    /// Whether the falling notes were shown while it played.
    pub aided: bool,
    /// Whether the piece has a score: a MIDI file has none.
    pub engraved: bool,
}

impl Feat {
    /// The feat of a run's `judgements`, of a piece whose pass owes `owed` notes; `aided` and
    /// `engraved` as [`Feat`] says.
    pub fn of(judgements: &[Judgement], owed: usize, aided: bool, engraved: bool) -> Self {
        let speed = judgements
            .iter()
            .map(|judgement| (judgement.speed * 100.).round() as u32)
            .min()
            .unwrap_or(0);
        let judged: HashSet<usize> = judgements.iter().filter_map(|j| j.note).collect();
        Self {
            speed,
            accuracy: Tally::of(judgements).standing().accuracy(),
            waited: judgements.iter().any(|judgement| !judgement.timed),
            whole: owed > 0 && judged.len() >= owed,
            aided,
            engraved,
        }
    }
}

/// The stars `feat` earns, 0 to [`MOST`].
pub fn earned(feat: &Feat) -> u8 {
    let accurate = feat.accuracy.is_some_and(|accuracy| accuracy >= ACCURACY);
    if !accurate || feat.waited || !feat.whole {
        return 0;
    }
    let by_speed = SPEEDS.iter().filter(|&&speed| feat.speed >= speed).count() as u8;
    let without_aid = !feat.aided && feat.engraved;
    if without_aid {
        by_speed
    } else {
        by_speed.min(MOST - 1)
    }
}

/// What the third star still asks of a run that earned two: none when it earned it, or when it
/// cannot (fewer than two, a MIDI piece).
pub fn third_asks(feat: &Feat) -> Option<&'static str> {
    (earned(feat) == MOST - 1 && feat.engraved && feat.speed >= SPEEDS[2] && feat.aided)
        .then_some("The third star is played on the score alone, without the falling notes.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::Grade;

    /// A run that earns everything: every note, perfect, at full speed, wait off, score alone.
    fn best() -> Feat {
        Feat {
            speed: 100,
            accuracy: Some(100),
            waited: false,
            whole: true,
            aided: false,
            engraved: true,
        }
    }

    #[test]
    fn one_star_at_half_speed_two_at_three_quarters_three_at_full_speed() {
        let at = |speed| earned(&Feat { speed, ..best() });
        assert_eq!(at(25), 0);
        assert_eq!(at(49), 0);
        assert_eq!(at(50), 1);
        assert_eq!(at(74), 1);
        assert_eq!(at(75), 2);
        assert_eq!(at(99), 2);
        assert_eq!(at(100), 3);
    }

    #[test]
    fn each_star_needs_ninety_percent_with_the_wait_off_over_the_whole_piece() {
        let with = |feat: Feat| earned(&feat);
        assert_eq!(
            with(Feat {
                accuracy: Some(90),
                ..best()
            }),
            3
        );
        assert_eq!(
            with(Feat {
                accuracy: Some(89),
                ..best()
            }),
            0
        );
        assert_eq!(
            with(Feat {
                accuracy: None,
                ..best()
            }),
            0
        );
        assert_eq!(
            with(Feat {
                waited: true,
                ..best()
            }),
            0
        );
        assert_eq!(
            with(Feat {
                whole: false,
                ..best()
            }),
            0
        );
        // "90 % with the wait off" at half speed: one star.
        assert_eq!(
            with(Feat {
                speed: 50,
                accuracy: Some(90),
                ..best()
            }),
            1
        );
    }

    #[test]
    fn the_third_star_needs_the_aid_gone_and_a_score() {
        let aided = Feat {
            aided: true,
            ..best()
        };
        assert_eq!(earned(&aided), 2);
        assert!(third_asks(&aided).is_some());
        let midi = Feat {
            aided: true,
            engraved: false,
            ..best()
        };
        assert_eq!(earned(&midi), 2, "a MIDI piece stops at two");
        assert_eq!(third_asks(&midi), None);
        assert_eq!(third_asks(&best()), None);
        assert_eq!(
            earned(&Feat {
                engraved: false,
                ..best()
            }),
            2
        );
    }

    fn judged(grade: Grade, note: Option<usize>, speed: f64, timed: bool) -> Judgement {
        Judgement {
            grade,
            key: 60,
            note,
            measure: 0,
            pass: 0,
            offset_ms: None,
            timed,
            speed,
            at_ms: note.map_or(0., |n| n as f64 * 500.),
        }
    }

    #[test]
    fn a_feat_is_read_from_the_judgements_of_a_run() {
        let run: Vec<Judgement> = (0..10)
            .map(|n| judged(Grade::Perfect, Some(n), 1., true))
            .chain([judged(Grade::Wrong, None, 0.5, true)])
            .collect();
        let feat = Feat::of(&run, 10, false, true);
        assert_eq!(feat.speed, 50, "the slowest");
        assert_eq!(feat.accuracy, Some(100), "a wrong key is no note");
        assert!(feat.whole);
        assert!(!feat.waited);
        assert_eq!(earned(&feat), 1);
        // A note never judged: not the whole piece. A note judged twice (a loop) is one note.
        let mut part = run[1..].to_vec();
        part.push(run[1]);
        assert!(!Feat::of(&part, 10, false, true).whole);
        // Under the wait.
        let waited = [judged(Grade::Perfect, Some(0), 1., false)];
        let feat = Feat::of(&waited, 1, false, true);
        assert!(feat.waited);
        assert_eq!(earned(&feat), 0);
        assert_eq!(Feat::of(&[], 0, false, true).speed, 0);
    }
}
