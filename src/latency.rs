//! The latency of the player's keys (design §6), measured in the settings: Gamme plays a click
//! every [`CLICK_EVERY_MS`], on the output the piece plays on, the player strikes a key with each,
//! and the mean of how far his keys land from the clicks, early or late, is kept (`config.rs`,
//! `latency_ms`). The judge takes it away from each key (`judge.rs`).
//!
//! It is the player's own offset: all that comes between a note of the clock and the key that
//! answers it, the sound on its way to the speakers, the ear and the hand (a player tapping along
//! often leads the beat by a few tens of ms, which makes it negative), the key on its way back
//! over MIDI. The keys are
//! placed on the clicks' clock as the play screen places them on a piece's
//! (`Clock::position_at`), so what is measured is what the judge meets.
//!
//! The first [`COUNT_IN`] clicks give the beat and are not measured. Each click counts once, with
//! the key nearest it within half a beat; keys further than [`OUTLIER_MS`] from the median are
//! slips, left out. Fewer than [`ENOUGH`] keys measure nothing. Since a key counts within half a
//! beat of its click, no measure goes beyond ±300 ms; what is kept is held within ±[`LIMIT_MS`].

use crate::timeline::{Hand, Note, Timeline};

/// One click every so often: 100 beats a minute.
pub const CLICK_EVERY_MS: f64 = 600.;
/// The clicks that give the beat first, not measured.
pub const COUNT_IN: usize = 4;
/// All the clicks, the count-in's included.
pub const CLICKS: usize = 16;
/// The fewest keys that measure something.
pub const ENOUGH: usize = 6;
/// The furthest a key may land from the median of the others and still count, in ms.
pub const OUTLIER_MS: f64 = 100.;
/// The largest latency kept, either way, in ms: a key counts within half a beat of its click
/// (300 ms), and more than this is a player not playing along rather than a latency.
pub const LIMIT_MS: i32 = 250;

/// The click's key: high on the piano, short, clear of the middle of a speaker's sound.
const CLICK_KEY: u8 = 96;

/// When each click sounds, in ms of the clicks' clock, the count-in's first. The first waits a
/// beat, so that it is not cut.
pub fn clicks() -> Vec<f64> {
    (1..=CLICKS)
        .map(|beat| beat as f64 * CLICK_EVERY_MS)
        .collect()
}

/// The clicks as a piece: one bar of four each, its first click louder.
pub fn timeline() -> Timeline {
    let clicks = clicks();
    let notes = clicks
        .iter()
        .enumerate()
        .map(|(beat, &on_ms)| Note {
            key: CLICK_KEY,
            velocity: if beat % 4 == 0 { 120 } else { 90 },
            on_ms,
            off_ms: on_ms + 80.,
            hand: Hand::Right,
            part: 0,
            measure: 0,
            element: None,
        })
        .collect();
    let measures = clicks.iter().step_by(4).copied().collect();
    let end = clicks.last().copied().unwrap_or(0.) + CLICK_EVERY_MS;
    Timeline::new(notes, measures, end)
}

