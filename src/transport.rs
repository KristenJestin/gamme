//! The transport of a piece: play and pause, seek by measure, speed, practice (the hands the
//! player plays, the wait, the A–B loop: `practice.rs`), and what the built-in piano is doing
//! (its SoundFont's download, with its progress). A bar built from the kit's own components,
//! under the play screen (`play_screen.rs`).
//!
//! It holds the [`Player`], and moves its clock by the wall clock while the piano does not
//! (`wall_clock.rs`): a piece played before its SoundFont is there, or with no audio output,
//! goes on in silence. A change of the hands or of the wait is said ([`Choice`] as an event),
//! for the play screen to keep it with the piece.
//!
//! It holds the judge of the run too (`judge.rs`): each key from MIDI in goes to it with the
//! instant it arrived, and each tick tells it time moved on. Hands changed, the run starts again.
//! What the judge says is counted (`tally.rs`: score, combo, accuracy, said in the practice row)
//! and flashed (`flash.rs`: drawn by the play screen on the notes and the keys).
//!
//! When a run ends at the end of the piece, its end (`run_end.rs`) is made here, for the play
//! screen to show over the piece: what it says, its stars, its Rework. Whether the falling notes
//! were shown while the run played (the aid, which the third star asks to be gone) is followed
//! here too, the play screen saying what it shows.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_design_system::{
    Button, Icon, Segmented, Select, SelectOption, Switch, Type as _, palette, space,
};
use gpui_kit::*;

use crate::flash::{self, Flashes};
use crate::icons;
use crate::judge::Live;
use crate::midi_in::{MidiIn, Played};
use crate::player::{Player, Sound};
use crate::practice::{self, Choice, Playing, Practice};
use crate::run_end::{self, Summary};
use crate::soundfont::Stage;
use crate::tally::{self, Tally};
use crate::wall_clock::WallClock;

/// How often the view looks at the clock while something moves.
const REFRESH: Duration = Duration::from_millis(33);

/// How long the app waits for the output to close when it quits.
const QUIT_WAIT: Duration = Duration::from_millis(500);

/// The speeds offered, in percent: the clock takes any from 25 to 100. Rework slows to one of them.
const SPEEDS: [u32; 3] = [25, 50, 100];

/// The end of the run that ended last, and whether it is shown.
struct End {
    summary: Summary,
    /// Its judge's run: one more, and another run started.
    run: u64,
    shown: bool,
}

pub struct Transport {
    player: Player,
    /// Each measure's name as the score prints it, in the order of play; empty for a piece with
    /// no score, whose measures are counted.
    names: Vec<SharedString>,
    practice: Practice,
    /// Whether a keyboard is listened to is looked at here, for the wait.
    midi_in: MidiIn,
    wall: WallClock,
    /// What the player said last about its output (it was replaced), kept on screen.
    notice: Option<String>,
    /// The judge of the run; none while the player listens.
    judge: Option<Live>,
    /// What the judge said, counted.
    tally: Tally,
    /// What the judge said, shown.
    flashes: flash::Shared,
    /// What the settings measured, taken away from each key.
    latency_ms: f64,
    /// Whether the piece has a score: a MIDI file has none.
    engraved: bool,
    /// Whether the play screen shows the falling notes.
    aid: bool,
    /// Whether they were shown while the judge's run [`Transport::aided_run`] played.
    aided: bool,
    aided_run: u64,
    end: Option<End>,
    _refresh: Task<()>,
}

impl EventEmitter<Choice> for Transport {}

