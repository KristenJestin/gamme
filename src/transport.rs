//! The transport of a piece: play and pause, seek by measure, speed, and what the built-in piano
//! is doing (its SoundFont's download, with its progress). A bar built from the kit's own
//! components, under the play screen (`play_screen.rs`).
//!
//! It holds the [`Player`], and moves its clock by the wall clock while the piano does not
//! (`wall_clock.rs`): a piece played before its SoundFont is there, or with no audio output,
//! goes on in silence.

use std::time::Duration;

use gpui_design_system::{Button, Icon, Segmented, Type as _, palette, space};
use gpui_kit::*;

use crate::icons;
use crate::player::{Player, Sound};
use crate::soundfont::Stage;
use crate::wall_clock::WallClock;

/// How often the view looks at the clock while something moves.
const REFRESH: Duration = Duration::from_millis(33);

/// The speeds offered, in percent: the clock takes any from 25 to 100.
const SPEEDS: [u32; 3] = [25, 50, 100];

pub struct Transport {
    player: Player,
    /// Each measure's name as the score prints it, in the order of play; empty for a piece with
    /// no score, whose measures are counted.
    names: Vec<SharedString>,
    wall: WallClock,
    _refresh: Task<()>,
}

impl Transport {
    pub fn new(player: Player, names: Vec<SharedString>, cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                let alive = this
                    .update(cx, |this, cx| {
                        let sound = this.player.sound();
                        this.wall.tick(&this.player.clock, sound != Sound::Ready);
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
        Self {
            player,
            names,
            wall: WallClock::default(),
            _refresh: refresh,
        }
    }

    pub fn player(&self) -> &Player {
        &self.player
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

/// `m:ss`.
fn time_text(ms: f64) -> String {
    let seconds = (ms.max(0.) / 1e3) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// What the piano says of itself, from the state of its preparation.
fn sound_text(
    sound: &Sound,
    stage: Stage,
    (done, total): (u64, u64),
    font: &crate::soundfont::Font,
) -> String {
    let megabytes = |bytes: u64| format!("{:.1}", bytes as f64 / (1 << 20) as f64);
    match (sound, stage) {
        (Sound::Off, _) => format!(
            "The built-in piano ({}) is downloaded the first time you play.",
            font.name
        ),
        (Sound::Preparing, Stage::Downloading) => format!(
            "Downloading the piano ({}): {} % ({} of {} MiB)",
            font.name,
            (done * 100).checked_div(total).unwrap_or(0),
            megabytes(done),
            megabytes(total),
        ),
        (Sound::Preparing, Stage::Verifying) => "Checking the download…".to_owned(),
        (Sound::Preparing, Stage::Unpacking) => "Unpacking the piano…".to_owned(),
        (Sound::Preparing, _) => "Opening the sound…".to_owned(),
        (Sound::Ready, _) => format!(
            "Built-in piano: {}, by {} ({})",
            font.name, font.author, font.licence
        ),
        (Sound::Failed(message), _) => format!("The piano could not be made ready: {message}"),
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
        let sound = sound_text(
            &player.sound(),
            progress.stage(),
            progress.bytes(),
            player.font(),
        );
        let downloading =
            progress.stage() == Stage::Downloading && player.sound() == Sound::Preparing;
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
        );
        assert_eq!(
            text,
            "Downloading the piano (Upright piano (small)): 50 % (3.0 of 6.0 MiB)"
        );
        let first = sound_text(&Sound::Off, Stage::Idle, (0, 0), &SMALL_PIANO);
        assert!(first.contains("first time you play"));
        assert!(
            sound_text(
                &Sound::Failed("no audio output".into()),
                Stage::Idle,
                (0, 0),
                &SMALL_PIANO
            )
            .ends_with("no audio output")
        );
    }
}
