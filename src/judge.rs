//! The judge (design §5.1): each note the player owes is judged once, by the timing of the key
//! struck for it; a key struck where no note is owed is wrong.
//!
//! | Judgement | Timing, in real time |
//! |---|---|
//! | perfect | within ±50 ms |
//! | good | within ±120 ms |
//! | late | 120 to 250 ms after |
//! | missed | not struck within 250 ms |
//!
//! - **The notes owed** are those of the hands the player plays ([`expected`]), each judged on its
//!   own: a chord is its notes. A key goes to the earliest note of that key whose window holds it,
//!   unless it is not perfect for it and lands in a later note's perfect window: so a player a
//!   little late on a fast repeated key is late on each note, not early on the next. Notes of one
//!   key within [`UNISON_MS`] of each other (two staves in unison) are one key on the piano: one
//!   strike judges them all.
//! - **Speed**: the windows are real time, as the wait's early tolerance is (`practice.rs`): at
//!   50 % they cover half as much of the piece. Each judgement keeps the speed it was made at.
//! - **Latency** (measured in the settings, `latency.rs`): a key is taken as struck that much real
//!   time before it arrived.
//! - **Wait**: while the clock waits for the player there is no timing: a key of a note owed is
//!   perfect, from as early as the wait takes it ([`EARLY_MS`]); any other is wrong, but for a key
//!   of a note struck already that the wait asks to be struck again (its chord was not all down at
//!   once), which is not judged. The latency is not taken away there, as the wait does not take it
//!   away either. Each judgement says whether it was timed.
//! - **Passes**: the judge follows the clock's counter of seeks. A wrap of the loop goes on along
//!   one line: the next pass's notes come after the last pass's, so a key struck just before the
//!   seam can be early for the first note after it, and one struck just after late for the last
//!   note before it. Any other change is a seek, and starts a pass afresh from where the clock
//!   stands (a little before, within its measure, for a seek seen a frame late); the notes it left
//!   unjudged are not judged: the player was not given them. Each pass owes its notes again. The
//!   judge's first moment, and a loop changed without a seek, start from where the clock stands:
//!   what is behind was not owed, or was judged already.
//! - **Missed** is said once a note's late window has closed, and [`GRACE_MS`] more: a key is read
//!   by the window a little after it arrived, and must not find its note missed already. When the
//!   clock has stopped at the end of the piece, what is still owed is missed: a piece that ends
//!   right after its last note (a MIDI file's often does) cuts that note's late window short.
//! - **Runs**: the piece played again after its end starts a new [`Run`]; the one before is kept
//!   ([`Judge::finished`]).
//!
//! The judgements of a run are in the order they were made, not in the order of the piece: a miss
//! is said some 350 ms after its note, after keys struck for later notes. What needs the piece's
//! order (a combo) sorts by pass, then moment.
//!
//! The judge itself ([`Judge`]) is pure: it is told [`Moment`]s read from the clock and the keys
//! struck, and keeps the run's judgements. [`Live`] feeds it from a piece's clock; the audio
//! callback has nothing to do with it.

use std::collections::VecDeque;
use std::time::Instant;

use crate::clock::{self, Clock, Wait};
use crate::midi_in::Played;
use crate::practice::{EARLY_MS, Playing};
use crate::timeline::{Hand, Timeline};

/// Notes of one key starting this close, in ms of the piece, are struck as one.
pub const UNISON_MS: f64 = 30.;

/// How long, in real time, a note's late window stays open for a key read late.
pub const GRACE_MS: f64 = 100.;

/// How far before where a seek is first seen its pass starts, in real time: a seek is seen at
/// the next frame or key, the clock moved on meanwhile. Never before the measure it lands in.
const SEEK_SLACK_MS: f64 = 150.;

/// How far ahead of the clock the notes are made owed, in ms of the piece: more than any early
/// window, the latency taken away included.
const AHEAD_MS: f64 = 2000.;

/// What the game says of a note owed, or of a key struck where none was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Grade {
    Perfect,
    Good,
    Late,
    Missed,
    Wrong,
}

impl Grade {
    fn index(self) -> usize {
        match self {
            Grade::Perfect => 0,
            Grade::Good => 1,
            Grade::Late => 2,
            Grade::Missed => 3,
            Grade::Wrong => 4,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Grade::Perfect => "perfect",
            Grade::Good => "good",
            Grade::Late => "late",
            Grade::Missed => "missed",
            Grade::Wrong => "wrong",
        }
    }
}

/// The windows of the judgements, in ms of real time on either side of a note (design §5.1;
/// §11 lists them as defaults to confirm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Windows {
    /// Within it, early or late: perfect.
    pub perfect_ms: f64,
    /// Within it, early or late: good. Earlier than it, a key is for no note.
    pub good_ms: f64,
    /// Later than good and within it: late. Later than it: the note is missed.
    pub late_ms: f64,
}

impl Windows {
    pub const DEFAULT: Windows = Windows {
        perfect_ms: 50.,
        good_ms: 120.,
        late_ms: 250.,
    };

    /// The grade of a key struck `offset_ms` of real time after its note (before it when
    /// negative); none outside the windows. Each edge belongs to the better grade.
    pub fn grade(&self, offset_ms: f64) -> Option<Grade> {
        let distance = offset_ms.abs();
        if distance <= self.perfect_ms {
            Some(Grade::Perfect)
        } else if distance <= self.good_ms {
            Some(Grade::Good)
        } else if offset_ms > 0. && offset_ms <= self.late_ms {
            Some(Grade::Late)
        } else {
            None
        }
    }
}

/// A note the player owes: one of his hands'.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Expected {
    /// Its place in [`Timeline::notes`].
    pub note: usize,
    pub key: u8,
    pub on_ms: f64,
    pub measure: u32,
}

