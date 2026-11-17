//! The score, the combo and the accuracy of a run (design §5.2), from its judgements, as it is
//! played. Pure: the transport (`transport.rs`) feeds it what the judge (`judge.rs`) said.
//!
//! - **Points**: perfect 100, good 70, late 30, missed and wrong 0, each multiplied by the combo
//!   it is part of, capped at [`COMBO_CAP`] (the design says capped, not where: ×10 is a default
//!   to confirm, as those of its §11).
//! - **Combo**: the notes struck in a row, each counting itself (the first is ×1), back to 0 at a
//!   note missed or a wrong key. A late note keeps it going.
//! - **Accuracy**: the share of the notes judged (perfect, good, late, missed) that were perfect or
//!   good. A wrong key is no note: it breaks the combo and scores nothing, and leaves the accuracy.
//! - **The wait** has no timing (`judge.rs`): what is judged under it keeps the combo going, or
//!   breaks it, but earns no points and is left out of the accuracy, which would be 100 % for
//!   any note struck. It is counted apart, to be said.
//!
//! The judge says a miss some 350 ms after its note, after the keys struck for the notes that
//! follow. The combo is that of the piece's order, by pass then moment: a judgement that comes
//! behind others is put in its place, and what follows it is counted again: only a few, as a
//! miss is never far behind.

use crate::judge::{Grade, Judgement};

/// The most the combo multiplies a note's points by.
pub const COMBO_CAP: u32 = 10;

/// What a note judged `grade` is worth before the combo.
pub fn points(grade: Grade) -> u64 {
    match grade {
        Grade::Perfect => 100,
        Grade::Good => 70,
        Grade::Late => 30,
        Grade::Missed | Grade::Wrong => 0,
    }
}

/// Where a run stands, after the judgements counted so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    pub score: u64,
    /// The notes struck in a row since the last miss or wrong key.
    pub combo: u32,
    pub best_combo: u32,
    /// The notes judged with timing (perfect, good, late, missed).
    pub notes: u32,
    /// Of them, those perfect or good.
    pub accurate: u32,
    /// The notes judged under the wait, struck or missed.
    pub waited: u32,
    /// The wrong keys, timed or not.
    pub wrong: u32,
}

impl Standing {
    /// The share of the timed notes perfect or good, in percent, rounded down (100 is all of
    /// them); none before the first timed note.
    pub fn accuracy(&self) -> Option<u32> {
        (self.notes > 0).then(|| {
            u32::try_from(u64::from(self.accurate) * 100 / u64::from(self.notes)).unwrap_or(100)
        })
    }

    /// The combo's multiplier now.
    pub fn multiplier(&self) -> u32 {
        self.combo.clamp(1, COMBO_CAP)
    }

    /// `self` after `judgement`, in the piece's order.
    fn after(mut self, grade: Grade, timed: bool) -> Self {
        let breaks = matches!(grade, Grade::Missed | Grade::Wrong);
        self.combo = if breaks { 0 } else { self.combo + 1 };
        self.best_combo = self.best_combo.max(self.combo);
        match (grade, timed) {
            (Grade::Wrong, _) => self.wrong += 1,
            (_, false) => self.waited += 1,
            (_, true) => {
                self.notes += 1;
                if matches!(grade, Grade::Perfect | Grade::Good) {
                    self.accurate += 1;
                }
                self.score += points(grade) * u64::from(self.combo.min(COMBO_CAP));
            }
        }
        self
    }
}

/// A judgement counted: its place in the piece's order, and where the run stood after it.
#[derive(Clone, Copy, Debug)]
struct Counted {
    pass: u32,
    at_ms: f64,
    /// The order it came in: two at the same moment (a chord) keep it.
    came: usize,
    grade: Grade,
    timed: bool,
    after: Standing,
}

impl Counted {
    fn before(&self, other: &Counted) -> bool {
        self.pass
            .cmp(&other.pass)
            .then(self.at_ms.total_cmp(&other.at_ms))
            .then(self.came.cmp(&other.came))
            .is_lt()
    }
}

/// The judgements of one run counted in the piece's order (see the module).
#[derive(Clone, Debug, Default)]
pub struct Tally {
    /// The judge's run they are of: another one starts the tally afresh.
    run: u64,
    /// In the piece's order.
    counted: Vec<Counted>,
}

