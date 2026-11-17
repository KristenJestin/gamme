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
//! Its tally is a line of the practice row until the live judgements come (#18).

use std::time::Duration;

use gpui_design_system::{
    Button, Icon, Segmented, Select, SelectOption, Switch, Type as _, palette, space,
};
use gpui_kit::*;

use crate::icons;
use crate::judge::{Grade, Live};
use crate::midi_in::{MidiIn, Played};
use crate::player::{Player, Sound};
use crate::practice::{self, Choice, Playing, Practice};
use crate::soundfont::Stage;
use crate::wall_clock::WallClock;

/// How often the view looks at the clock while something moves.
const REFRESH: Duration = Duration::from_millis(33);

/// How long the app waits for the output to close when it quits.
const QUIT_WAIT: Duration = Duration::from_millis(500);

/// The speeds offered, in percent: the clock takes any from 25 to 100.
const SPEEDS: [u32; 3] = [25, 50, 100];

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
    /// What the settings measured, taken away from each key.
    latency_ms: f64,
    _refresh: Task<()>,
}

impl EventEmitter<Choice> for Transport {}

impl Transport {
    /// `practice` is put on the player at once; `latency_ms` is the settings'.
    pub fn new(
        player: Player,
        names: Vec<SharedString>,
        mut practice: Practice,
        midi_in: MidiIn,
        latency_ms: i32,
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
                        if let Some(judge) = &mut this.judge
                            && !judge.tick(&this.player.clock).is_empty()
                        {
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
            latency_ms,
            _refresh: refresh,
        }
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
        let judged = self
            .judge
            .as_mut()
            .and_then(|judge| judge.key(played, &self.player.clock));
        let released =
            self.practice
                .key(played.key, played.velocity, played.at, &self.player.clock);
        if judged.is_some() || released {
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
        let names: &[SharedString] = if self.names.len() == self.player.timeline.measure_count() {
            &self.names
        } else {
            &[]
        };
        let (looped, half) = (self.practice.looped(), self.practice.half_loop());
        let label = practice::loop_text(looped, half, names);
        // Until the live judgements (#18): how many of each, once there is one.
        let tally = self
            .judge
            .as_ref()
            .map(Live::run)
            .filter(|run| !run.is_empty())
            .map(|run| {
                let words: Vec<String> = Grade::ALL
                    .iter()
                    .map(|&grade| format!("{} {}", run.count(grade), grade.word()))
                    .collect();
                div()
                    .id("transport-judged")
                    .ui_text()
                    .text_color(p.mute)
                    .whitespace_nowrap()
                    .child(words.join(" · "))
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
        let clear = (looped.is_some() || half.is_some()).then(|| {
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
                    .text_color(if looped.is_some() { p.text } else { p.mute })
                    .whitespace_nowrap()
                    .child(label),
            )
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