/// The latency measured from the keys `struck` (where each landed on the clicks' clock, in ms)
/// against the clicks `counted` (those after the count-in): the mean of how late they were,
/// rounded, held within ±[`LIMIT_MS`]. None with fewer than [`ENOUGH`] keys that count.
pub fn measure(counted: &[f64], struck: &[f64]) -> Option<i32> {
    let half = CLICK_EVERY_MS / 2.;
    let mut nearest: Vec<Option<f64>> = vec![None; counted.len()];
    for &key in struck {
        let Some((click, offset)) = counted
            .iter()
            .enumerate()
            .map(|(click, &at)| (click, key - at))
            .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        else {
            continue;
        };
        if offset.abs() >= half || !offset.is_finite() {
            continue;
        }
        if nearest[click].is_none_or(|kept| offset.abs() < kept.abs()) {
            nearest[click] = Some(offset);
        }
    }
    let mut offsets: Vec<f64> = nearest.into_iter().flatten().collect();
    if offsets.len() < ENOUGH {
        return None;
    }
    offsets.sort_by(f64::total_cmp);
    let median = offsets[offsets.len() / 2];
    let kept: Vec<f64> = offsets
        .into_iter()
        .filter(|offset| (offset - median).abs() <= OUTLIER_MS)
        .collect();
    if kept.len() < ENOUGH {
        return None;
    }
    let mean = kept.iter().sum::<f64>() / kept.len() as f64;
    Some((mean.round() as i32).clamp(-LIMIT_MS, LIMIT_MS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::{Expected, Grade, Judge, Moment, Windows};

    fn counted() -> Vec<f64> {
        clicks()[COUNT_IN..].to_vec()
    }

    /// Keys `late` after each counted click, give or take a few ms.
    fn struck(late: f64) -> Vec<f64> {
        let jitter = [-12., 7., 3., -5., 10., -2., 0., 4., -8., 6., -3., 0.];
        counted()
            .iter()
            .zip(jitter)
            .map(|(click, jitter)| click + late + jitter)
            .collect()
    }

    #[test]
    fn the_clicks_are_a_piece_of_four_bars_after_a_beat_of_silence() {
        let timeline = timeline();
        assert_eq!(timeline.notes().len(), CLICKS);
        assert_eq!(timeline.notes()[0].on_ms, CLICK_EVERY_MS);
        assert_eq!(timeline.measure_count(), 1 + CLICKS / 4);
        assert_eq!(timeline.duration_ms(), (CLICKS + 1) as f64 * CLICK_EVERY_MS);
        assert!(timeline.notes()[0].velocity > timeline.notes()[1].velocity);
    }

    #[test]
    fn a_calibration_measures_how_late_the_keys_land() {
        assert_eq!(measure(&counted(), &struck(0.)), Some(0));
        assert_eq!(measure(&counted(), &struck(40.)), Some(40));
        assert_eq!(measure(&counted(), &struck(-25.)), Some(-25));
        // Another calibration, another offset.
        assert_ne!(
            measure(&counted(), &struck(40.)),
            measure(&counted(), &struck(90.))
        );
    }

    #[test]
    fn slips_double_keys_and_the_count_in_do_not_count() {
        let mut keys = struck(40.);
        // A slip, far from the others but within half a beat.
        keys[3] += 200.;
        // A second key on one click: the nearer one counts.
        keys.push(counted()[5] + 140.);
        // Keys on the count-in.
        keys.extend(&clicks()[..COUNT_IN]);
        let measured = measure(&counted(), &keys).unwrap();
        assert!((39..=41).contains(&measured), "{measured}");
    }

    #[test]
    fn a_measure_beyond_the_limit_is_held_to_it() {
        assert_eq!(measure(&counted(), &struck(285.)), Some(LIMIT_MS));
        assert_eq!(measure(&counted(), &struck(-285.)), Some(-LIMIT_MS));
    }

    #[test]
    fn too_few_keys_measure_nothing() {
        let keys = struck(40.);
        assert_eq!(measure(&counted(), &keys[..ENOUGH - 1]), None);
        assert!(measure(&counted(), &keys[..ENOUGH]).is_some());
        assert_eq!(measure(&counted(), &[]), None);
        // Keys half a beat away are for no click.
        let off: Vec<f64> = counted().iter().map(|c| c + CLICK_EVERY_MS / 2.).collect();
        assert_eq!(measure(&counted(), &off), None);
    }

    #[test]
    fn the_judge_takes_away_the_latency_measured() {
        let measured = measure(&counted(), &struck(60.)).unwrap();
        let note = Expected {
            note: 0,
            key: 60,
            on_ms: 1000.,
            measure: 0,
        };
        let moment = |ms| Moment {
            seeks: 0,
            ms,
            speed: 1.,
            wait: false,
            playing: true,
        };
        let grade = |latency: f64| {
            let mut judge = Judge::new(vec![note], vec![0.], 5000., Windows::DEFAULT, latency);
            judge.at(moment(0.));
            // A key on time, which arrives as late as the calibration measured.
            judge
                .strike(60, moment(1000. + f64::from(measured) + 10.))
                .unwrap()
        };
        let uncalibrated = grade(0.);
        let calibrated = grade(f64::from(measured));
        assert_eq!(uncalibrated.grade, Grade::Good);
        assert_eq!(calibrated.grade, Grade::Perfect);
        assert_eq!(calibrated.offset_ms, Some(10.));
    }
}