impl Tally {
    /// The tally of `judgements`, all of one run, in the order they were made.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the end of a run (#19) counts the run that ended")
    )]
    pub fn of(judgements: &[Judgement]) -> Self {
        let mut tally = Self::default();
        tally.follow(0, judgements);
        tally
    }

    /// Catches up with the judge: `run` is which run it is on, `judgements` that run's, in the
    /// order they were made. Returns those it had not counted yet.
    pub fn follow<'a>(&mut self, run: u64, judgements: &'a [Judgement]) -> &'a [Judgement] {
        if run != self.run || judgements.len() < self.counted.len() {
            self.run = run;
            self.counted.clear();
        }
        let new = &judgements[self.counted.len()..];
        for judgement in new {
            self.push(judgement);
        }
        new
    }

    pub fn standing(&self) -> Standing {
        self.counted.last().map(|c| c.after).unwrap_or_default()
    }

    fn push(&mut self, judgement: &Judgement) {
        let new = Counted {
            pass: judgement.pass,
            at_ms: judgement.at_ms,
            came: self.counted.len(),
            grade: judgement.grade,
            timed: judgement.timed,
            after: Standing::default(),
        };
        // From the end: what comes late is never far behind.
        let at = self
            .counted
            .iter()
            .rposition(|counted| counted.before(&new))
            .map_or(0, |before| before + 1);
        self.counted.insert(at, new);
        let mut standing = match at {
            0 => Standing::default(),
            _ => self.counted[at - 1].after,
        };
        for counted in &mut self.counted[at..] {
            standing = standing.after(counted.grade, counted.timed);
            counted.after = standing;
        }
    }
}

/// What the transport says of `standing`, in order, each as what and how much: the score and
/// the accuracy once a note was timed, the combo (and its multiplier past 1), the notes judged
/// under the wait, the wrong keys. Nothing before the first judgement.
pub fn readout(standing: &Standing) -> Vec<(&'static str, String)> {
    let mut said = Vec::new();
    if *standing == Standing::default() {
        return said;
    }
    if standing.notes > 0 {
        said.push(("Score", grouped(standing.score)));
    }
    let combo = match standing.multiplier() {
        1 => standing.combo.to_string(),
        times => format!("{} \u{d7}{times}", standing.combo),
    };
    said.push(("Combo", combo));
    if let Some(accuracy) = standing.accuracy() {
        said.push(("Accuracy", format!("{accuracy} %")));
    }
    if standing.waited > 0 {
        let notes = match standing.waited {
            1 => "1 note".to_owned(),
            n => format!("{n} notes"),
        };
        said.push(("With the wait", format!("{notes}, not scored")));
    }
    if standing.wrong > 0 {
        said.push(("Wrong", standing.wrong.to_string()));
    }
    said
}

