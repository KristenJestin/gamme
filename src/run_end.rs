//! The end of a run (design §5.3): its score, accuracy and best combo, the stars it earned
//! (`stars.rs`), the measures most missed, and Rework: a loop on those measures, slowed.
//!
//! - **The measures most missed**: each fault weighs on the measure it was made in, in the order
//!   of play (a repeat plays a measure twice: each time is its own), every pass of a loop counted.
//!   A note missed and a wrong key weigh [`FAULT`]: both break the combo and score nothing, and
//!   the ladder counts both as faults (design §4.1). A late note weighs [`LATE`]: it keeps the
//!   combo and scores a little, and is a fault only from the ladder's fourth step. The heaviest
//!   [`MOST_MISSED`] are named, heaviest first, a tie in the order of play; a measure is named as
//!   the score prints it, with the time it is played when a repeat names it again ("3 (2nd
//!   time)"). Late notes count, so the end says them "to rework" rather than "missed".
//! - **Rework** loops those measures one after the other, in the order of play (`clock.rs`'s
//!   spans: measures 3 and 7 play 3, 7, 3, 7…; measures that follow each other play as one span),
//!   at the next speed below the run's among those the transport offers (100 % to 50 %, 50 % to
//!   25 %; 25 % stays): the step the player would take himself, and one the speed control shows.
//!
//! The pure parts are [`most_missed`], [`rework`] and [`Summary::of`]; [`view`] draws them with
//! the kit's components.

use std::collections::BTreeMap;

use gpui_design_system::{
    Button, Icon, LabelledLine, Modal, Type as _, lines_block, palette, space,
};
use gpui_kit::*;

use crate::clock::Spans;
use crate::icons;
use crate::judge::{Grade, Judgement};
use crate::stars::{self, Feat};
use crate::tally::{self, Standing, Tally};

/// How many measures the end of a run names, and Rework loops.
pub const MOST_MISSED: usize = 3;

/// What a note missed or a wrong key weighs on its measure.
pub const FAULT: u32 = 2;

/// What a late note weighs on its measure.
pub const LATE: u32 = 1;

/// The faults of one measure in a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// In the order of play, 0 the first.
    pub measure: usize,
    pub missed: u32,
    pub wrong: u32,
    pub late: u32,
}

impl Faults {
    pub fn weight(&self) -> u32 {
        FAULT * (self.missed + self.wrong) + LATE * self.late
    }
}

/// The measures most missed in a run's `judgements`, heaviest first (see the module); none
/// when nothing was missed, wrong or late.
pub fn most_missed(judgements: &[Judgement]) -> Vec<Faults> {
    let mut measures: BTreeMap<u32, Faults> = BTreeMap::new();
    for judgement in judgements {
        let faults = measures.entry(judgement.measure).or_insert_with(|| Faults {
            measure: judgement.measure as usize,
            ..Faults::default()
        });
        match judgement.grade {
            Grade::Missed => faults.missed += 1,
            Grade::Wrong => faults.wrong += 1,
            Grade::Late => faults.late += 1,
            Grade::Perfect | Grade::Good => {}
        }
    }
    let mut faults: Vec<Faults> = measures
        .into_values()
        .filter(|faults| faults.weight() > 0)
        .collect();
    // Stable: a tie keeps the order of play.
    faults.sort_by_key(|faults| std::cmp::Reverse(faults.weight()));
    faults.truncate(MOST_MISSED);
    faults
}

/// What Rework plays: the measures, in the order of play, as the clock's spans, and the speed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub measures: Vec<usize>,
    pub spans: Spans,
    /// In percent.
    pub speed: u32,
}

/// Rework of `missed`, for a run played at `speed` (percent), slowed to the next of `offered`
/// below it (see the module); none when nothing was missed.
pub fn rework(missed: &[Faults], speed: u32, offered: &[u32]) -> Option<Plan> {
    if missed.is_empty() {
        return None;
    }
    let mut measures: Vec<usize> = missed.iter().map(|faults| faults.measure).collect();
    measures.sort_unstable();
    let slowed = offered
        .iter()
        .copied()
        .filter(|&offer| offer < speed)
        .max()
        .or_else(|| offered.iter().copied().min())
        .unwrap_or(speed);
    Some(Plan {
        spans: Spans::of(measures.iter().map(|&measure| (measure, measure))),
        measures,
        speed: slowed,
    })
}

/// A run that ended, as its end shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub standing: Standing,
    pub feat: Feat,
    pub stars: u8,
    pub missed: Vec<Faults>,
    pub rework: Option<Plan>,
}