impl Transport {
    /// `practice` is put on the player at once; `latency_ms` is the settings'; `engraved` whether
    /// the piece has a score.
    pub fn new(
        player: Player,
        names: Vec<SharedString>,
        mut practice: Practice,
        midi_in: MidiIn,
        latency_ms: i32,
        engraved: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        practice.set_keyboard(midi_in.listening_to_any(), &player);
        practice.apply(&player);
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                let alive = this
                    .update(cx, |this, cx| {
                        let sound = this.player.sound();
                        // Said once by the player (the output was replaced), kept on screen.
                        if let Some(notice) = this.player.notice() {
                            this.notice = Some(notice);
                            cx.notify();
                        }
                        this.wall.tick(&this.player.clock);
                        if let Some(judge) = &mut this.judge {
                            judge.tick(&this.player.clock);
                        }
                        if this.catch_up() {
                            cx.notify();
                        }
                        this.follow_aid();
                        if this.look_for_the_end() {
                            cx.notify();
                        }
                        // A keyboard plugged in or out: the wait can be had, or not.
                        let keyboard = this.midi_in.listening_to_any();
                        if this.practice.set_keyboard(keyboard, &this.player) {
                            cx.notify();
                        }
                        // Idle, nothing changes: no frame to draw.
                        if this.player.wants_to_play() || sound == Sound::Preparing {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        // The app quitting does not drop the entities: a MIDI output is closed here, its keys
        // let go, or a note would go on sounding on the piano after the window is gone.
        cx.on_app_quit(|this, _| {
            this.player.shutdown(QUIT_WAIT);
            async {}
        })
        .detach();
        let latency_ms = f64::from(latency_ms);
        let judge = Live::new(
            &player.timeline,
            practice.choice().playing,
            latency_ms,
            &player.clock,
        );
        Self {
            player,
            names,
            practice,
            midi_in,
            wall: WallClock::default(),
            notice: None,
            judge,
            tally: Tally::default(),
            flashes: Rc::new(RefCell::new(Flashes::default())),
            latency_ms,
            engraved,
            aid: true,
            aided: false,
            aided_run: 0,
            end: None,
            _refresh: refresh,
        }
    }

    /// Whether the play screen shows the falling notes from now on.
    pub fn set_aid(&mut self, aid: bool) {
        self.aid = aid;
        self.follow_aid();
    }

    /// The run being played was aided once the falling notes were shown while it played.
    fn follow_aid(&mut self) {
        let run = self.judge.as_ref().map_or(0, Live::runs);
        if run != self.aided_run {
            self.aided_run = run;
            self.aided = false;
        }
        if self.aid && self.player.clock.playing() {
            self.aided = true;
        }
    }

    /// Makes the end of a run that just ended, and shows it; hides the one shown once another
    /// run started. Whether anything changed.
    fn look_for_the_end(&mut self) -> bool {
        let Some(judge) = &self.judge else {
            return false;
        };
        let run = judge.runs();
        if !judge.ended() {
            let started = self
                .end
                .as_ref()
                .is_some_and(|end| end.shown && end.run != run);
            if let Some(end) = self.end.as_mut().filter(|_| started) {
                end.shown = false;
            }
            return started;
        }
        if self.end.as_ref().is_some_and(|end| end.run == run) || judge.run().is_empty() {
            return false;
        }
        let summary = Summary::of(
            judge.run().judgements(),
            judge.owed(),
            self.aided,
            self.engraved,
            &SPEEDS,
        );
        self.end = Some(End {
            summary,
            run,
            shown: true,
        });
        true
    }

    /// Rework of the run that ended, while its end is closed and no other run started: the
    /// practice row offers it again.
    fn rework_at_hand(&self) -> Option<&run_end::Plan> {
        let judge = self.judge.as_ref()?;
        let end = self.end.as_ref()?;
        let at_hand = judge.ended()
            && end.run == judge.runs()
            && !end.shown
            && self.practice.reworking().is_none();
        end.summary.rework.as_ref().filter(|_| at_hand)
    }

    /// The end of the run that ended last, and whether it is shown; none before one ended.
    pub fn end(&self) -> Option<(&Summary, bool)> {
        self.end.as_ref().map(|end| (&end.summary, end.shown))
    }

    /// The measures as the score prints them, in the order of play, when they fit the piece.
    pub fn names(&self) -> &[SharedString] {
        if self.names.len() == self.player.timeline.measure_count() {
            &self.names
        } else {
            &[]
        }
    }

    pub fn close_end(&mut self, cx: &mut Context<Self>) {
        if let Some(end) = &mut self.end {
            end.shown = false;
        }
        cx.notify();
    }

    /// The piece again from its start, as it was played: Rework ends.
    pub fn play_again(&mut self, cx: &mut Context<Self>) {
        self.close_end(cx);
        if self.practice.reworking().is_some() {
            self.practice.clear_loop(&self.player);
        }
        self.player.clock.seek_ms(0.);
        if !self.player.wants_to_play() {
            self.toggle(cx);
        }
    }

    /// Rework of the run that ended: its measures most missed, looped, slowed, played at once.
    pub fn rework(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = self.end.as_ref().and_then(|end| end.summary.rework.clone()) else {
            return;
        };
        self.close_end(cx);
        self.practice.rework(plan, &self.player);
        if !self.player.wants_to_play() {
            self.toggle(cx);
        }
    }

    /// The judgements, as the views draw them.
    pub fn flashes(&self) -> &flash::Shared {
        &self.flashes
    }

    /// Counts and flashes what the judge said since the last time; whether it said anything.
    fn catch_up(&mut self) -> bool {
        let Some(judge) = &self.judge else {
            return false;
        };
        let new = self.tally.follow(judge.runs(), judge.run().judgements());
        let now = Instant::now();
        let mut flashes = self.flashes.borrow_mut();
        for judgement in new {
            flashes.add(judgement, now);
        }
        !new.is_empty()
    }

    pub fn player(&self) -> &Player {
        &self.player
    }

    pub fn practice(&self) -> &Practice {
        &self.practice
    }

    /// A key from MIDI in, for the judge and the wait. The judge reads the clock first: the
    /// wait's release moves it on.
    pub fn key(&mut self, played: Played, cx: &mut Context<Self>) {
        if let Some(judge) = &mut self.judge {
            judge.key(played, &self.player.clock);
        }
        let judged = self.catch_up();
        let released =
            self.practice
                .key(played.key, played.velocity, played.at, &self.player.clock);
        if judged || released {
            cx.notify();
        }
    }

    pub fn set_playing(&mut self, playing: Playing, cx: &mut Context<Self>) {
        self.practice.set_playing(playing, &self.player);
        // Other notes owed: another run, from where the piece stands.
        self.judge = Live::new(
            &self.player.timeline,
            playing,
            self.latency_ms,
            &self.player.clock,
        );
        self.tally = Tally::default();
        self.aided_run = 0;
        self.aided = false;
        if let Some(end) = &mut self.end {
            // The new judge counts its runs from 0: the end shown is no run of it.
            end.run = u64::MAX;
            end.shown = false;
        }
        cx.emit(self.practice.choice());
        cx.notify();
    }

    /// A wait that cannot be had (listening, no keyboard) is not turned on.
    pub fn set_wait(&mut self, wait: bool, cx: &mut Context<Self>) {
        if self.practice.set_wait(wait, &self.player) {
            cx.emit(self.practice.choice());
            cx.notify();
        }
    }

    /// `a` and `b`: the loop starts, or ends, with the measure the piece is in.
    pub fn set_a(&mut self, cx: &mut Context<Self>) {
        self.practice.set_a(&self.player);
        cx.notify();
    }

    pub fn set_b(&mut self, cx: &mut Context<Self>) {
        self.practice.set_b(&self.player);
        cx.notify();
    }

    pub fn clear_loop(&mut self, cx: &mut Context<Self>) {
        self.practice.clear_loop(&self.player);
        cx.notify();
    }

    /// Plays, or pauses. Until the piano is ready the clock starts at once and the wall clock
    /// moves it: the piece is seen going on, and heard from when the piano can.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.player.toggle();
        if self.player.wants_to_play() && self.player.sound() != Sound::Ready {
            self.player.clock.play();
        }
        cx.notify();
    }

    /// The measure the piece is in, as the score names it, and how many measures it plays.
    fn measure_text(&self, index: usize) -> String {
        let count = self.player.timeline.measure_count();
        match self.names.get(index) {
            Some(name) if self.names.len() == count => {
                format!("Measure {name} ({} of {count})", index + 1)
            }
            _ => format!("Measure {} of {count}", index + 1),
        }
    }

    /// Back to the start of the measure, or to the one before when already there.
    pub fn previous_measure(&mut self) {
        let clock = &self.player.clock;
        let measure = clock.measure(&self.player.timeline);
        let start = self.player.timeline.measure_start_ms(measure).unwrap_or(0.);
        let target = if clock.position_ms() - start > 500. {
            measure
        } else {
            measure.saturating_sub(1)
        };
        self.player.seek_measure(target);
    }

    pub fn next_measure(&mut self) {
        let measure = self.player.clock.measure(&self.player.timeline);
        self.player.seek_measure(measure + 1);
    }
}

impl Transport {
    /// The hands the player plays, the wait, and the loop, from the kit's components.
    fn practice_row(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let this = cx.entity();
        let choice = self.practice.choice();
        // Four choices: the kit's Segmented takes three at most, a Select the rest.
        let hands = {
            let this = this.clone();
            Select::new(
                "transport-hands",
                [
                    SelectOption::new("listen", "Listen").line("The computer plays the piece"),
                    SelectOption::new("right", "Right hand").line("The computer plays the left"),
                    SelectOption::new("left", "Left hand").line("The computer plays the right"),
                    SelectOption::new("both", "Both hands").line("The computer plays nothing"),
                ],
                choice.playing.index(),
            )
            .width(px(180.))
            .on_choose(move |index, _, cx| {
                this.update(cx, |t, cx| t.set_playing(Playing::ALL[index], cx))
            })
        };
        let wait = {
            let this = this.clone();
            let switch = Switch::new("transport-wait", choice.wait, "Wait for me (w)")
                .disabled(!self.practice.can_wait());
            let switch = if self.practice.keyboard() {
                switch
            } else {
                switch.hint("Plug in a MIDI keyboard: the wait listens to it")
            };
            switch.on_click(move |_, _, cx| {
                this.update(cx, |t, cx| {
                    let wait = !t.practice.choice().wait;
                    t.set_wait(wait, cx);
                })
            })
        };
        let waiting = self.player.clock.held().then(|| {
            div()
                .id("transport-waiting")
                .ui_text()
                .text_color(p.acc)
                .whitespace_nowrap()
                .child(format!("Waiting for {}…", choice.playing.words()))
        });
        // Names that do not fit the measures played are not used (as `measure_text`).
        let names = self.names();
        let (looped, half) = (self.practice.looped(), self.practice.half_loop());
        let reworking = self.practice.reworking();
        let label = match reworking {
            Some(plan) => run_end::rework_text(plan, names),
            None => practice::loop_text(looped, half, names),
        };
        let looped = looped.is_some() || reworking.is_some();
        // The run as it stands: score, combo, accuracy, once something was judged.
        let said = self
            .judge
            .as_ref()
            .map(|_| tally::readout(&self.tally.standing()))
            .unwrap_or_default();
        let tally = (!said.is_empty()).then(|| {
            div()
                .id("transport-judged")
                .flex()
                .items_center()
                .gap(space::MD)
                .children(said.into_iter().map(|(what, value)| {
                    div()
                        .flex()
                        .gap(space::XS)
                        .ui_text()
                        .whitespace_nowrap()
                        .child(div().text_color(p.mute).child(what))
                        .child(div().text_color(p.text).semibold().child(value))
                }))
        });
        let rework = self.rework_at_hand().map(|plan| {
            let this = this.clone();
            Button::secondary("transport-rework", format!("Rework at {} %", plan.speed))
                .leading(icons::REPEAT)
                .tooltip(format!(
                    "Loop {}, slowed",
                    run_end::measures_text(&plan.measures, names)
                ))
                .on_click(move |_, _, cx| this.update(cx, |t, cx| t.rework(cx)))
        });
        let set_a = {
            let this = this.clone();
            Button::secondary("transport-loop-a", "A")
                .tooltip("Loop from this measure (a)")
                .on_click(move |_, _, cx| this.update(cx, |t, cx| t.set_a(cx)))
        };
        let set_b = {
            let this = this.clone();
            Button::secondary("transport-loop-b", "B")
                .tooltip("Loop to this measure (b)")
                .on_click(move |_, _, cx| this.update(cx, |t, cx| t.set_b(cx)))
        };
        let clear = (looped || half.is_some()).then(|| {
            let this = this.clone();
            Button::quiet("transport-loop-clear", "Clear")
                .tooltip("No loop (l)")
                .on_click(move |_, _, cx| this.update(cx, |t, cx| t.clear_loop(cx)))
        });
        div()
            .flex()
            .items_center()
            .gap(space::MD)
            .child(
                div()
                    .ui_text()
                    .text_color(p.mute)
                    .whitespace_nowrap()
                    .child("You play (h)"),
            )
            .child(hands)
            .child(wait)
            .children(waiting)
            .children(tally)
            .child(div().flex_1())
            .child(
                div()
                    .id("transport-loop")
                    .ui_text()
                    .text_color(if looped { p.text } else { p.mute })
                    .whitespace_nowrap()
                    .child(label),
            )
            .children(rework)
            .child(set_a)
            .child(set_b)
            .children(clear)
    }
}

/// `m:ss`.
fn time_text(ms: f64) -> String {
    let seconds = (ms.max(0.) / 1e3) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// What the sound says of itself, from the state of its preparation. `output` is the MIDI
/// output the piece goes to, when it is not the built-in piano.
fn sound_text(
    sound: &Sound,
    stage: Stage,
    (done, total): (u64, u64),
    font: &crate::soundfont::Font,
    output: Option<&str>,
) -> String {
    let megabytes = |bytes: u64| format!("{:.1}", bytes as f64 / (1 << 20) as f64);
    match (sound, stage, output) {
        (Sound::Off, _, Some(output)) => format!("The piece plays on the {output}."),
        (Sound::Preparing, _, Some(output)) => format!("Opening the {output}…"),
        (Sound::Ready, _, Some(output)) => format!("Playing on the {output}."),
        (Sound::Failed(message), _, Some(output)) => {
            format!("The {output} could not be opened: {message}")
        }
        (Sound::Off, _, None) => format!(
            "The built-in piano ({}) is downloaded the first time you play or strike a key.",
            font.name
        ),
        (Sound::Preparing, Stage::Downloading, None) => format!(
            "Downloading the piano ({}): {} % ({} of {} MiB)",
            font.name,
            (done * 100).checked_div(total).unwrap_or(0),
            megabytes(done),
            megabytes(total),
        ),
        (Sound::Preparing, Stage::Verifying, None) => "Checking the download…".to_owned(),
        (Sound::Preparing, Stage::Unpacking, None) => "Unpacking the piano…".to_owned(),
        (Sound::Preparing, _, None) => "Opening the sound…".to_owned(),
        (Sound::Ready, _, None) => format!(
            "Built-in piano: {}, by {} ({})",
            font.name, font.author, font.licence
        ),
        (Sound::Failed(message), _, None) => {
            format!("The piano could not be made ready: {message}")
        }
    }
}

impl Render for Transport {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let this = cx.entity();
        let player = &self.player;
        let timeline = &player.timeline;
        let position = player.clock.position_ms();
        let measure = timeline.measure_at(position);
        let playing = player.wants_to_play();
        let speed = player.clock.speed().percent();
        let chosen = SPEEDS
            .iter()
            .position(|&s| s == speed)
            .unwrap_or(SPEEDS.len() - 1);

        let play = {
            let this = this.clone();
            Button::round(
                "transport-play",
                if playing { icons::PAUSE } else { Icon::Play },
                if playing {
                    "Pause (Space)"
                } else {
                    "Play (Space)"
                },
            )
            .on_click(move |_, _, cx| this.update(cx, |t, cx| t.toggle(cx)))
        };
        let previous = {
            let this = this.clone();
            Button::icon("transport-previous", icons::SKIP_BACK, "Previous measure").on_click(
                move |_, _, cx| {
                    this.update(cx, |t, cx| {
                        t.previous_measure();
                        cx.notify();
                    })
                },
            )
        };
        let next = {
            let this = this.clone();
            Button::icon("transport-next", icons::SKIP_FORWARD, "Next measure").on_click(
                move |_, _, cx| {
                    this.update(cx, |t, cx| {
                        t.next_measure();
                        cx.notify();
                    })
                },
            )
        };
        let speeds = {
            let this = this.clone();
            Segmented::new(
                "transport-speed",
                "Speed",
                [("25", "25 %"), ("50", "50 %"), ("100", "100 %")],
                chosen,
            )
            .on_choose(move |index, _, cx| {
                this.update(cx, |t, cx| {
                    t.player.set_speed(SPEEDS[index]);
                    cx.notify();
                })
            })
        };

        let progress = player.progress();
        let state = player.sound();
        let output = player.output_label();
        let sound = sound_text(
            &state,
            progress.stage(),
            progress.bytes(),
            player.font(),
            output.as_deref(),
        );
        let downloading = progress.stage() == Stage::Downloading && state == Sound::Preparing;
        let cancel = downloading.then(|| {
            let this = this.clone();
            Button::quiet("transport-cancel-download", "Cancel").on_click(move |_, _, cx| {
                this.update(cx, |t, cx| {
                    t.player.cancel_download();
                    cx.notify();
                })
            })
        });
        let place = format!(
            "{}  ·  {} / {}",
            self.measure_text(measure),
            time_text(position),
            time_text(timeline.duration_ms()),
        );
        let practice = self.practice_row(cx);

        div()
            .id("transport")
            .w_full()
            .flex()
            .flex_col()
            .gap(space::SM)
            .px(space::XL)
            .py(space::MD)
            .border_t_1()
            .border_color(p.line)
            .bg(p.bg)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .child(previous)
                    .child(play)
                    .child(next)
                    .child(
                        div()
                            .id("transport-position")
                            .ui_text()
                            .text_color(p.text)
                            .whitespace_nowrap()
                            .child(place),
                    )
                    .child(div().flex_1())
                    .child(speeds),
            )
            .child(practice)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .child(
                        div()
                            .id("transport-sound")
                            .flex_1()
                            .ui_text()
                            .text_color(p.mute)
                            .child(sound),
                    )
                    .children(cancel),
            )
            .children(
                self.notice
                    .clone()
                    .map(|notice| div().id("transport-notice").ui_text().child(notice)),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's own `test` attribute would come with it.
    use super::{Sound, Stage, sound_text, time_text};
    use crate::soundfont::SMALL_PIANO;

    #[test]
    fn time_reads_minutes_and_seconds() {
        assert_eq!(time_text(0.), "0:00");
        assert_eq!(time_text(61_999.), "1:01");
        assert_eq!(time_text(-5.), "0:00");
    }

    #[test]
    fn the_download_shows_its_progress() {
        let text = sound_text(
            &Sound::Preparing,
            Stage::Downloading,
            (3 << 20, 6 << 20),
            &SMALL_PIANO,
            None,
        );
        assert_eq!(
            text,
            "Downloading the piano (Upright piano (small)): 50 % (3.0 of 6.0 MiB)"
        );
        let first = sound_text(&Sound::Off, Stage::Idle, (0, 0), &SMALL_PIANO, None);
        assert!(first.contains("first time you play"));
        assert!(
            sound_text(
                &Sound::Failed("no audio output".into()),
                Stage::Idle,
                (0, 0),
                &SMALL_PIANO,
                None
            )
            .ends_with("no audio output")
        );
    }

    #[test]
    fn a_midi_output_is_named_and_has_no_download() {
        let output = Some("MIDI output “Digital Piano”");
        let said = |sound: Sound, stage| sound_text(&sound, stage, (0, 0), &SMALL_PIANO, output);
        assert_eq!(
            said(Sound::Ready, Stage::Idle),
            "Playing on the MIDI output “Digital Piano”."
        );
        assert!(!said(Sound::Off, Stage::Idle).contains("downloaded"));
        assert!(said(Sound::Preparing, Stage::Idle).starts_with("Opening the MIDI output"));
    }
}
