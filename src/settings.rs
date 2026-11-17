//! The settings page: MIDI (the output the piece sounds on, the input the keys come from), sound
//! (which piano the built-in one plays, and the touch of your keys on it), the latency of your keys
//! and colours (Omarchy's or Gamme's), from the kit's
//! components. Opened from the home screen's header (`screens.rs`), which it replaces until "Back"
//! (or Escape).
//!
//! The latency is measured by playing along (`latency.rs`): "Calibrate" plays clicks on the output
//! a piece plays on (a player of their own, the built-in piano made ready first if it must be),
//! the keys of MIDI in come here meanwhile, each placed on the clicks' clock as it arrives, and at
//! the last click what was measured is kept, or nothing when too few keys came.
//!
//! A choice is used and kept at once, in `$XDG_CONFIG_HOME/gamme/settings.json` (`config.rs`)
//! and, for the colours, in the state folder (`appearance.rs`). Listing the MIDI ports talks to the
//! system, so it is done off the main thread, when the page opens and on "Look again".

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_design_system::{
    Button, Field, Icon, PageHeader, Section, Select, SelectOption, Toast, Type as _, palette,
    space, toast,
};
use gpui_kit::*;

use crate::clock::Clock;
use crate::config::{self, Settings};
use crate::midi_in::{self, Attached, MidiIn, Pedal, Played};
use crate::midi_out::{self, PortInfo, Ports};
use crate::player::{Player, Sound};
use crate::touch::Touch;
use crate::{appearance, latency, paths, soundfont};

/// How often a calibration is looked at while its clicks play.
const WATCH_EVERY: Duration = Duration::from_millis(33);

/// Saves are numbered as they are asked for, so that one that comes late cannot be written
/// over a newer one.
static ASKED: AtomicU64 = AtomicU64::new(0);
static WRITTEN: Mutex<u64> = Mutex::new(0);

/// The options of a select of ports, what each stands for, and the one chosen.
#[derive(Debug, PartialEq, Eq)]
struct Choice {
    /// The first is `none`'s: no port.
    values: Vec<Option<String>>,
    titles: Vec<String>,
    chosen: usize,
}

/// The ports as a select offers them: `none` first, then each port by its key (identical devices
/// are told apart, `midi_out::keys`), then the port saved when it is not among them (a piano that
/// is unplugged is still the choice).
fn port_choice(ports: &[PortInfo], saved: Option<&str>, none: &str) -> Choice {
    let mut values = vec![None];
    let mut titles = vec![none.to_owned()];
    for key in midi_out::keys(ports) {
        titles.push(key.clone());
        values.push(Some(key));
    }
    let mut chosen = 0;
    if let Some(saved) = saved {
        match midi_out::find(ports, saved) {
            Some(port) => chosen = 1 + ports.iter().position(|p| p == port).unwrap_or(0),
            None => {
                values.push(Some(saved.to_owned()));
                titles.push(format!("{saved} (not connected)"));
                chosen = values.len() - 1;
            }
        }
    }
    Choice {
        values,
        titles,
        chosen,
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.0} MiB", bytes as f64 / (1 << 20) as f64)
}

/// The way back is asked: the window shows the home screen again.
pub struct Back;

/// The keys struck during a calibration, on MIDI's threads: each placed on the clicks' clock as
/// it arrived, once the clicks play. They are not sounded: the clicks are what is heard.
struct Strikes {
    clock: Clock,
    struck: Arc<Mutex<Vec<f64>>>,
}

impl midi_in::Keys for Strikes {
    fn played(&self, played: Played, _: &PortInfo) {
        if played.velocity > 0 && self.clock.playing() {
            let (_, ms) = self.clock.position_at(played.at);
            self.struck.lock().expect("the strikes' lock").push(ms);
        }
    }

    fn pedal(&self, _: Pedal, _: &PortInfo) {}
}