/// The notes of `timeline` the player owes when he plays `playing`, by time; none when he
/// listens.
pub fn expected(timeline: &Timeline, playing: Playing) -> Vec<Expected> {
    let plays = |hand: Hand| match playing {
        Playing::Listen => false,
        Playing::Right => hand == Hand::Right,
        Playing::Left => hand == Hand::Left,
        Playing::Both => true,
    };
    timeline
        .notes()
        .iter()
        .enumerate()
        .filter(|(_, note)| plays(note.hand))
        .map(|(note, n)| Expected {
            note,
            key: n.key,
            on_ms: n.on_ms,
            measure: n.measure,
        })
        .collect()
}

/// Where the clock stands when the judge is told something, as read from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    /// The clock's counter of seeks: one more is a wrap of the loop, any other change a seek.
    pub seeks: u32,
    /// Where the piece stands, in ms of its own tempo.
    pub ms: f64,
    /// The speed's factor, 0.25 to 1.
    pub speed: f64,
    /// Whether the clock waits for the player: no timing.
    pub wait: bool,
    /// Whether the clock plays: stopped at the end of the piece, it has ended.
    pub playing: bool,
}

/// What the judge said of one note owed, or of one key struck for none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Judgement {
    pub grade: Grade,
    pub key: u8,
    /// The note judged, by its place in [`Timeline::notes`]; none for a wrong key.
    pub note: Option<usize>,
    /// The note's measure, or the measure the wrong key was struck in.
    pub measure: u32,
    /// The pass of the run it was made in: 0 the first, one more at each wrap of the loop or seek.
    pub pass: u32,
    /// How far after its note the key was struck, in ms of real time, the latency taken away
    /// (before it when negative); none for a note missed, a wrong key, and untimed.
    pub offset_ms: Option<f64>,
    /// Whether timing applied: false while the clock waited for the player.
    pub timed: bool,
    /// The speed's factor it was made at, 0.25 to 1.
    pub speed: f64,
    /// Where in the piece, in ms: the note's moment, or where the wrong key was struck.
    pub at_ms: f64,
}

/// The judgements of a run, in the order they were made (see the module), and how many of each
/// grade.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    judgements: Vec<Judgement>,
    counts: [usize; 5],
}

impl Run {
    pub fn judgements(&self) -> &[Judgement] {
        &self.judgements
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the end of a run (#19) counts each grade")
    )]
    pub fn count(&self, grade: Grade) -> usize {
        self.counts[grade.index()]
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the end of a run (#19) says when nothing was judged"
        )
    )]
    pub fn is_empty(&self) -> bool {
        self.judgements.is_empty()
    }

    fn push(&mut self, judgement: Judgement) {
        self.counts[judgement.grade.index()] += 1;
        self.judgements.push(judgement);
    }
}

/// A note owed in one pass: its place in the judge's notes, and its moment on the judge's line.
#[derive(Clone, Copy, Debug)]
struct Owed {
    index: usize,
    due: f64,
    pass: u32,
}

/// Where the notes owed are made next: the next of the judge's notes, the shift of its pass's
/// moments onto the line, that pass, and the moment of the piece the pass ends at (the loop's
/// end, when it wraps there).
#[derive(Clone, Copy, Debug)]
struct Next {
    index: usize,
    shift: f64,
    pass: u32,
    end: f64,
}

/// Judges the keys struck against the notes owed (see the module).
///
/// Its line is the piece's time laid end to end across the wraps of the loop: a moment of the
/// clock's current pass is at `ms + shift` on it.
pub struct Judge {
    /// By time.
    expected: Vec<Expected>,
    /// Where each measure starts, in ms; the first is 0.
    measures: Vec<f64>,
    end_ms: f64,
    windows: Windows,
    /// Taken away from each key, in ms of real time.
    latency_ms: f64,
    looped: Option<(f64, f64)>,
    /// The loop changed since the last moment: a pass starts afresh.
    loop_changed: bool,
    /// The clock's counter of seeks last seen; none before the first moment.
    seeks: Option<u32>,
    started: bool,
    /// The clock stopped at the end of the piece: what comes next is another run.
    ended: bool,
    pass: u32,
    shift: f64,
    /// By moment on the line.
    owed: VecDeque<Owed>,
    /// The notes struck lately, for a key the wait asks to be struck again.
    struck: VecDeque<Owed>,
    next: Next,
    run: Run,
    finished: Option<Run>,
    /// How many runs ended: told apart in [`Judge::at`].
    runs: u64,
}

impl Judge {
    /// `measures` the start of each measure in ms, `end_ms` the piece's length, `latency_ms`
    /// what the settings measured.
    pub fn new(
        mut expected: Vec<Expected>,
        mut measures: Vec<f64>,
        end_ms: f64,
        windows: Windows,
        latency_ms: f64,
    ) -> Self {
        expected.sort_by(|a, b| a.on_ms.total_cmp(&b.on_ms));
        measures.sort_by(f64::total_cmp);
        if measures.first().is_none_or(|&first| first > 0.) {
            measures.insert(0, 0.);
        }
        Self {
            expected,
            measures,
            end_ms,
            windows,
            latency_ms: if latency_ms.is_finite() {
                latency_ms
            } else {
                0.
            },
            looped: None,
            loop_changed: false,
            seeks: None,
            started: false,
            ended: false,
            pass: 0,
            shift: 0.,
            owed: VecDeque::new(),
            struck: VecDeque::new(),
            next: Next {
                index: 0,
                shift: 0.,
                pass: 0,
                end: f64::INFINITY,
            },
            run: Run::default(),
            finished: None,
            runs: 0,
        }
    }

    /// The run being played.
    pub fn run(&self) -> &Run {
        &self.run
    }

    /// How many runs ended before it: one more, and [`Judge::run`] is another run.
    pub fn runs(&self) -> u64 {
        self.runs
    }