impl Summary {
    /// The end of the run of `judgements`, of a piece whose pass owes `owed` notes; `aided` and
    /// `engraved` as [`Feat`] says, `offered` the speeds the transport offers.
    pub fn of(
        judgements: &[Judgement],
        owed: usize,
        aided: bool,
        engraved: bool,
        offered: &[u32],
    ) -> Self {
        let feat = Feat::of(judgements, owed, aided, engraved);
        let missed = most_missed(judgements);
        Self {
            standing: Tally::of(judgements).standing(),
            stars: stars::earned(&feat),
            rework: rework(&missed, feat.speed, offered),
            feat,
            missed,
        }
    }

    /// Why no star came, when none did and the run was timed: what the stars ask.
    pub fn why_no_star(&self) -> Option<String> {
        if self.stars > 0 {
            return None;
        }
        let feat = &self.feat;
        Some(if feat.waited {
            format!(
                "Stars are earned with the wait off: {} % accuracy at 50 % speed for the first.",
                stars::ACCURACY
            )
        } else if !feat.whole {
            "Stars are earned on the whole piece, from its first note to its last.".to_owned()
        } else if feat.speed < stars::SPEEDS[0] {
            format!(
                "The first star is earned at {} % speed or faster.",
                stars::SPEEDS[0]
            )
        } else {
            format!(
                "Each star needs {} % accuracy: notes perfect or good.",
                stars::ACCURACY
            )
        })
    }
}

/// `1st`, `2nd`, `3rd`, `4th`…
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Measure `index` (in the order of play) as the score prints it (`names`, in the order of play;
/// counted from 1 when it has none), and the time it is played when a repeat names it again.
pub fn measure_name(index: usize, names: &[impl AsRef<str>]) -> String {
    let Some(name) = names.get(index).map(AsRef::as_ref) else {
        return (index + 1).to_string();
    };
    let before = names[..index]
        .iter()
        .filter(|other| other.as_ref() == name)
        .count();
    match before {
        0 => name.to_owned(),
        n => format!("{name} ({} time)", ordinal(n + 1)),
    }
}

/// "measure 3", "measures 3 and 7", "measures 3, 5 and 7": `measures` in the order given.
pub fn measures_text(measures: &[usize], names: &[impl AsRef<str>]) -> String {
    let named: Vec<String> = measures
        .iter()
        .map(|&measure| measure_name(measure, names))
        .collect();
    match named.as_slice() {
        [] => String::new(),
        [one] => format!("measure {one}"),
        [first @ .., last] => format!("measures {} and {last}", first.join(", ")),
    }
}

/// What a measure's faults say: "2 missed, 1 wrong".
pub fn faults_text(faults: &Faults) -> String {
    let parts: Vec<String> = [
        (faults.missed, "missed"),
        (faults.wrong, "wrong"),
        (faults.late, "late"),
    ]
    .iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, word)| format!("{count} {word}"))
    .collect();
    parts.join(", ")
}

/// What the transport says of a Rework under way: "Rework: measures 3 and 7".
pub fn rework_text(plan: &Plan, names: &[impl AsRef<str>]) -> String {
    format!("Rework: {}", measures_text(&plan.measures, names))
}

/// Upper-cased first letter.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The sentence under the end's title: what to rework, and how.
pub fn sentence(summary: &Summary, names: &[impl AsRef<str>]) -> String {
    match &summary.rework {
        Some(plan) => format!(
            "{} to rework most: Rework loops {} at {} %.",
            capitalised(&measures_text(&plan.measures, names)),
            if plan.measures.len() == 1 {
                "it"
            } else {
                "them"
            },
            plan.speed
        ),
        None if summary.standing.notes + summary.standing.waited > 0 => {
            "Nothing to rework: no note missed, late or wrong.".to_owned()
        }
        None => "Nothing was judged.".to_owned(),
    }
}

/// What a button of the end of a run does.
pub type Action = Box<dyn Fn(&mut Window, &mut App)>;

/// What the end of a run does when a button is clicked.
pub struct Actions {
    pub rework: Action,
    pub again: Action,
    pub close: Action,
}