/// A calibration of the latency, while its clicks play.
struct Calibration {
    /// Told apart from one stopped and started again.
    turn: u64,
    /// The clicks, on the output the settings choose.
    player: Player,
    struck: Arc<Mutex<Vec<f64>>>,
    /// The keys come to `struck` while it is there.
    _attached: Attached,
}

/// What the latency's row says of a value: "+32 ms", "0 ms".
fn latency_text(ms: i32) -> String {
    if ms > 0 {
        format!("+{ms} ms")
    } else {
        format!("{ms} ms")
    }
}

pub struct SettingsPage {
    focus: FocusHandle,
    ports: Arc<dyn Ports>,
    /// Told at once of the input chosen.
    midi_in: MidiIn,
    config: Option<PathBuf>,
    settings: Settings,
    /// Whether the kept settings were read: nothing is chosen before, or the defaults shown
    /// meanwhile would be kept over them.
    read: bool,
    outputs: Vec<PortInfo>,
    inputs: Vec<PortInfo>,
    /// Whether the ports are being listed, and whether they have been once.
    looking: bool,
    listed: bool,
    /// Which pianos are downloaded already, as `soundfont::FONTS` lists them.
    downloaded: Vec<bool>,
    calibration: Option<Calibration>,
    calibrations: u64,
    /// What the last calibration came to.
    calibrated: Option<String>,
}

impl EventEmitter<Back> for SettingsPage {}

impl SettingsPage {
    pub fn new(
        ports: Arc<dyn Ports>,
        midi_in: MidiIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // For Escape.
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let config = paths::config();
        let mut page = Self {
            focus,
            ports,
            midi_in,
            config,
            settings: Settings::default(),
            read: false,
            outputs: Vec::new(),
            inputs: Vec::new(),
            looking: false,
            listed: false,
            downloaded: Vec::new(),
            calibration: None,
            calibrations: 0,
            calibrated: None,
        };
        page.read_kept(cx);
        page.look(cx);
        page
    }