    /// The run played before this one, to its end.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the end of a run (#19) reads the run that ended")
    )]
    pub fn finished(&self) -> Option<&Run> {
        self.finished.as_ref()
    }

    /// The loop's start and end, in ms of the piece, for measures `first` to `last` (both
    /// included), as the clock plays them.
    pub fn loop_ms(&self, first: usize, last: usize) -> Option<(f64, f64)> {
        let start = *self.measures.get(first)?;
        let end = self
            .measures
            .get(last + 1)
            .copied()
            .unwrap_or(self.end_ms)
            .min(self.end_ms);
        (start < end).then_some((start, end))
    }

    /// The loop the clock plays now, in ms of the piece. A change starts a pass afresh at the
    /// next moment.
    pub fn set_loop(&mut self, looped: Option<(f64, f64)>) {
        let looped = looped.filter(|(start, end)| start < end);
        if looped != self.looped {
            self.looped = looped;
            self.loop_changed = true;
        }
    }

    /// Time moves on to `moment`: the clock's pass is followed, the notes coming are owed, those
    /// whose window closed are missed. What it judged (missed notes) is the end of the run.
    pub fn at(&mut self, moment: Moment) -> &[Judgement] {
        let (run, before) = (self.runs, self.run.judgements.len());
        self.observe(&moment);
        // A new run: all of it is new.
        let from = if self.runs == run { before } else { 0 };
        &self.run.judgements[from..]
    }

    /// A key struck at `moment` (the moment it arrived, read from the clock): the judgement of
    /// the note it was for, or wrong; none for a key the wait asks to be struck again. What time
    /// moving on to it judged comes before it in the run.
    pub fn strike(&mut self, key: u8, moment: Moment) -> Option<Judgement> {
        self.observe(&moment);
        let line = self.line(&moment);
        let speed = moment.speed;
        let early = if moment.wait {
            EARLY_MS
        } else {
            self.windows.good_ms
        };
        let holds = |ahead: f64| ahead >= -early * speed && ahead <= self.windows.late_ms * speed;
        let candidates: Vec<(usize, f64)> = self
            .owed
            .iter()
            .enumerate()
            .filter(|(_, owed)| self.expected[owed.index].key == key)
            .map(|(at, owed)| (at, line - owed.due))
            .filter(|&(_, ahead)| holds(ahead))
            .collect();
        let perfect = self.windows.perfect_ms * speed;
        let chosen = candidates.first().map(|&first| {
            if first.1.abs() <= perfect {
                return first;
            }
            // A later note's perfect window takes the key from an earlier note it is not perfect
            // for; the nearest of them.
            candidates[1..]
                .iter()
                .filter(|candidate| candidate.1.abs() <= perfect)
                .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .copied()
                .unwrap_or(first)
        });
        let Some((at, after)) = chosen else {
            let again = moment.wait
                && self
                    .struck
                    .iter()
                    .any(|owed| self.expected[owed.index].key == key && holds(line - owed.due));
            return (!again).then(|| self.wrong(key, line, &moment));
        };
        let owed = self.owed.remove(at)?;
        let offset_ms = (!moment.wait).then(|| after / speed);
        let grade = offset_ms.map_or(Grade::Perfect, |offset| {
            // Out of the windows by a rounding only, at an edge.
            self.windows.grade(offset).unwrap_or(if offset < 0. {
                Grade::Good
            } else {
                Grade::Late
            })
        });
        let judgement = self.judge(owed, grade, offset_ms, &moment);
        self.struck.push_back(owed);
        // Its unisons are the same key: struck with it.
        while let Some(at) = self.owed.iter().position(|other| {
            other.pass == owed.pass
                && self.expected[other.index].key == key
                && (other.due - owed.due).abs() <= UNISON_MS
        }) {
            if let Some(unison) = self.owed.remove(at) {
                self.judge(unison, grade, offset_ms, &moment);
                self.struck.push_back(unison);
            }
        }
        Some(judgement)
    }

    /// Where `moment` is on the line, the latency taken away when it is timed.
    fn line(&self, moment: &Moment) -> f64 {
        let latency = if moment.wait {
            0.
        } else {
            self.latency_ms * moment.speed
        };
        moment.ms + self.shift - latency
    }

    fn observe(&mut self, moment: &Moment) {
        self.follow(moment);
        let line = self.line(moment);
        self.make(line + AHEAD_MS);
        // Only a clock stopped there has ended: one that plays a loop ending with the piece
        // reads its end for a buffer before each wrap.
        let ended = !moment.playing && moment.ms >= self.end_ms;
        let close = (self.windows.late_ms + GRACE_MS) * moment.speed;
        while let Some(first) = self.owed.front().copied() {
            if !ended && first.due + close >= line {
                break;
            }
            self.owed.pop_front();
            self.judge(first, Grade::Missed, None, moment);
        }
        while self
            .struck
            .front()
            .is_some_and(|owed| owed.due + close < line)
        {
            self.struck.pop_front();
        }
        if ended {
            self.ended = true;
        }
    }

    /// Follows the clock's passes: a wrap goes on along the line, anything else starts afresh.
    fn follow(&mut self, moment: &Moment) {
        let Some(seen) = self.seeks else {
            return self.restart(moment, 0.);
        };
        if seen == moment.seeks {
            if self.loop_changed {
                self.restart(moment, 0.);
            }
            return;
        }
        if !self.loop_changed
            && let Some((start, end)) = self.looped
            && clock::one_wrap(seen, moment.seeks)
        {
            self.seeks = Some(moment.seeks);
            self.shift += end - start;
            self.pass += 1;
            // Nothing of the new pass was made yet (nothing came near the seam): from its start.
            if self.next.pass < self.pass {
                self.next = Next {
                    index: self.first_from(start),
                    shift: self.shift,
                    pass: self.pass,
                    end,
                };
            }
            return;
        }
        self.restart(moment, SEEK_SLACK_MS * moment.speed);
    }

    fn restart(&mut self, moment: &Moment, slack: f64) {
        if self.ended {
            self.finished = Some(std::mem::take(&mut self.run));
            self.runs += 1;
            self.ended = false;
            self.started = false;
            self.pass = 0;
        }
        self.owed.clear();
        self.struck.clear();
        self.seeks = Some(moment.seeks);
        self.loop_changed = false;
        if self.started {
            self.pass += 1;
        }
        self.started = true;
        self.shift = 0.;
        let measure = self
            .measures
            .partition_point(|&start| start <= moment.ms)
            .saturating_sub(1);
        let measure_start = self.measures.get(measure).copied().unwrap_or(0.);
        let from = (moment.ms - slack).max(measure_start);
        let end = match self.looped {
            Some((_, end)) if moment.ms < end => end,
            _ => f64::INFINITY,
        };
        self.next = Next {
            index: self.first_from(from),
            shift: 0.,
            pass: self.pass,
            end,
        };
    }

    fn first_from(&self, ms: f64) -> usize {
        self.expected.partition_point(|note| note.on_ms < ms)
    }

    /// Owes the notes up to `until` on the line, round the loop as it wraps.
    fn make(&mut self, until: f64) {
        loop {
            while let Some(note) = self.expected.get(self.next.index) {
                if note.on_ms >= self.next.end {
                    break;
                }
                let due = note.on_ms + self.next.shift;
                if due > until {
                    return;
                }
                self.owed.push_back(Owed {
                    index: self.next.index,
                    due,
                    pass: self.next.pass,
                });
                self.next.index += 1;
            }
            let Some((start, end)) = self.looped else {
                return;
            };
            if self.next.end != end {
                return;
            }
            let shift = self.next.shift + (end - start);
            if start + shift > until {
                return;
            }
            self.next = Next {
                index: self.first_from(start),
                shift,
                pass: self.next.pass + 1,
                end,
            };
        }
    }

    fn judge(
        &mut self,
        owed: Owed,
        grade: Grade,
        offset_ms: Option<f64>,
        moment: &Moment,
    ) -> Judgement {
        let note = self.expected[owed.index];
        let judgement = Judgement {
            grade,
            key: note.key,
            note: Some(note.note),
            measure: note.measure,
            pass: owed.pass,
            offset_ms,
            timed: !moment.wait,
            speed: moment.speed,
            at_ms: note.on_ms,
        };
        self.run.push(judgement);
        judgement
    }

    fn wrong(&mut self, key: u8, line: f64, moment: &Moment) -> Judgement {
        let mut at_ms = line - self.shift;
        let mut pass = self.pass;
        // Struck before the seam, read after it: where it was, in the pass before.
        if let Some((start, end)) = self.looped
            && at_ms < start
            && self.shift >= end - start
        {
            at_ms += end - start;
            pass = pass.saturating_sub(1);
        }
        let at_ms = at_ms.max(0.);
        let measure = self
            .measures
            .partition_point(|&start| start <= at_ms)
            .saturating_sub(1);
        let judgement = Judgement {
            grade: Grade::Wrong,
            key,
            note: None,
            measure: u32::try_from(measure).unwrap_or(u32::MAX),
            pass,
            offset_ms: None,
            timed: !moment.wait,
            speed: moment.speed,
            at_ms,
        };
        self.run.push(judgement);
        judgement
    }
}