/// `12 345`: the score, its thousands apart.
pub fn grouped(score: u64) -> String {
    let digits = score.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judged(grade: Grade, pass: u32, at_ms: f64) -> Judgement {
        Judgement {
            grade,
            key: 60,
            note: (grade != Grade::Wrong).then_some(0),
            measure: 0,
            pass,
            offset_ms: None,
            timed: true,
            speed: 1.,
            at_ms,
        }
    }

    fn untimed(grade: Grade, at_ms: f64) -> Judgement {
        Judgement {
            timed: false,
            ..judged(grade, 0, at_ms)
        }
    }

    use Grade::{Good, Late, Missed, Perfect, Wrong};

    #[test]
    fn each_note_is_worth_its_points_times_its_combo() {
        let run = [
            judged(Perfect, 0, 0.),
            judged(Good, 0, 500.),
            judged(Late, 0, 1000.),
        ];
        let standing = Tally::of(&run).standing();
        assert_eq!(standing.score, 100 + 70 * 2 + 30 * 3);
        assert_eq!((standing.combo, standing.best_combo), (3, 3));
        assert_eq!(standing.multiplier(), 3);
        assert_eq!((standing.notes, standing.accurate), (3, 2));
        assert_eq!(standing.accuracy(), Some(66), "rounded down");
    }

    #[test]
    fn a_miss_or_a_wrong_key_breaks_the_combo() {
        let run = [
            judged(Perfect, 0, 0.),
            judged(Perfect, 0, 500.),
            judged(Missed, 0, 1000.),
            judged(Perfect, 0, 1500.),
            judged(Wrong, 0, 1700.),
            judged(Perfect, 0, 2000.),
        ];
        let standing = Tally::of(&run).standing();
        // 100, 200, the miss, 100, the wrong key, 100.
        assert_eq!(standing.score, 100 + 200 + 100 + 100);
        assert_eq!((standing.combo, standing.best_combo), (1, 2));
        // The wrong key is no note: four of five notes were accurate.
        assert_eq!((standing.notes, standing.wrong), (5, 1));
        assert_eq!(standing.accuracy(), Some(80));
    }

    #[test]
    fn the_multiplier_stops_at_the_cap() {
        let run: Vec<Judgement> = (0..15)
            .map(|n| judged(Perfect, 0, f64::from(n) * 100.))
            .collect();
        let standing = Tally::of(&run).standing();
        let below: u64 = (1..=u64::from(COMBO_CAP)).map(|combo| 100 * combo).sum();
        let above = 100 * u64::from(COMBO_CAP) * (15 - u64::from(COMBO_CAP));
        assert_eq!(standing.score, below + above);
        assert_eq!(standing.combo, 15);
        assert_eq!(standing.multiplier(), COMBO_CAP);
        assert_eq!(standing.accuracy(), Some(100));
    }

    #[test]
    fn a_miss_said_after_later_hits_is_counted_in_its_place() {
        // As the judge says them: the notes at 1000 and 1200 struck, then the one at 800 missed.
        let made = [
            judged(Perfect, 0, 500.),
            judged(Perfect, 0, 1000.),
            judged(Perfect, 0, 1200.),
            judged(Missed, 0, 800.),
        ];
        let mut tally = Tally::default();
        tally.follow(0, &made[..3]);
        assert_eq!(tally.standing().combo, 3);
        assert_eq!(tally.standing().score, 100 + 200 + 300);
        tally.follow(0, &made);
        let standing = tally.standing();
        // In the piece's order: perfect, missed, perfect, perfect.
        assert_eq!(standing.combo, 2);
        assert_eq!(standing.best_combo, 2);
        assert_eq!(standing.score, 100 + 100 + 200);
        // The same as counting them in the piece's order at once.
        let ordered = [made[0], made[3], made[1], made[2]];
        assert_eq!(Tally::of(&ordered).standing(), standing);
    }

    #[test]
    fn passes_come_before_moments() {
        // The loop wrapped: the second pass's first note is earlier in the piece, later in the run.
        let made = [
            judged(Perfect, 0, 3000.),
            judged(Perfect, 1, 1000.),
            judged(Missed, 0, 3500.),
        ];
        let standing = Tally::of(&made).standing();
        assert_eq!(standing.combo, 1, "the miss of pass 0 comes before pass 1");
        assert_eq!(standing.score, 100 + 100);
    }

    #[test]
    fn a_chord_keeps_the_order_its_notes_came_in() {
        let made = [
            judged(Perfect, 0, 1000.),
            judged(Good, 0, 1000.),
            judged(Missed, 0, 1000.),
        ];
        let standing = Tally::of(&made).standing();
        assert_eq!(standing.score, 100 + 70 * 2);
        assert_eq!(standing.combo, 0);
    }

    #[test]
    fn the_wait_keeps_the_combo_but_earns_nothing() {
        let made = [
            untimed(Perfect, 0.),
            untimed(Perfect, 500.),
            untimed(Wrong, 700.),
            untimed(Perfect, 1000.),
        ];
        let standing = Tally::of(&made).standing();
        assert_eq!(standing.score, 0);
        assert_eq!(standing.accuracy(), None);
        assert_eq!((standing.waited, standing.wrong), (3, 1));
        assert_eq!((standing.combo, standing.best_combo), (1, 2));
        // The wait turned off: the combo it built multiplies the timed notes.
        let mut more = made.to_vec();
        more.push(judged(Perfect, 0, 1500.));
        assert_eq!(Tally::of(&more).standing().score, 200);
    }

    #[test]
    fn another_run_starts_afresh_and_only_new_judgements_are_returned() {
        let first = [judged(Perfect, 0, 0.), judged(Perfect, 0, 500.)];
        let mut tally = Tally::default();
        assert_eq!(tally.follow(0, &first[..1]).len(), 1);
        assert_eq!(tally.follow(0, &first).len(), 1);
        assert!(tally.follow(0, &first).is_empty());
        assert_eq!(tally.standing().combo, 2);
        let second = [judged(Missed, 0, 0.)];
        assert_eq!(tally.follow(1, &second).len(), 1);
        assert_eq!(tally.standing().combo, 0);
        assert_eq!(tally.standing().notes, 1);
        assert_eq!(Tally::default().standing(), Standing::default());
    }

    #[test]
    fn the_readout_says_what_the_run_has() {
        assert!(readout(&Standing::default()).is_empty());
        let timed = Tally::of(&[
            judged(Perfect, 0, 0.),
            judged(Good, 0, 500.),
            judged(Wrong, 0, 700.),
            judged(Perfect, 0, 1000.),
            judged(Perfect, 0, 1500.),
        ])
        .standing();
        assert_eq!(
            readout(&timed),
            [
                ("Score", "540".to_owned()),
                ("Combo", "2 \u{d7}2".to_owned()),
                ("Accuracy", "100 %".to_owned()),
                ("Wrong", "1".to_owned()),
            ]
        );
        // Under the wait alone: no score, no accuracy.
        let waited = Tally::of(&[untimed(Perfect, 0.)]).standing();
        assert_eq!(
            readout(&waited),
            [
                ("Combo", "1".to_owned()),
                ("With the wait", "1 note, not scored".to_owned()),
            ]
        );
    }

    #[test]
    fn the_score_reads_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1\u{202f}000");
        assert_eq!(grouped(1_234_567), "1\u{202f}234\u{202f}567");
    }
}