/// The end of a run, over the play screen: the kit's dialog, drawn all the time, `open` while it
/// shows; `names` the measures as the score prints them, in the order of play.
pub fn view(
    summary: &Summary,
    names: &[impl AsRef<str>],
    open: bool,
    actions: Actions,
    cx: &App,
) -> Modal {
    let p = palette(cx);
    let standing = &summary.standing;
    let stars_said = format!("{} of {}", summary.stars, stars::MOST);
    let star_icons = (0..stars::MOST).map(|at| {
        let earned = at < summary.stars;
        gpui_design_system::icon(if earned {
            icons::STAR_FILL
        } else {
            icons::STAR
        })
        .size(px(14.))
        .text_color(if earned { p.acc } else { p.faint })
    });
    let accuracy = standing
        .accuracy()
        .map_or_else(|| "\u{2013}".to_owned(), |accuracy| format!("{accuracy} %"));
    let lines = [
        LabelledLine::new(
            "run-end-score",
            icons::TROPHY,
            "Score",
            tally::grouped(standing.score),
        )
        .child(tally::grouped(standing.score))
        .into_any_element(),
        LabelledLine::new(
            "run-end-accuracy",
            icons::TARGET,
            "Accuracy",
            accuracy.clone(),
        )
        .child(accuracy)
        .into_any_element(),
        LabelledLine::new(
            "run-end-combo",
            icons::FLAME,
            "Best combo",
            standing.best_combo.to_string(),
        )
        .child(standing.best_combo.to_string())
        .into_any_element(),
        LabelledLine::new("run-end-stars", icons::STAR, "Stars", stars_said.clone())
            .children(star_icons)
            .child(
                div()
                    .pl(space::XS)
                    .child(format!("{stars_said} at {} %", summary.feat.speed)),
            )
            .into_any_element(),
    ];
    let said_of_stars = summary
        .why_no_star()
        .or_else(|| stars::third_asks(&summary.feat).map(str::to_owned));
    let note = said_of_stars.map(|said| {
        div()
            .id("run-end-stars-note")
            .ui_text()
            .text_color(p.mute)
            .child(said)
    });
    let missed = (!summary.missed.is_empty()).then(|| {
        div()
            .id("run-end-missed")
            .flex()
            .flex_col()
            .gap(space::XS)
            .child(div().ui_text().semibold().child("The measures to rework"))
            .children(summary.missed.iter().map(|faults| {
                div()
                    .flex()
                    .gap(space::SM)
                    .ui_text()
                    .child(
                        div()
                            .text_color(p.text)
                            .child(format!("Measure {}", measure_name(faults.measure, names))),
                    )
                    .child(div().text_color(p.mute).child(faults_text(faults)))
            }))
    });
    let sentence = sentence(summary, names);
    let Actions {
        rework,
        again,
        close,
    } = actions;
    let mut modal = Modal::new("run-end", "End of the run")
        .sentence(sentence)
        .cancel_label("Close")
        .takes_focus()
        .open(open)
        .on_close(close)
        .child(lines_block(lines))
        .children(note)
        .children(missed);
    modal = modal.action(
        Button::secondary("run-end-again", "Play again")
            .leading(Icon::Restart)
            .on_click(move |_, window, cx| again(window, cx)),
    );
    if let Some(plan) = &summary.rework {
        modal = modal.action(
            Button::primary("run-end-rework", format!("Rework at {} %", plan.speed))
                .leading(icons::REPEAT)
                .tooltip(capitalised(&measures_text(&plan.measures, names)))
                .on_click(move |_, window, cx| rework(window, cx)),
        );
    }
    modal
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's own `test` attribute would come with it.
    use super::{
        Faults, Grade, Judgement, MOST_MISSED, Summary, faults_text, measure_name, measures_text,
        most_missed, ordinal, rework, rework_text, sentence,
    };

    fn judged(grade: Grade, measure: u32, pass: u32, at_ms: f64) -> Judgement {
        Judgement {
            grade,
            key: 60,
            note: (grade != Grade::Wrong).then_some(measure as usize),
            measure,
            pass,
            offset_ms: None,
            timed: true,
            speed: 1.,
            at_ms,
        }
    }

    use Grade::{Good, Late, Missed, Perfect, Wrong};

    /// A run of ten measures, a note each, with misses in measures 3 and 7 (2 and 6 in the
    /// order of play, from 0): the issue's check.
    fn misses_in_3_and_7() -> Vec<Judgement> {
        (0..10)
            .map(|m| {
                let grade = if m == 2 || m == 6 { Missed } else { Perfect };
                judged(grade, m, 0, f64::from(m) * 1000.)
            })
            .collect()
    }

    #[test]
    fn misses_in_measures_3_and_7_are_named_and_reworked_slowed() {
        let summary = Summary::of(&misses_in_3_and_7(), 10, true, true, &[25, 50, 100]);
        let measures: Vec<usize> = summary.missed.iter().map(|f| f.measure).collect();
        assert_eq!(measures, [2, 6]);
        let names: Vec<String> = (1..=10).map(|n| n.to_string()).collect();
        assert_eq!(measures_text(&measures, &names), "measures 3 and 7");
        let plan = summary.rework.as_ref().unwrap();
        assert_eq!(plan.spans.as_slice(), [(2, 2), (6, 6)]);
        assert_eq!(plan.speed, 50);
        assert_eq!(rework_text(plan, &names), "Rework: measures 3 and 7");
        assert_eq!(
            sentence(&summary, &names),
            "Measures 3 and 7 to rework most: Rework loops them at 50 %."
        );
        // 8 of 10 accurate: no star, and the end says why.
        assert_eq!(summary.standing.accuracy(), Some(80));
        assert_eq!(summary.stars, 0);
        assert!(summary.why_no_star().unwrap().contains("90 %"));
    }

    #[test]
    fn a_miss_and_a_wrong_key_weigh_more_than_a_late_note() {
        let run = [
            judged(Late, 1, 0, 1000.),
            judged(Late, 1, 0, 1200.),
            judged(Late, 1, 0, 1400.),
            judged(Missed, 4, 0, 4000.),
            judged(Wrong, 4, 0, 4100.),
            judged(Missed, 6, 0, 6000.),
            judged(Late, 6, 0, 6100.),
            judged(Good, 8, 0, 8000.),
            judged(Late, 9, 0, 9000.),
        ];
        let missed = most_missed(&run);
        let weights: Vec<(usize, u32)> = missed.iter().map(|f| (f.measure, f.weight())).collect();
        // 4: 2 + 2; 1: three lates; 6: 2 + 1, a tie with 1 that comes later in the play.
        assert_eq!(weights, [(4, 4), (1, 3), (6, 3)]);
        assert_eq!(faults_text(&missed[0]), "1 missed, 1 wrong");
        assert_eq!(
            missed.len(),
            MOST_MISSED,
            "measure 9's late note is not named"
        );
        // Rework plays them in the order of play.
        let plan = rework(&missed, 100, &[25, 50, 100]).unwrap();
        assert_eq!(plan.measures, [1, 4, 6]);
    }

    #[test]
    fn every_pass_of_a_loop_counts() {
        let run = [
            judged(Missed, 3, 0, 3000.),
            judged(Missed, 5, 0, 5000.),
            judged(Missed, 3, 1, 3000.),
        ];
        let missed = most_missed(&run);
        assert_eq!(missed[0].measure, 3);
        assert_eq!(missed[0].missed, 2);
    }

    #[test]
    fn a_clean_run_has_nothing_to_rework() {
        let run: Vec<Judgement> = (0..4).map(|m| judged(Perfect, m, 0, 0.)).collect();
        let summary = Summary::of(&run, 4, false, true, &[25, 50, 100]);
        assert!(summary.missed.is_empty());
        assert_eq!(summary.rework, None);
        assert_eq!(summary.stars, 3);
        assert_eq!(summary.why_no_star(), None);
        let none: [&str; 0] = [];
        assert_eq!(
            sentence(&summary, &none),
            "Nothing to rework: no note missed, late or wrong."
        );
    }

    #[test]
    fn rework_slows_to_the_next_speed_offered_below_the_runs() {
        let missed = [Faults {
            measure: 2,
            missed: 1,
            ..Faults::default()
        }];
        let speed = |run| rework(&missed, run, &[25, 50, 100]).unwrap().speed;
        assert_eq!(speed(100), 50);
        assert_eq!(speed(75), 50);
        assert_eq!(speed(50), 25);
        assert_eq!(speed(25), 25, "no slower");
    }

    #[test]
    fn measures_next_to_each_other_are_reworked_as_one_span() {
        let missed = [3, 4, 9].map(|measure| Faults {
            measure,
            missed: 1,
            ..Faults::default()
        });
        let plan = rework(&missed, 100, &[25, 50, 100]).unwrap();
        assert_eq!(plan.spans.as_slice(), [(3, 4), (9, 9)]);
    }

    #[test]
    fn a_measure_is_named_as_the_score_prints_it_repeats_included() {
        // 1 2 |: 3 4 :| 5, the repeat played: 3 and 4 twice.
        let names = ["1", "2", "3", "4", "3", "4", "5"];
        assert_eq!(measure_name(2, &names), "3");
        assert_eq!(measure_name(4, &names), "3 (2nd time)");
        assert_eq!(
            measures_text(&[2, 5], &names),
            "measures 3 and 4 (2nd time)"
        );
        assert_eq!(measures_text(&[0, 2, 6], &names), "measures 1, 3 and 5");
        assert_eq!(measures_text(&[6], &names), "measure 5");
        // No score (a MIDI file): counted.
        let none: [&str; 0] = [];
        assert_eq!(measure_name(6, &none), "7");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(22), "22nd");
    }
}