/// The judge of a piece being played, fed from its clock: the play screen's, on the window's
/// thread. None judges while the player listens.
pub struct Live {
    judge: Judge,
    /// The clock's loop, as last seen.
    looped: Option<(usize, usize)>,
}

impl Live {
    /// The judge of `playing` on `timeline`, `latency_ms` taken away from each key, from where
    /// `clock` stands now (what is behind is not owed); none when the player listens.
    pub fn new(
        timeline: &Timeline,
        playing: Playing,
        latency_ms: f64,
        clock: &Clock,
    ) -> Option<Self> {
        if playing == Playing::Listen {
            return None;
        }
        let measures = (0..timeline.measure_count())
            .filter_map(|index| timeline.measure_start_ms(index))
            .collect();
        let mut live = Self {
            judge: Judge::new(
                expected(timeline, playing),
                measures,
                timeline.duration_ms(),
                Windows::DEFAULT,
                latency_ms,
            ),
            looped: None,
        };
        live.tick(clock);
        Some(live)
    }

    pub fn run(&self) -> &Run {
        self.judge.run()
    }

    /// How many runs ended before the one being played.
    pub fn runs(&self) -> u64 {
        self.judge.runs()
    }

    fn moment(clock: &Clock, at: Instant) -> Moment {
        let (seeks, ms) = clock.position_at(at);
        Moment {
            seeks,
            ms,
            speed: clock.speed().factor(),
            wait: clock.wait() != Wait::Off,
            playing: clock.playing(),
        }
    }

    fn follow_loop(&mut self, clock: &Clock) {
        let looped = clock.looped();
        if looped != self.looped {
            self.looped = looped;
            let looped = looped.and_then(|(first, last)| self.judge.loop_ms(first, last));
            self.judge.set_loop(looped);
        }
    }

    /// Time moves on (a frame, a tick of the transport): the notes whose window closed are
    /// missed, and returned.
    pub fn tick(&mut self, clock: &Clock) -> &[Judgement] {
        self.follow_loop(clock);
        self.judge.at(Self::moment(clock, Instant::now()))
    }