    /// Reads the kept settings, off the main thread; one that cannot be read is said, and the
    /// defaults are shown.
    fn read_kept(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.config.clone() else {
            self.read = true;
            return;
        };
        cx.spawn(async move |this, cx| {
            let kept = cx
                .background_executor()
                .spawn(async move { config::read(&folder) })
                .await;
            this.update(cx, |this, cx| {
                match kept {
                    Ok(settings) => this.settings = settings,
                    Err(why) => toast(
                        Toast::failure("The settings cannot be read")
                            .detail(format!("{why}. The defaults are shown.")),
                        cx,
                    ),
                }
                this.read = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Lists the MIDI ports again, and looks at which pianos are downloaded.
    fn look(&mut self, cx: &mut Context<Self>) {
        if self.looking {
            return;
        }
        self.looking = true;
        cx.notify();
        let ports = self.ports.clone();
        let data = paths::data();
        cx.spawn(async move |this, cx| {
            let (outputs, inputs, downloaded) = cx
                .background_executor()
                .spawn(async move {
                    let downloaded: Vec<bool> = soundfont::FONTS
                        .iter()
                        .map(|font| data.as_deref().is_some_and(|data| font.installed(data)))
                        .collect();
                    (ports.outputs(), ports.inputs(), downloaded)
                })
                .await;
            this.update(cx, |this, cx| {
                this.outputs = outputs;
                this.inputs = inputs;
                this.downloaded = downloaded;
                this.looking = false;
                this.listed = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        cx.emit(Back);
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            self.back(cx);
        }
    }

    fn choose_output(&mut self, port: Option<String>, cx: &mut Context<Self>) {
        if self.read && self.settings.midi_out != port {
            self.settings.midi_out = port;
            self.keep(cx);
        }
    }

    fn choose_input(&mut self, port: Option<String>, cx: &mut Context<Self>) {
        if self.read && self.settings.midi_in != port {
            self.midi_in.choose(port.clone());
            self.settings.midi_in = port;
            self.keep(cx);
        }
    }

    fn choose_piano(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.read && self.settings.piano != id {
            self.settings.piano = id.to_owned();
            self.keep(cx);
        }
    }

    fn choose_touch(&mut self, touch: Touch, cx: &mut Context<Self>) {
        if self.read && self.settings.touch != touch {
            self.settings.touch = touch;
            self.keep(cx);
        }
    }

    /// Starts a calibration of the latency, or stops the one playing (nothing is kept then).
    fn calibrate(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        if self.calibration.take().is_some() {
            self.calibrated = Some("Stopped: the latency is as it was.".into());
            return;
        }
        if !self.read {
            return;
        }
        let Some(data) = paths::data() else {
            toast(
                Toast::failure("Nowhere to keep the piano")
                    .detail("Gamme found no home folder to keep its piano in."),
                cx,
            );
            return;
        };
        let (audio, font) = self.settings.audio(self.ports.clone());
        let mut player = Player::new(
            latency::timeline(),
            data,
            font,
            Arc::new(soundfont::Curl::new()),
            audio,
        );
        let struck = Arc::new(Mutex::new(Vec::new()));
        let attached = self.midi_in.attach(Arc::new(Strikes {
            clock: player.clock.clone(),
            struck: struck.clone(),
        }));
        // The clock starts once the output is ready.
        player.play();
        self.calibrations += 1;
        let turn = self.calibrations;
        self.calibration = Some(Calibration {
            turn,
            player,
            struck,
            _attached: attached,
        });
        self.calibrated = None;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_EVERY).await;
                let going = this.update(cx, |this, cx| this.watch(turn, cx));
                if !going.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Looks at calibration `turn`: false once it is over. At its last click, what it measured is
    /// kept.
    fn watch(&mut self, turn: u64, cx: &mut Context<Self>) -> bool {
        let Some(calibration) = self.calibration.as_ref().filter(|c| c.turn == turn) else {
            return false;
        };
        // The count of keys moves on screen.
        cx.notify();
        let player = &calibration.player;
        let sound = player.sound();
        if let Sound::Failed(why) = sound {
            self.calibration = None;
            self.calibrated = Some(format!("The clicks could not be played: {why}"));
            return false;
        }
        let clock = &player.clock;
        let over = sound == Sound::Ready
            && !player.wants_to_play()
            && clock.position_ms() >= player.timeline.duration_ms();
        if !over {
            return true;
        }
        let struck = calibration
            .struck
            .lock()
            .expect("the strikes' lock")
            .clone();
        // What was measured is that output's: the one chosen, or the built-in piano that took
        // its place (and why).
        let output = player
            .output_label()
            .unwrap_or_else(|| "built-in piano".to_owned());
        let notice = player
            .notice()
            .map(|notice| format!(" {notice}"))
            .unwrap_or_default();
        // Its output is closed with it.
        self.calibration = None;
        let counted = &latency::clicks()[latency::COUNT_IN..];
        match latency::measure(counted, &struck) {
            Some(ms) => {
                self.calibrated = Some(format!(
                    "Measured {} on the {output}, and kept.{notice}",
                    latency_text(ms)
                ));
                if self.settings.latency_ms != ms {
                    self.settings.latency_ms = ms;
                    self.keep(cx);
                }
            }
            None => {
                self.calibrated = Some(format!(
                    "Too few keys struck with the clicks (at least {} are needed): the latency \
                     is as it was.{notice}",
                    latency::ENOUGH
                ));
            }
        }
        false
    }

    /// What the latency's row says under it.
    fn latency_hint(&self) -> String {
        let Some(calibration) = &self.calibration else {
            if !self.midi_in.listening_to_any() {
                return "Plug in a MIDI keyboard, then press Look again: the calibration listens \
                        to it."
                    .to_owned();
            }
            return self.calibrated.clone().unwrap_or_else(|| {
                "Strike any key with each click. Your own offset, early or late (the sound, \
                 your hand, the keyboard), is taken away when your keys are judged. Measure it \
                 again when you change your keyboard, your output or your speakers."
                    .to_owned()
            });
        };
        let player = &calibration.player;
        if player.sound() != Sound::Ready {
            return "Getting the sound ready…".to_owned();
        }
        let struck = calibration.struck.lock().expect("the strikes' lock").len();
        let beat = (player.clock.position_ms() / latency::CLICK_EVERY_MS) as usize;
        if beat <= latency::COUNT_IN {
            format!(
                "Listen to {} clicks, then strike a key with each click.",
                latency::COUNT_IN
            )
        } else {
            format!("Strike a key with each click: {struck} so far.")
        }
    }

    /// Keeps the settings, off the main thread; a failure is said.
    fn keep(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let Some(folder) = self.config.clone() else {
            toast(
                Toast::failure("Nowhere to keep the settings")
                    .detail("Gamme found no home folder to keep them in."),
                cx,
            );
            return;
        };
        let settings = self.settings.clone();
        let turn = ASKED.fetch_add(1, Relaxed) + 1;
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move {
                    let mut written = WRITTEN.lock().expect("the saves' lock");
                    if *written > turn {
                        return Ok(());
                    }
                    *written = turn;
                    config::save(&folder, &settings)
                })
                .await;
            if let Err(error) = saved {
                this.update(cx, |_, cx| {
                    toast(
                        Toast::failure("The settings were not kept").detail(error.to_string()),
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }

    fn select(
        id: &'static str,
        choice: Choice,
        cx: &mut Context<Self>,
        on_choose: impl Fn(&mut Self, Option<String>, &mut Context<Self>) + 'static,
    ) -> Select {
        let this = cx.entity().downgrade();
        let Choice {
            values,
            titles,
            chosen,
        } = choice;
        let options = titles
            .into_iter()
            .enumerate()
            .map(|(at, title)| SelectOption::new(format!("{id}-{at}"), title));
        Select::new(id, options, chosen).on_choose(move |index, _, cx| {
            let port = values[index].clone();
            this.update(cx, |this, cx| on_choose(this, port, cx)).ok();
        })
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let hint = |text: String| div().ui_text().text_color(p.mute).child(text);

        let output = port_choice(
            &self.outputs,
            self.settings.midi_out.as_deref(),
            "Built-in piano",
        );
        let saved = self.settings.midi_out.as_deref();
        let missing = saved.filter(|saved| midi_out::find(&self.outputs, saved).is_none());
        let output_hint = if !self.listed {
            "Looking for MIDI ports…".to_owned()
        } else if let Some(port) = missing {
            format!("{port} is not connected: the built-in piano plays until it is.")
        } else if saved.is_some() {
            "The piece sounds on this port, and not on your speakers.".to_owned()
        } else if self.outputs.is_empty() {
            "No MIDI output found. Connect a piano and press Look again.".to_owned()
        } else {
            "Choose a piano with its own speakers to play the piece on it.".to_owned()
        };
        let output = Self::select("settings-output", output, cx, |this, port, cx| {
            this.choose_output(port, cx)
        });

        let input = port_choice(&self.inputs, self.settings.midi_in.as_deref(), "Every port");
        let input_hint = if self.listed && self.inputs.is_empty() {
            "No MIDI input found. Connect a keyboard and press Look again.".to_owned()
        } else {
            "Where your keys are read from: every port as it appears, or only this one.".to_owned()
        };
        let input = Self::select("settings-input", input, cx, |this, port, cx| {
            this.choose_input(port, cx)
        });

        let current = self.settings.font();
        let pianos = soundfont::FONTS.iter().enumerate().map(|(at, font)| {
            let state = if self.downloaded.get(at).copied().unwrap_or(false) {
                "downloaded"
            } else {
                "downloaded the first time you play"
            };
            SelectOption::new(font.id, font.name)
                .line(format!("{}, {state}", mib(font.archive_bytes)))
        });
        let chosen = soundfont::FONTS
            .iter()
            .position(|font| font.id == current.id)
            .unwrap_or(0);
        let this = cx.entity().downgrade();
        let piano = Select::new("settings-piano", pianos, chosen).on_choose(move |index, _, cx| {
            let id = soundfont::FONTS[index].id;
            this.update(cx, |this, cx| this.choose_piano(id, cx)).ok();
        });
        let piano_hint = if self.settings.midi_out.is_some() {
            "Played by the built-in piano, which a MIDI output replaces; it takes over if the \
             port goes."
        } else {
            "The built-in piano's sound. A piano is downloaded once, then kept."
        };

        let touches = Touch::ALL
            .iter()
            .map(|touch| SelectOption::new(touch.id(), touch.name()).line(touch.line()));
        let chosen = Touch::ALL
            .iter()
            .position(|touch| *touch == self.settings.touch)
            .unwrap_or(0);
        let this = cx.entity().downgrade();
        let touch =
            Select::new("settings-touch", touches, chosen).on_choose(move |index, _, cx| {
                let touch = Touch::ALL[index];
                this.update(cx, |this, cx| this.choose_touch(touch, cx))
                    .ok();
            });
        let touch_hint = "How hard your keys sound on the built-in piano: Light lifts soft keys, \
             Heavy asks for a firmer touch, Fixed plays every key at one volume. It does not \
             change the piece, nor a MIDI output, whose own touch setting applies.";

        let calibrate = Button::secondary(
            "settings-calibrate",
            if self.calibration.is_some() {
                "Stop"
            } else {
                "Calibrate"
            },
        )
        .disabled(!self.read || (self.calibration.is_none() && !self.midi_in.listening_to_any()))
        .on_click(cx.listener(|this, _, _, cx| this.calibrate(cx)));
        let latency_hint = self.latency_hint();

        let back = {
            let this = cx.entity().downgrade();
            Button::quiet("settings-back", "Pieces")
                .leading(Icon::Previous)
                .tooltip("Back to the pieces (Escape)")
                .on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| this.back(cx)).ok();
                })
        };
        let look = Button::quiet(
            "settings-look",
            if self.looking {
                "Looking…"
            } else {
                "Look again"
            },
        )
        .disabled(self.looking)
        .on_click(cx.listener(|this, _, _, cx| this.look(cx)));

        div()
            .id("settings")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .bg(p.bg)
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(space::COLUMN)
                    .max_w_full()
                    .min_h_full()
                    .px(space::XL)
                    .pt(space::XXXL)
                    .pb(space::XXXL)
                    .flex()
                    .flex_col()
                    .gap(space::XL)
                    .child(
                        PageHeader::new("settings-header", "Settings")
                            .crumb(div().flex().child(back)),
                    )
                    .child(
                        Section::new("settings-midi", "MIDI")
                            .sentence("Your piano and your keyboard, as the system lists them.")
                            .action(look)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(space::LG)
                                    .child(Field::new("settings-output-field", "Output", output))
                                    .child(hint(output_hint))
                                    .child(Field::new("settings-input-field", "Input", input))
                                    .child(hint(input_hint)),
                            ),
                    )
                    .child(
                        Section::new("settings-sound", "Sound")
                            .sentence("The built-in piano, played through your speakers.")
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(space::LG)
                                    .child(Field::new("settings-piano-field", "Piano", piano))
                                    .child(hint(piano_hint.to_owned()))
                                    .child(Field::new("settings-touch-field", "Touch", touch))
                                    .child(hint(touch_hint.to_owned())),
                            ),
                    )
                    .child(
                        Section::new("settings-latency", "Latency")
                            .sentence(
                                "Your own offset from the beat, taken away when your keys are \
                                 judged.",
                            )
                            .action(calibrate)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(space::SM)
                                    .child(
                                        div()
                                            .ui_text()
                                            .text_color(p.text)
                                            .child(latency_text(self.settings.latency_ms)),
                                    )
                                    .child(hint(latency_hint)),
                            ),
                    )
                    .child(
                        Section::new("settings-colours", "Colours")
                            .sentence("Gamme's own colours, or your desktop's theme as it changes.")
                            .child(appearance::control(cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's own `test` attribute would come with it.
    use super::{PortInfo, latency_text, mib, port_choice};
    use crate::soundfont;

    fn port(id: &str, name: &str) -> PortInfo {
        PortInfo {
            id: id.into(),
            name: name.into(),
        }
    }

    fn ports() -> Vec<PortInfo> {
        vec![
            port("14:0", "Midi Through 14:0"),
            port("24:0", "Digital Piano MIDI 1 24:0"),
        ]
    }

    #[test]
    fn nothing_saved_is_the_first_option() {
        let choice = port_choice(&ports(), None, "Built-in piano");
        assert_eq!(choice.chosen, 0);
        assert_eq!(choice.titles[0], "Built-in piano");
        assert_eq!(choice.values[0], None);
        // The ports are offered by the name that is kept, without ALSA's numbers.
        assert_eq!(choice.titles[1..], ["Midi Through", "Digital Piano MIDI 1"]);
        assert_eq!(
            choice.values[1..],
            [
                Some("Midi Through".to_owned()),
                Some("Digital Piano MIDI 1".to_owned())
            ]
        );
    }

    #[test]
    fn identical_devices_are_two_options_each_chosen_by_its_own_key() {
        let twins = [
            port("24:0", "Twin Keys MIDI 1 24:0"),
            port("32:0", "Twin Keys MIDI 1 32:0"),
        ];
        let choice = port_choice(&twins, None, "Every port");
        assert_eq!(
            choice.titles,
            ["Every port", "Twin Keys MIDI 1", "Twin Keys MIDI 1 (2)"]
        );
        assert_eq!(
            choice.values[1..],
            [
                Some("Twin Keys MIDI 1".to_owned()),
                Some("Twin Keys MIDI 1 (2)".to_owned())
            ]
        );
        // What was kept is chosen again: the second one is not mistaken for the first.
        let second = port_choice(&twins, Some("Twin Keys MIDI 1 (2)"), "Every port");
        assert_eq!(second.chosen, 2);
        assert_eq!(second.values.len(), 3);
        // An older file's plain name is the first.
        let first = port_choice(&twins, Some("Twin Keys MIDI 1"), "Every port");
        assert_eq!(first.chosen, 1);
        // One of them alone: the second is not connected, and says so.
        let alone = port_choice(&twins[1..], Some("Twin Keys MIDI 1 (2)"), "Every port");
        assert_eq!(alone.titles[2], "Twin Keys MIDI 1 (2) (not connected)");
        assert_eq!(alone.chosen, 2);
    }

    #[test]
    fn a_port_saved_is_chosen_whatever_its_numbers_are_now() {
        let choice = port_choice(&ports(), Some("Digital Piano MIDI 1 20:0"), "Every port");
        assert_eq!(choice.chosen, 2);
        assert_eq!(choice.values.len(), 3);
    }

    #[test]
    fn a_port_saved_that_is_unplugged_is_still_the_choice_and_says_so() {
        let choice = port_choice(&ports(), Some("Old piano"), "Built-in piano");
        assert_eq!(choice.chosen, 3);
        assert_eq!(choice.values[3], Some("Old piano".to_owned()));
        assert_eq!(choice.titles[3], "Old piano (not connected)");
        let none = port_choice(&[], Some("Old piano"), "Built-in piano");
        assert_eq!(none.chosen, 1);
    }

    #[test]
    fn a_latency_is_said_with_its_sign() {
        assert_eq!(latency_text(32), "+32 ms");
        assert_eq!(latency_text(0), "0 ms");
        assert_eq!(latency_text(-15), "-15 ms");
    }

    #[test]
    fn a_size_is_said_in_mebibytes() {
        assert_eq!(mib(soundfont::SMALL_PIANO.archive_bytes), "6 MiB");
        assert_eq!(mib(soundfont::CONCERT_GRAND.archive_bytes), "296 MiB");
    }
}