    /// A key from MIDI in: its judgement when it is struck while the piece plays.
    pub fn key(&mut self, played: Played, clock: &Clock) -> Option<Judgement> {
        if played.velocity == 0 || !clock.playing() {
            return None;
        }
        self.follow_loop(clock);
        self.judge
            .strike(played.key, Self::moment(clock, played.at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Marks;
    use crate::timeline::Note;

    const W: Windows = Windows::DEFAULT;

    fn note(note: usize, key: u8, on_ms: f64) -> Expected {
        Expected {
            note,
            key,
            on_ms,
            measure: 0,
        }
    }

    /// A judge of `notes` (key, moment), measures of 2 s, a piece of 20 s.
    fn judge_of(notes: &[(u8, f64)], latency_ms: f64) -> Judge {
        let expected = notes
            .iter()
            .enumerate()
            .map(|(at, &(key, on))| Expected {
                measure: (on / 2000.) as u32,
                ..note(at, key, on)
            })
            .collect();
        let measures = (0..10).map(|m| f64::from(m) * 2000.).collect();
        Judge::new(expected, measures, 20_000., W, latency_ms)
    }

    fn at(ms: f64) -> Moment {
        Moment {
            seeks: 0,
            ms,
            speed: 1.,
            wait: false,
            playing: true,
        }
    }

    /// A key struck that is judged.
    fn hit(judge: &mut Judge, key: u8, moment: Moment) -> Judgement {
        judge.strike(key, moment).expect("a judgement")
    }

    fn grades(run: &Run) -> Vec<(Grade, Option<usize>)> {
        run.judgements()
            .iter()
            .map(|judgement| (judgement.grade, judgement.note))
            .collect()
    }

    #[test]
    fn the_windows_grade_each_side_of_each_edge() {
        let cases = [
            (0., Some(Grade::Perfect)),
            (50., Some(Grade::Perfect)),
            (-50., Some(Grade::Perfect)),
            (50.001, Some(Grade::Good)),
            (-50.001, Some(Grade::Good)),
            (120., Some(Grade::Good)),
            (-120., Some(Grade::Good)),
            (120.001, Some(Grade::Late)),
            (-120.001, None),
            (250., Some(Grade::Late)),
            (250.001, None),
        ];
        for (offset, grade) in cases {
            assert_eq!(W.grade(offset), grade, "{offset}");
        }
    }

    #[test]
    fn a_key_struck_for_its_note_is_judged_by_its_timing_at_each_edge() {
        let cases = [
            (1000., Grade::Perfect),
            (1050., Grade::Perfect),
            (950., Grade::Perfect),
            (1051., Grade::Good),
            (949., Grade::Good),
            (1120., Grade::Good),
            (880., Grade::Good),
            (1121., Grade::Late),
            (1250., Grade::Late),
        ];
        for (struck, grade) in cases {
            let mut judge = judge_of(&[(60, 1000.)], 0.);
            judge.at(at(0.));
            let judgement = hit(&mut judge, 60, at(struck));
            assert_eq!(judgement.grade, grade, "{struck}");
            assert_eq!(judgement.note, Some(0));
            assert_eq!(judgement.offset_ms, Some(struck - 1000.));
            assert_eq!(judgement.at_ms, 1000.);
            assert!(judgement.timed);
            assert_eq!(judgement.speed, 1.);
            // Judged once: nothing more comes of it.
            judge.at(at(5000.));
            assert_eq!(judge.run().judgements().len(), 1, "{struck}");
        }
    }

    #[test]
    fn a_key_at_the_early_edge_is_good_whatever_the_rounding() {
        // At 30 %, 120 ms of real time are 36 ms of the piece, give or take a rounding.
        let slow = |ms| Moment {
            speed: 0.3,
            ..at(ms)
        };
        let mut judge = judge_of(&[(60, 1000.)], 0.);
        judge.at(slow(0.));
        assert_eq!(
            hit(&mut judge, 60, slow(1000. - 120. * 0.3)).grade,
            Grade::Good
        );
    }

    #[test]
    fn a_key_too_early_or_too_late_or_of_another_note_is_wrong() {
        for (key, struck) in [(60, 879.), (60, 1251.), (62, 1000.)] {
            let mut judge = judge_of(&[(60, 1000.)], 0.);
            judge.at(at(0.));
            let judgement = hit(&mut judge, key, at(struck));
            assert_eq!(judgement.grade, Grade::Wrong, "{key} at {struck}");
            assert_eq!(judgement.note, None);
            assert_eq!(judgement.key, key);
            assert_eq!(judgement.at_ms, struck);
        }
        // Nothing owed at all.
        let mut judge = judge_of(&[], 0.);
        assert_eq!(hit(&mut judge, 60, at(500.)).grade, Grade::Wrong);
    }

    #[test]
    fn a_note_not_struck_is_missed_once_its_window_and_the_grace_have_passed() {
        let mut judge = judge_of(&[(60, 1000.)], 0.);
        assert!(judge.at(at(0.)).is_empty());
        // 250 ms late, and the grace for a key read late.
        assert!(judge.at(at(1000. + 250. + GRACE_MS)).is_empty());
        let missed = judge.at(at(1000. + 250. + GRACE_MS + 1.)).to_vec();
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].grade, Grade::Missed);
        assert_eq!(missed[0].note, Some(0));
        assert_eq!(missed[0].offset_ms, None);
        assert!(missed[0].timed);
        // A key read late, struck within the window, still finds its note.
        let mut judge = judge_one();
        judge.at(at(1000. + 250. + GRACE_MS));
        assert_eq!(hit(&mut judge, 60, at(1249.)).grade, Grade::Late);
    }

    fn judge_one() -> Judge {
        let mut judge = judge_of(&[(60, 1000.)], 0.);
        judge.at(at(0.));
        judge
    }

    #[test]
    fn each_note_of_a_chord_is_judged_on_its_own() {
        let mut judge = judge_of(&[(60, 1000.), (64, 1005.), (67, 1010.)], 0.);
        judge.at(at(0.));
        assert_eq!(hit(&mut judge, 64, at(1000.)).grade, Grade::Perfect);
        assert_eq!(hit(&mut judge, 60, at(1100.)).grade, Grade::Good);
        judge.at(at(3000.));
        assert_eq!(
            grades(judge.run()),
            [
                (Grade::Perfect, Some(1)),
                (Grade::Good, Some(0)),
                (Grade::Missed, Some(2))
            ]
        );
        assert_eq!(judge.run().count(Grade::Missed), 1);
        assert_eq!(judge.run().count(Grade::Perfect), 1);
    }

    #[test]
    fn a_key_repeated_fast_goes_to_each_of_its_notes_in_turn() {
        let mut judge = judge_of(&[(60, 1000.), (60, 1150.), (60, 1300.)], 0.);
        judge.at(at(0.));
        for (struck, note) in [(1010., 0), (1140., 1), (1320., 2)] {
            let judgement = hit(&mut judge, 60, at(struck));
            assert_eq!(
                (judgement.grade, judgement.note),
                (Grade::Perfect, Some(note))
            );
        }
        // One strike in a later note's perfect window, not perfect for the earlier one: the later
        // note's, and the other is missed.
        let mut judge = judge_of(&[(60, 1000.), (60, 1150.)], 0.);
        judge.at(at(0.));
        assert_eq!(hit(&mut judge, 60, at(1110.)).note, Some(1));
        judge.at(at(3000.));
        assert_eq!(
            grades(judge.run()),
            [(Grade::Perfect, Some(1)), (Grade::Missed, Some(0))]
        );
        // A second strike of a note judged already is wrong.
        let mut judge = judge_one();
        hit(&mut judge, 60, at(1000.));
        assert_eq!(hit(&mut judge, 60, at(1010.)).grade, Grade::Wrong);
    }

    #[test]
    fn a_player_steadily_late_on_a_fast_repeated_key_is_late_on_each_note() {
        let notes: Vec<(u8, f64)> = (0..6).map(|n| (60, 1000. + f64::from(n) * 150.)).collect();
        let mut judge = judge_of(&notes, 0.);
        judge.at(at(0.));
        for (n, &(_, on)) in notes.iter().enumerate() {
            // 90 ms late: nearer the next note (60 ms early) than its own.
            let judgement = hit(&mut judge, 60, at(on + 90.));
            assert_eq!(
                (judgement.grade, judgement.note, judgement.offset_ms),
                (Grade::Good, Some(n), Some(90.))
            );
        }
        judge.at(at(5000.));
        assert_eq!(judge.run().count(Grade::Missed), 0);
    }

    #[test]
    fn notes_in_unison_are_struck_as_one_key() {
        let mut judge = judge_of(&[(60, 1000.), (60, 1010.), (60, 1300.)], 0.);
        judge.at(at(0.));
        assert_eq!(hit(&mut judge, 60, at(1020.)).grade, Grade::Perfect);
        assert_eq!(
            grades(judge.run()),
            [(Grade::Perfect, Some(0)), (Grade::Perfect, Some(1))]
        );
        assert_eq!(hit(&mut judge, 60, at(1300.)).note, Some(2));
    }

    #[test]
    fn at_a_slower_speed_the_windows_cover_less_of_the_piece() {
        let half = |ms| Moment {
            speed: 0.5,
            ..at(ms)
        };
        // At 50 %, 25 ms of the piece are 50 ms of real time.
        for (struck, grade, offset) in [
            (1025., Grade::Perfect, 50.),
            (1026., Grade::Good, 52.),
            (940., Grade::Good, -120.),
            (1125., Grade::Late, 250.),
            (939., Grade::Wrong, 0.),
            (1126., Grade::Wrong, 0.),
        ] {
            let mut judge = judge_of(&[(60, 1000.)], 0.);
            judge.at(half(0.));
            let judgement = hit(&mut judge, 60, half(struck));
            assert_eq!(judgement.grade, grade, "{struck}");
            assert_eq!(judgement.speed, 0.5);
            if grade != Grade::Wrong {
                assert_eq!(judgement.offset_ms, Some(offset), "{struck}");
            }
        }
        // Missed after 250 ms of real time and the grace: 175 ms of the piece.
        let mut judge = judge_of(&[(60, 1000.)], 0.);
        judge.at(half(0.));
        assert!(judge.at(half(1175.)).is_empty());
        assert_eq!(judge.at(half(1176.))[0].grade, Grade::Missed);
    }

    #[test]
    fn the_latency_is_taken_away_from_each_key() {
        // Keys that arrive 40 ms late: struck on time.
        let mut judge = judge_of(&[(60, 1000.), (62, 2000.)], 40.);
        judge.at(at(0.));
        let judgement = hit(&mut judge, 60, at(1040.));
        assert_eq!(judgement.grade, Grade::Perfect);
        assert_eq!(judgement.offset_ms, Some(0.));
        // Without it, the same key is good, not perfect.
        let mut plain = judge_one();
        assert_eq!(hit(&mut plain, 60, at(1080.)).grade, Grade::Good);
        let mut judge_80 = judge_of(&[(60, 1000.)], 40.);
        judge_80.at(at(0.));
        assert_eq!(hit(&mut judge_80, 60, at(1080.)).grade, Grade::Perfect);
        // At half speed, 40 ms of real time are 20 ms of the piece.
        let half = |ms| Moment {
            speed: 0.5,
            ..at(ms)
        };
        let mut judge = judge_of(&[(60, 1000.)], 40.);
        judge.at(half(0.));
        assert_eq!(hit(&mut judge, 60, half(1020.)).offset_ms, Some(0.));
        // A note is missed that much later too.
        let mut judge = judge_of(&[(60, 1000.)], 40.);
        judge.at(at(0.));
        assert!(judge.at(at(1000. + 250. + GRACE_MS + 40.)).is_empty());
        assert_eq!(judge.at(at(1000. + 250. + GRACE_MS + 41.)).len(), 1);
    }

    fn wait(ms: f64) -> Moment {
        Moment {
            wait: true,
            ..at(ms)
        }
    }

    #[test]
    fn under_the_wait_a_key_of_a_note_owed_is_perfect_and_any_other_wrong() {
        let mut judge = judge_of(&[(60, 1000.), (64, 1000.)], 80.);
        judge.at(wait(0.));
        // The clock stands on the chord, the player takes his time.
        let judgement = hit(&mut judge, 60, wait(1000.));
        assert_eq!(
            (judgement.grade, judgement.offset_ms, judgement.timed),
            (Grade::Perfect, None, false)
        );
        assert_eq!(hit(&mut judge, 62, wait(1000.)).grade, Grade::Wrong);
        // As early as the wait takes a key, and no earlier.
        assert_eq!(
            hit(&mut judge, 64, wait(1000. - EARLY_MS)).grade,
            Grade::Perfect
        );
        let mut judge = judge_of(&[(60, 1000.)], 0.);
        judge.at(wait(0.));
        assert_eq!(
            hit(&mut judge, 60, wait(1000. - EARLY_MS - 1.)).grade,
            Grade::Wrong
        );
    }

    #[test]
    fn under_the_wait_a_key_struck_again_for_its_chord_is_not_judged() {
        // C, let go, E: the chord is not all down; C again releases it.
        let mut judge = judge_of(&[(60, 1000.), (64, 1000.)], 0.);
        judge.at(wait(0.));
        assert_eq!(hit(&mut judge, 60, wait(1000.)).grade, Grade::Perfect);
        assert_eq!(hit(&mut judge, 64, wait(1000.)).grade, Grade::Perfect);
        assert_eq!(judge.strike(60, wait(1000.)), None);
        assert_eq!(judge.run().judgements().len(), 2);
        // Timed, a key struck twice is wrong the second time.
        let mut judge = judge_one();
        hit(&mut judge, 60, at(1000.));
        assert_eq!(hit(&mut judge, 60, at(1000.)).grade, Grade::Wrong);
    }

    /// The loop of measures 1 and 2: 2 s to 6 s.
    fn looping(notes: &[(u8, f64)]) -> Judge {
        let mut judge = judge_of(notes, 0.);
        let looped = judge.loop_ms(1, 2);
        assert_eq!(looped, Some((2000., 6000.)));
        judge.set_loop(looped);
        judge
    }

    fn pass(seeks: u32, ms: f64) -> Moment {
        Moment { seeks, ..at(ms) }
    }

    #[test]
    fn each_pass_of_the_loop_owes_its_notes_again() {
        let mut judge = looping(&[(60, 2000.), (62, 5000.), (64, 7000.)]);
        judge.at(pass(0, 1900.));
        assert_eq!(hit(&mut judge, 60, pass(0, 2000.)).grade, Grade::Perfect);
        assert_eq!(hit(&mut judge, 62, pass(0, 5010.)).grade, Grade::Perfect);
        // The wrap: one more on the counter.
        judge.at(pass(1, 2100.));
        let again = hit(&mut judge, 60, pass(1, 2030.));
        assert_eq!(
            (again.grade, again.note, again.pass),
            (Grade::Perfect, Some(0), 1)
        );
        // Not struck this time: missed in the second pass, and the note after the loop is never
        // owed.
        judge.at(pass(1, 5900.));
        let run = judge.run().judgements();
        let last = run.last().unwrap();
        assert_eq!(
            (last.grade, last.note, last.pass),
            (Grade::Missed, Some(1), 1)
        );
        assert!(run.iter().all(|judgement| judgement.note != Some(2)));
    }

    #[test]
    fn a_loop_that_ends_with_the_piece_goes_round_without_ending_it() {
        // Measures 8 and 9: 16 s to the end, 20 s.
        let mut judge = judge_of(&[(60, 16_000.), (62, 19_800.)], 0.);
        let looped = judge.loop_ms(8, 9);
        assert_eq!(looped, Some((16_000., 20_000.)));
        judge.set_loop(looped);
        judge.at(pass(0, 15_900.));
        hit(&mut judge, 60, pass(0, 16_000.));
        // The clock reads the end for a buffer before it wraps, playing.
        assert!(judge.at(pass(0, 20_000.)).is_empty());
        // The last note, late, and the first of the next pass, early: both before the seam.
        let late = hit(&mut judge, 62, pass(0, 20_000.));
        assert_eq!((late.grade, late.pass), (Grade::Late, 0));
        let early = hit(&mut judge, 60, pass(0, 20_000.));
        assert_eq!((early.grade, early.pass), (Grade::Perfect, 1));
        judge.at(pass(1, 16_020.));
        assert_eq!(judge.run().count(Grade::Missed), 0);
        assert_eq!(judge.run().count(Grade::Wrong), 0);
    }

    #[test]
    fn a_key_near_the_seam_belongs_to_the_pass_it_is_nearest() {
        // Early for the first note of the next pass: struck before the seam, read before it.
        let mut judge = looping(&[(60, 2000.), (62, 5980.)]);
        judge.at(pass(0, 2000.));
        hit(&mut judge, 60, pass(0, 2000.));
        let early = hit(&mut judge, 60, pass(0, 5920.));
        assert_eq!(
            (early.grade, early.note, early.pass, early.offset_ms),
            (Grade::Good, Some(0), 1, Some(-80.))
        );
        // Read after the seam, the clock's position lies before the loop's start: the same.
        let mut judge = looping(&[(60, 2000.)]);
        judge.at(pass(0, 5900.));
        judge.at(pass(1, 2010.));
        let early = hit(&mut judge, 60, pass(1, 1960.));
        assert_eq!((early.grade, early.pass), (Grade::Perfect, 1));
        // A wrong key there is where it was struck, in the pass before.
        let wrong = hit(&mut judge, 62, pass(1, 1970.));
        assert_eq!(
            (wrong.grade, wrong.at_ms, wrong.pass, wrong.measure),
            (Grade::Wrong, 5970., 0, 2)
        );
        // Late for the last note of the pass before, struck after the seam.
        let mut judge = looping(&[(60, 2000.), (62, 5900.)]);
        judge.at(pass(0, 5000.));
        judge.at(pass(1, 2050.));
        let late = hit(&mut judge, 62, pass(1, 2050.));
        assert_eq!(
            (late.grade, late.note, late.pass, late.offset_ms),
            (Grade::Late, Some(1), 0, Some(150.))
        );
    }

    #[test]
    fn a_loop_made_while_playing_outside_it_owes_its_first_note() {
        let mut judge = judge_of(&[(60, 2000.)], 0.);
        judge.at(at(500.));
        // The loop is made, and the clock seeks to A; seen a little after.
        judge.set_loop(judge.loop_ms(1, 2));
        judge.at(pass(2, 2030.));
        assert_eq!(hit(&mut judge, 60, pass(2, 2030.)).grade, Grade::Perfect);
        // Made while the clock is inside it: what is behind is not owed again.
        let mut judge = judge_of(&[(60, 2000.), (62, 2100.)], 0.);
        judge.at(at(1900.));
        hit(&mut judge, 60, at(2000.));
        judge.set_loop(judge.loop_ms(1, 2));
        judge.at(at(2050.));
        judge.at(at(3000.));
        assert_eq!(
            grades(judge.run()),
            [(Grade::Perfect, Some(0)), (Grade::Missed, Some(1))]
        );
    }

    #[test]
    fn a_seek_starts_a_pass_afresh_and_drops_what_it_left_unjudged() {
        let mut judge = judge_of(&[(60, 1000.), (62, 4000.), (64, 4100.), (65, 6000.)], 0.);
        judge.at(at(900.));
        // Seeks add two to the counter. Back to measure 2 (4 s), seen 80 ms after it landed.
        judge.at(pass(2, 4080.));
        let struck = hit(&mut judge, 62, pass(2, 4090.));
        assert_eq!((struck.grade, struck.pass), (Grade::Good, 1));
        // The note at 1 s was left: never judged.
        judge.at(pass(2, 7000.));
        assert_eq!(
            grades(judge.run()),
            [
                (Grade::Good, Some(1)),
                (Grade::Missed, Some(2)),
                (Grade::Missed, Some(3))
            ]
        );
        // Back to the start: the first note is owed again.
        judge.at(pass(4, 0.));
        assert_eq!(hit(&mut judge, 60, pass(4, 1000.)).grade, Grade::Perfect);
        // Seen late, a seek never reaches into the measure before.
        let mut judge = judge_of(&[(60, 1950.), (62, 2000.)], 0.);
        judge.at(at(0.));
        judge.at(pass(2, 2050.));
        assert_eq!(hit(&mut judge, 60, pass(2, 2050.)).grade, Grade::Wrong);
        assert_eq!(hit(&mut judge, 62, pass(2, 2050.)).grade, Grade::Perfect);
    }

    #[test]
    fn a_judge_made_in_the_middle_owes_nothing_behind() {
        let mut judge = judge_of(&[(60, 1000.), (62, 1200.)], 0.);
        // Its first moment: the hands changed while the piece played.
        judge.at(at(1100.));
        assert_eq!(hit(&mut judge, 60, at(1100.)).grade, Grade::Wrong);
        judge.at(at(5000.));
        assert_eq!(
            grades(judge.run()),
            [(Grade::Wrong, None), (Grade::Missed, Some(1))]
        );
    }

    #[test]
    fn what_is_owed_when_the_piece_has_ended_is_missed_and_a_new_run_follows() {
        let mut judge = judge_of(&[(60, 1000.), (62, 19_900.)], 0.);
        judge.at(at(900.));
        hit(&mut judge, 60, at(1000.));
        judge.at(at(19_000.));
        // Stopped at the end.
        let ended = Moment {
            playing: false,
            ..at(20_000.)
        };
        let missed = judge.at(ended).to_vec();
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].grade, Grade::Missed);
        assert!(judge.finished().is_none());
        // Play again: from the start, another run.
        judge.at(pass(2, 0.));
        assert!(judge.run().is_empty());
        assert_eq!(judge.finished().map(|run| run.judgements().len()), Some(2));
        let again = hit(&mut judge, 60, pass(2, 1000.));
        assert_eq!((again.grade, again.pass), (Grade::Perfect, 0));
    }

    #[test]
    fn the_notes_owed_are_those_of_the_hands_played() {
        let n = |key, on_ms, hand| Note {
            key,
            velocity: 80,
            on_ms,
            off_ms: on_ms + 100.,
            hand,
            part: 0,
            measure: 0,
            element: None,
        };
        let timeline = Timeline::new(
            vec![
                n(48, 0., Hand::Left),
                n(72, 0., Hand::Right),
                n(50, 500., Hand::Left),
            ],
            vec![0.],
            1000.,
        );
        let keys = |playing| -> Vec<u8> {
            expected(&timeline, playing)
                .iter()
                .map(|note| note.key)
                .collect()
        };
        assert_eq!(keys(Playing::Right), [72]);
        assert_eq!(keys(Playing::Left), [48, 50]);
        assert_eq!(keys(Playing::Both), [48, 72, 50]);
        assert!(keys(Playing::Listen).is_empty());
        let clock = Clock::with_marks(1000., &Marks::default());
        assert!(Live::new(&timeline, Playing::Listen, 0., &clock).is_none());
        // The places are the timeline's.
        let left = expected(&timeline, Playing::Left);
        assert_eq!(timeline.notes()[left[1].note].key, 50);
    }

    #[test]
    fn the_live_judge_reads_keys_on_the_clock_only_while_it_plays() {
        let n = |key, on_ms| Note {
            key,
            velocity: 80,
            on_ms,
            off_ms: on_ms + 100.,
            hand: Hand::Right,
            part: 0,
            measure: 0,
            element: None,
        };
        let timeline = Timeline::new(vec![n(60, 0.), n(62, 1000.)], vec![0.], 2000.);
        let clock = Clock::with_marks(2000., &Marks::default());
        let mut live = Live::new(&timeline, Playing::Right, 0., &clock).unwrap();
        let key = |key, velocity| Played {
            key,
            velocity,
            at: Instant::now(),
        };
        // Paused: not judged.
        assert_eq!(live.key(key(60, 90), &clock), None);
        clock.play();
        // Playing at 0, standing there: the first note, perfect; let go: nothing.
        let judgement = live.key(key(60, 90), &clock).unwrap();
        assert_eq!(judgement.grade, Grade::Perfect);
        assert_eq!(live.key(key(60, 0), &clock), None);
        // At the end (the clock stops there), the note left is missed.
        let snapshot = clock.snapshot();
        clock.advance(&snapshot, 2_000_000);
        assert!(!clock.playing());
        let missed = live.tick(&clock).to_vec();
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].note, Some(1));
        assert_eq!(live.run().count(Grade::Perfect), 1);
    }
}
