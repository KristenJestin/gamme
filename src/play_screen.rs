//! The play screen: a piece played, its score and its falling notes in one of three views, the
//! transport under them.
//!
//! - Views (design §6): the score over the falling notes (the default), the falling notes alone,
//!   the score alone. A MIDI piece has no score: falling notes only. The views share one clock,
//!   so switching keeps the position, and the score keeps its pages while it is not shown.
//! - The piece is engraved and its timeline read off the main thread; the clock (`clock.rs`)
//!   drives the cursor and the falling notes, the transport (`transport.rs`) drives the clock.
//! - A MIDI piece's parts can be given other hands ("Hands"): the choice is kept with the piece
//!   in the library (`library.rs`), and the piece is made again with it, at the same place.
//! - The first play marks the piece played, so that the home screen lists it first.
//! - The keys the player strikes on a MIDI keyboard (`midi_in.rs`) light on the keyboard and sound
//!   on the piece's output, playing or not, his sustain pedal too: the output is opened with the
//!   piece when its piano is downloaded already, else at the first key struck (or play). Keys and
//!   pedal are kept apart, with the instant each arrived, for the game.
//! - Practice (`practice.rs`, its controls in the transport): the hands the player plays, the
//!   wait (each key from MIDI in goes to it as it comes), the A–B loop. The hands and the wait
//!   are kept with a piece of the library; all of it goes on when a MIDI file's parts change hands.
//! - The end of a run (`run_end.rs`), made by the transport when the piece reaches its end, is
//!   shown over the piece: its score, its stars, the measures most missed, and Rework. The
//!   transport is told whether the falling notes are shown: the third star is earned without.
//! - A run that ended is kept with the piece of the library, in its progress (`progress.rs`: the
//!   stars, the best scores, the history), and the end says when the run beat what was kept.
//!
//! Keys: Space plays and pauses, `v` changes the view, Left and Right move by measure, `+`, `-`
//! and `0` zoom the score, `h` changes the hands played, `w` waits or not, `a` and `b` set the
//! loop's first and last measures where the piece is, `l` ends the loop, Escape goes back.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui_design_system::{
    Button, EmptyLine, Icon, PageHeader, Segmented, Skeleton, Toast, look, palette, space, toast,
};
use gpui_kit::*;

use crate::clock::Clock;
use crate::config::{self, Settings};
use crate::engraver;
use crate::falling::falling;
use crate::fonts::Pixel as _;
use crate::icons;
use crate::keyboard::{Keyboard, Lights};
use crate::library::{Library, Piece};
use crate::midi_in::{self, Attached, MidiIn, Pedal, Played};
use crate::midi_out::{PortInfo, Ports};
use crate::paths;
use crate::player::{self, Player};
use crate::practice::{Choice, Practice};
use crate::progress::{Outcome, Progress, Run, Store};
use crate::run_end::{self, Summary};
use crate::score::Score;
use crate::score_view::{Playhead, ScoreView, timing};
use crate::soundfont::{self, Font};
use crate::timeline::{PartHand, Timeline};
use crate::transport::{Ended, Transport};

/// What the play screen shows of a piece.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The score over the falling notes.
    Both,
    Falling,
    Score,
}

impl View {
    const ALL: [View; 3] = [View::Both, View::Falling, View::Score];

    fn index(self) -> usize {
        Self::ALL.iter().position(|&view| view == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "both" => Some(View::Both),
            "falling" => Some(View::Falling),
            "score" => Some(View::Score),
            _ => None,
        }
    }
}

/// A piece to open on the play screen.
#[derive(Clone, Debug)]
pub struct Opening {
    pub piece: PathBuf,
    /// The piece of the library it is, if it is one: its title, its hands, and where it is
    /// marked played.
    pub library: Option<(Library, Piece)>,
    /// Where the clock starts, in ms.
    pub at: f64,
    /// Whether it plays at once.
    pub play: bool,
    pub view: View,
    /// The piano `gamme play --piano` asked for; none: the settings' choice.
    pub font: Option<Font>,
}

impl Opening {
    /// A file, at its start, in the default view, on the output the settings choose.
    pub fn file(piece: impl Into<PathBuf>) -> Self {
        Self {
            piece: piece.into(),
            library: None,
            at: 0.,
            play: false,
            view: View::Both,
            font: None,
        }
    }

    /// A piece of the library.
    pub fn piece(library: Library, piece: Piece) -> Self {
        Self {
            library: Some((library.clone(), piece.clone())),
            ..Self::file(library.file(&piece))
        }
    }

    /// `gamme <piece> [--at <seconds>] [--play] [--view both|falling|score]`.
    pub fn parse(piece: String, mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut opening = Self::file(piece);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--play" => opening.play = true,
                "--at" => {
                    let seconds = args
                        .next()
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|s| s.is_finite() && *s >= 0.)
                        .ok_or("--at takes a number of seconds")?;
                    opening.at = seconds * 1e3;
                }
                "--view" => {
                    opening.view = args
                        .next()
                        .as_deref()
                        .and_then(View::parse)
                        .ok_or("--view is both, falling or score")?;
                }
                other => return Err(format!("unknown argument {other} (see gamme --help)")),
            }
        }
        Ok(opening)
    }

    /// Whether the piece has a score: MusicXML and MEI do, MIDI does not.
    fn engraved(&self) -> bool {
        match &self.library {
            Some((_, piece)) => piece.kind.engraved(),
            None => !self
                .piece
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("mid") || ext.eq_ignore_ascii_case("midi")
                }),
        }
    }

    fn title(&self) -> String {
        match &self.library {
            Some((_, piece)) => piece.title.clone(),
            None => self.piece.file_stem().map_or_else(
                || "Piece".to_owned(),
                |stem| stem.to_string_lossy().into_owned(),
            ),
        }
    }
}

/// The play screen asks to go back to the pieces.
pub struct Back;

/// How many keys played, and pedal moves, are kept: the last ones.
const PLAYED_KEPT: usize = 512;

/// What came from MIDI in, to the window.
#[derive(Clone, Copy)]
enum Arrival {
    Key(Played),
    Pedal(Pedal),
}

/// The keys the player strikes and his pedal, as MIDI in hands them over, on its threads: lit and
/// sounded there, and sent on to the window.
struct Struck {
    lights: Lights,
    sound: Mutex<Heard>,
    arrived: async_channel::Sender<Arrival>,
}

/// Where the keys are heard, and the pedal as it stands, so that a new player starts with it.
#[derive(Default)]
struct Heard {
    /// The player's output, once there is a player.
    keys: Option<player::Keys>,
    pedal: u8,
    /// The port the pedal came from, by its id.
    pedal_from: String,
}

impl Struck {
    /// The keys are heard on `keys` from now on, the pedal held already too.
    fn hear_on(&self, keys: player::Keys) {
        let mut heard = self.sound.lock().expect("the keys' lock");
        if heard.pedal > 0 {
            keys.pedal(heard.pedal, &heard.pedal_from);
        }
        heard.keys = Some(keys);
    }
}

impl midi_in::Keys for Struck {
    fn played(&self, played: Played, from: &PortInfo) {
        if played.velocity > 0 {
            self.lights.press(played.key);
        } else {
            self.lights.release(played.key);
        }
        if let Some(keys) = &self.sound.lock().expect("the keys' lock").keys {
            keys.note(played.key, played.velocity, &from.id);
        }
        // Unbounded: never full, and it goes with the screen.
        let _ = self.arrived.try_send(Arrival::Key(played));
    }

    fn pedal(&self, pedal: Pedal, from: &PortInfo) {
        let mut heard = self.sound.lock().expect("the keys' lock");
        heard.pedal = pedal.value;
        from.id.clone_into(&mut heard.pedal_from);
        if let Some(keys) = &heard.keys {
            keys.pedal(pedal.value, &from.id);
        }
        drop(heard);
        let _ = self.arrived.try_send(Arrival::Pedal(pedal));
    }
}

/// The clock as the score's playhead.
struct ClockHead(Clock);

impl Playhead for ClockHead {
    fn position(&self) -> f64 {
        self.0.position_ms()
    }

    fn moving(&self) -> bool {
        self.0.playing()
    }
}

/// What is made off the main thread.
struct Loaded {
    score: Option<Arc<Score>>,
    timeline: Timeline,
}

/// Engraves the piece and reads its timeline, or reads a MIDI file with the hands kept for it.
fn load(piece: &Path, engraved: bool, hands: Option<&[PartHand]>) -> Result<Loaded, String> {
    if engraved {
        let engraved = engraver::shared().engrave(piece)?;
        let timeline =
            Timeline::from_engraved(&engraved.midi, &engraved.events, &engraved.written)?;
        let score = Score::place(engraved)?;
        return Ok(Loaded {
            score: Some(Arc::new(score)),
            timeline,
        });
    }
    let bytes =
        std::fs::read(piece).map_err(|e| format!("cannot read {}: {e}", piece.display()))?;
    let timeline = Timeline::from_smf(&bytes)?;
    Ok(Loaded {
        score: None,
        timeline: match hands {
            Some(hands) => timeline.with_hands(hands),
            None => timeline,
        },
    })
}

struct Ready {
    transport: Entity<Transport>,
    clock: Clock,
    timeline: Arc<Timeline>,
    score: Option<Entity<ScoreView>>,
    keyboard: Keyboard,
    _observe: Subscription,
    /// The practice chosen, kept with the piece.
    _keep: Subscription,
    /// The runs that end, kept with the piece.
    _ended: Subscription,
}

enum State {
    Loading,
    Failed(SharedString),
    Ready(Ready),
}

pub struct PlayScreen {
    opening: Opening,
    /// The MIDI ports of the machine, for the output the settings choose.
    ports: Arc<dyn Ports>,
    /// What the settings chose, read with the piece (the defaults until then).
    settings: Settings,
    title: SharedString,
    state: State,
    view: View,
    /// The keys lit from outside the piece: the ones the player strikes.
    lights: Lights,
    struck: Arc<Struck>,
    /// The keys go to `struck` while it is there.
    _attached: Attached,
    _arrivals: Task<()>,
    /// The keys played last, and the pedal's last moves, oldest first.
    played: VecDeque<Played>,
    pedal: VecDeque<Pedal>,
    /// Whether the output is opened without a play: its piano is there, or a key was struck.
    sound_wanted: bool,
    /// Whether the piece was marked played in the library.
    marked: bool,
    /// Whether the hands of the parts are shown, to be changed.
    hands_open: bool,
    /// The practice on its way to the library.
    kept: Arc<Kept>,
    /// What was kept of the piece's runs, read with it and brought up to date as they end: what
    /// a run is compared with (empty for a piece that is not of the library).
    progress: Progress,
    /// What the run that ended last did to `progress`, for its end to say.
    outcome: Option<Outcome>,
    /// Whether a keyboard is listened to, for the wait.
    midi_in: MidiIn,
    focus: FocusHandle,
}

/// A choice on its way to the library: the latest one only, written one at a time and in the
/// order chosen, so that an earlier one never lands after a later one.
#[derive(Default)]
struct Kept {
    latest: Mutex<Option<Choice>>,
    writing: Mutex<()>,
}

impl Kept {
    /// `choice` is the one to write from now on.
    fn wants(&self, choice: Choice) {
        *self.latest.lock().expect("the practice's lock") = Some(choice);
    }

    /// Writes the latest choice with `write`, unless a write before took it already. Blocking.
    fn write(&self, write: impl FnOnce(Choice)) {
        let _turn = self.writing.lock().expect("the practice's lock");
        let latest = self.latest.lock().expect("the practice's lock").take();
        if let Some(choice) = latest {
            write(choice);
        }
    }
}

impl EventEmitter<Back> for PlayScreen {}

impl PlayScreen {
    pub fn new(
        opening: Opening,
        ports: Arc<dyn Ports>,
        midi_in: &MidiIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let (piece, engraved) = (opening.piece.clone(), opening.engraved());
        let hands = opening
            .library
            .as_ref()
            .and_then(|(_, piece)| piece.hands.clone());
        let asked = opening.font;
        let id = opening.library.as_ref().map(|(_, piece)| piece.id.clone());
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            // The kept settings and the piano's file are a read: with the piece, off the main
            // thread.
            let (loaded, (settings, unreadable), installed, progress) = cx
                .background_executor()
                .spawn(async move {
                    let loaded = load(&piece, engraved, hands.as_deref());
                    let (settings, unreadable) = config::load();
                    let font = asked.unwrap_or_else(|| settings.font());
                    let installed = paths::data().is_some_and(|data| font.installed(&data));
                    // What the piece's runs kept: what the first run to end is compared with.
                    let progress = id.and_then(|id| Some(Store::open()?.load(&id)));
                    (loaded, (settings, unreadable), installed, progress)
                })
                .await;
            this.update(cx, |this, cx| {
                this.settings = settings;
                this.progress = progress.unwrap_or_default();
                this.sound_wanted |= installed;
                if let Some(why) = unreadable {
                    toast(
                        Toast::failure("The settings cannot be read")
                            .detail(format!("{why}. The defaults are used.")),
                        cx,
                    );
                }
                timing(format_args!(
                    "opened the piece in {} ms",
                    started.elapsed().as_millis()
                ));
                match loaded {
                    Ok(loaded) => this.ready(loaded, this.opening.at, this.opening.play, cx),
                    Err(error) => this.state = State::Failed(error.into()),
                }
                cx.notify();
            })
        })
        .detach();
        let lights = Lights::default();
        let (arrived, arrivals) = async_channel::unbounded();
        let struck = Arc::new(Struck {
            lights: lights.clone(),
            sound: Mutex::default(),
            arrived,
        });
        let attached = midi_in.attach(struck.clone());
        let arrivals = cx.spawn(async move |this, cx| {
            while let Ok(arrival) = arrivals.recv().await {
                let shown = this.update(cx, |this, cx| match arrival {
                    Arrival::Key(played) => this.on_played(played, cx),
                    Arrival::Pedal(pedal) => this.on_pedal(pedal),
                });
                if shown.is_err() {
                    break;
                }
            }
        });
        Self {
            title: opening.title().into(),
            view: if engraved {
                opening.view
            } else {
                View::Falling
            },
            opening,
            ports,
            settings: Settings::default(),
            state: State::Loading,
            lights,
            struck,
            _attached: attached,
            _arrivals: arrivals,
            played: VecDeque::new(),
            pedal: VecDeque::new(),
            sound_wanted: false,
            marked: false,
            hands_open: false,
            kept: Arc::default(),
            progress: Progress::default(),
            outcome: None,
            midi_in: midi_in.clone(),
            focus,
        }
    }

    /// The keys played last, oldest first, each with the instant it arrived.
    #[expect(
        dead_code,
        reason = "the keys played last, for what comes after the judge; the judge and the wait take each as it comes"
    )]
    pub fn played(&self) -> &VecDeque<Played> {
        &self.played
    }

    /// The pedal's last moves, oldest first, apart from the keys.
    #[expect(dead_code, reason = "the game's judgement reads the pedal")]
    pub fn pedal(&self) -> &VecDeque<Pedal> {
        &self.pedal
    }

    /// The pedal moved (sounded already): kept. Nothing on screen shows it.
    fn on_pedal(&mut self, pedal: Pedal) {
        if self.pedal.len() == PLAYED_KEPT {
            self.pedal.pop_front();
        }
        self.pedal.push_back(pedal);
    }

    /// A key came from MIDI in (lit and sounded already): kept, and shown.
    fn on_played(&mut self, played: Played, cx: &mut Context<Self>) {
        if self.played.len() == PLAYED_KEPT {
            self.played.pop_front();
        }
        self.played.push_back(played);
        if let State::Ready(ready) = &self.state {
            ready.transport.update(cx, |transport, cx| {
                transport.key(played, cx);
            });
        }
        if played.velocity > 0 {
            // The first key struck opens the output, if nothing did yet; the next ones are heard.
            self.sound_wanted = true;
            if let State::Ready(ready) = &self.state {
                ready.transport.read(cx).player().warm();
            }
        }
        cx.notify();
    }

    /// Makes the player of `loaded`, at `at`, playing or not.
    fn ready(&mut self, loaded: Loaded, at: f64, play: bool, cx: &mut Context<Self>) {
        let Some(data) = paths::data() else {
            self.state = State::Failed("Gamme found no home folder to keep its piano in.".into());
            return;
        };
        let names: Vec<SharedString> = loaded.score.as_ref().map_or_else(Vec::new, |score| {
            score
                .measures()
                .iter()
                .map(|m| SharedString::from(m.name.clone()))
                .collect()
        });
        let keyboard = Keyboard::for_timeline(&loaded.timeline);
        // What the settings choose: the MIDI output if there is one, else the built-in piano.
        let (audio, font) = self.settings.audio(self.ports.clone());
        let player = Player::new(
            loaded.timeline,
            data,
            self.opening.font.unwrap_or(font),
            Arc::new(soundfont::Curl::new()),
            audio,
        );
        let (clock, timeline) = (player.clock.clone(), player.timeline.clone());
        clock.seek_ms(at);
        // The keys struck sound on this player's output from now on; the last one's is closed.
        self.struck.hear_on(player.keys());
        if self.sound_wanted {
            player.warm();
        }
        // The practice of the player this one replaces goes on; else the piece's own.
        let practice = match &self.state {
            State::Ready(ready) => ready
                .transport
                .read(cx)
                .practice()
                .carried(player.chords.clone()),
            _ => Practice::new(
                self.opening
                    .library
                    .as_ref()
                    .and_then(|(_, piece)| piece.practice)
                    .unwrap_or_default(),
                player.chords.clone(),
            ),
        };
        let midi_in = self.midi_in.clone();
        let latency_ms = self.settings.latency_ms;
        let engraved = self.opening.engraved();
        let aid = self.view != View::Score;
        let transport = cx.new(|cx| {
            let mut transport =
                Transport::new(player, names, practice, midi_in, latency_ms, engraved, cx);
            transport.set_aid(aid);
            transport
        });
        // The score is drawn again with the screen: a view is drawn only when told to, and it
        // asks for its own frames only once it sees the clock move or a judgement fade.
        let observe = cx.observe(&transport, |this, _, cx| {
            if let State::Ready(Ready {
                score: Some(score), ..
            }) = &this.state
            {
                score.update(cx, |_, cx| cx.notify());
            }
            cx.notify();
        });
        let keep = cx.subscribe(&transport, |this, _, choice: &Choice, cx| {
            this.keep_practice(*choice, cx);
        });
        let ended = cx.subscribe(&transport, |this, _, ended: &Ended, cx| {
            this.keep_run(&ended.0, cx);
        });
        let flashes = transport.read(cx).flashes().clone();
        let score = loaded.score.map(|score| {
            let head = Box::new(ClockHead(clock.clone()));
            // Each note's engraved head, for its judgement.
            let heads = timeline
                .notes()
                .iter()
                .map(|note| timeline.element(note).and_then(|id| score.head(id)))
                .collect();
            cx.new(|_| ScoreView::new(score, head).judged(flashes, heads))
        });
        if play {
            transport.update(cx, |transport, cx| transport.toggle(cx));
        }
        self.state = State::Ready(Ready {
            transport,
            clock,
            timeline,
            score,
            keyboard,
            _observe: observe,
            _keep: keep,
            _ended: ended,
        });
    }

    /// A run ended: added to the piece's progress, and kept off the main thread. What it did to
    /// the progress (a new best) is what its end says; none for a file that is not a piece of the
    /// library, which keeps nothing.
    fn keep_run(&mut self, summary: &Summary, cx: &mut Context<Self>) {
        self.outcome = None;
        let Some((_, piece)) = &self.opening.library else {
            return;
        };
        let id = piece.id.clone();
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let run = Run::of(summary, at);
        self.outcome = Some(self.progress.add(run.clone()));
        cx.background_executor()
            .spawn(async move {
                let Some(store) = Store::open() else {
                    return;
                };
                if let Err(error) = store.record(&id, run) {
                    eprintln!("gamme: the run was not kept: {error}");
                }
            })
            .detach();
    }

    /// The hands played and the wait, kept with the piece, off the main thread: only the latest
    /// choice, whatever came between.
    fn keep_practice(&mut self, choice: Choice, cx: &mut Context<Self>) {
        let Some((library, piece)) = &mut self.opening.library else {
            return;
        };
        // What opens it again on this screen (its hands changed) starts with it too.
        piece.practice = Some(choice);
        let (library, id) = (library.clone(), piece.id.clone());
        self.kept.wants(choice);
        let kept = self.kept.clone();
        cx.background_executor()
            .spawn(async move {
                kept.write(|choice| {
                    if let Err(error) = library.set_practice(&id, choice) {
                        eprintln!("gamme: the practice was not kept: {error}");
                    }
                });
            })
            .detach();
    }

    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        let State::Ready(ready) = &self.state else {
            return;
        };
        if ready.score.is_none() || view == self.view {
            return;
        }
        self.view = view;
        if let Some(score) = &ready.score {
            score.update(cx, |score, cx| score.reveal(cx));
        }
        ready
            .transport
            .update(cx, |transport, _| transport.set_aid(view != View::Score));
        cx.notify();
    }

    /// Gives part `part` to `hand`: the piece is made again at the same place, paused, and the
    /// choice kept with the piece.
    fn set_hand(&mut self, part: usize, hand: PartHand, cx: &mut Context<Self>) {
        let State::Ready(ready) = &self.state else {
            return;
        };
        let mut hands = ready.timeline.hands();
        let Some(slot) = hands.get_mut(part) else {
            return;
        };
        *slot = hand;
        let timeline = ready.timeline.with_hands(&hands);
        let at = ready.clock.position_ms();
        ready
            .transport
            .update(cx, |transport, _| transport.player().clock.pause());
        let loaded = Loaded {
            score: None,
            timeline,
        };
        self.ready(loaded, at, false, cx);
        if let Some((library, piece)) = &self.opening.library {
            let (library, id) = (library.clone(), piece.id.clone());
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = library.set_hands(&id, &hands) {
                        eprintln!("gamme: the hands were not kept: {error}");
                    }
                })
                .detach();
        }
        cx.notify();
    }

    /// The first time the piece plays, the library hears of it.
    fn mark_played(&mut self, cx: &mut Context<Self>) {
        let Some((library, piece)) = &self.opening.library else {
            return;
        };
        self.marked = true;
        let (library, id) = (library.clone(), piece.id.clone());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = library.mark_played(&id, now) {
                    eprintln!("gamme: the piece was not marked played: {error}");
                }
            })
            .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            cx.emit(Back);
            return;
        }
        let State::Ready(ready) = &self.state else {
            return;
        };
        let transport = ready.transport.clone();
        let score = ready.score.clone();
        let held = &event.keystroke.modifiers;
        let chorded = held.control || held.alt || held.platform;
        match key {
            "space" => transport.update(cx, |transport, cx| transport.toggle(cx)),
            "left" | "right" => {
                transport.update(cx, |transport, cx| {
                    match key {
                        "left" => transport.previous_measure(),
                        _ => transport.next_measure(),
                    }
                    cx.notify();
                });
            }
            // A letter with Ctrl, Alt or Super is another shortcut's (the desktop's), not ours.
            "v" if !chorded => self.set_view(self.view.next(), cx),
            "h" | "w" | "a" | "b" | "l" if !chorded => {
                transport.update(cx, |transport, cx| match key {
                    "h" => {
                        let next = transport.practice().choice().playing.next();
                        transport.set_playing(next, cx);
                    }
                    "w" => {
                        let wait = !transport.practice().choice().wait;
                        transport.set_wait(wait, cx);
                    }
                    "a" => transport.set_a(cx),
                    "b" => transport.set_b(cx),
                    _ => transport.clear_loop(cx),
                })
            }
            _ => {
                // The score zooms only where it is shown.
                let shown = score.filter(|_| self.view != View::Falling);
                if let Some(score) = shown {
                    score.update(cx, |score, cx| score.zoom_key(key, cx));
                }
            }
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> PageHeader {
        let p = palette(cx);
        let this = cx.entity().downgrade();
        let back = {
            let this = this.clone();
            Button::quiet("play-back", "Pieces")
                .leading(Icon::Previous)
                .tooltip("Back to the pieces (Escape)")
                .on_click(move |_, _, cx| {
                    this.update(cx, |_, cx| cx.emit(Back)).ok();
                })
        };
        let file = self
            .opening
            .piece
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // In a row of its own, the button keeps its size rather than the header's width.
        let mut header = PageHeader::new("play-header", self.title.clone())
            .crumb(div().flex().child(back))
            .path(file);
        let status = match &self.state {
            State::Loading if self.opening.engraved() => "Engraving…",
            State::Loading => "Reading…",
            State::Failed(_) => "Not opened",
            State::Ready(ready) if ready.score.is_none() => {
                "Falling notes only: a MIDI file has no score."
            }
            State::Ready(_) => "",
        };
        if !status.is_empty() {
            header = header.below(div().pixel_text().text_color(p.mute).child(status));
        }
        if let State::Ready(ready) = &self.state {
            if ready.score.is_some() {
                let this = this.clone();
                header = header.below(
                    Segmented::new(
                        "play-view",
                        "View (v)",
                        [
                            ("both", "Score and notes"),
                            ("falling", "Falling notes"),
                            ("score", "Score"),
                        ],
                        self.view.index(),
                    )
                    .on_choose(move |index, _, cx| {
                        this.update(cx, |screen, cx| screen.set_view(View::ALL[index], cx))
                            .ok();
                    }),
                );
            } else {
                let this = this.clone();
                let open = self.hands_open;
                header = header.action(
                    "play-hands",
                    if open { "Hide hands" } else { "Hands" },
                    None,
                    move |_, cx| {
                        this.update(cx, |screen, cx| {
                            screen.hands_open = !screen.hands_open;
                            cx.notify();
                        })
                        .ok();
                    },
                );
            }
        }
        header
    }

    /// One line per part of a MIDI file: its name, and the hand that plays it.
    fn hands(&self, ready: &Ready, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let this = cx.entity().downgrade();
        let rows = ready
            .timeline
            .parts()
            .iter()
            .enumerate()
            .map(|(index, part)| {
                let chosen = match part.hand {
                    PartHand::Right => 0,
                    PartHand::Left => 1,
                    PartHand::Split => 2,
                };
                let this = this.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pixel_text()
                            .text_color(p.text)
                            .child(part.name.clone()),
                    )
                    .child(
                        Segmented::new(
                            ElementId::Name(format!("play-hand-{index}").into()),
                            format!("Hand of {}", part.name),
                            [("right", "Right"), ("left", "Left"), ("split", "Both")],
                            chosen,
                        )
                        .on_choose(move |choice, _, cx| {
                            let hand = [PartHand::Right, PartHand::Left, PartHand::Split][choice];
                            this.update(cx, |screen, cx| screen.set_hand(index, hand, cx))
                                .ok();
                        }),
                    )
            });
        div()
            .flex_none()
            .px(space::XXL)
            .py(space::LG)
            .flex()
            .flex_col()
            .gap(space::SM)
            .border_b(look().border)
            .border_color(p.line)
            .child(div().pixel_text().text_color(p.mute).child(
                "Which hand plays each part. \u{201c}Both\u{201d} splits it at the middle C.",
            ))
            .children(rows)
    }
}

impl Render for PlayScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let now = Instant::now();
        if let State::Ready(ready) = &self.state {
            let playing = ready.clock.playing();
            // The judgements fade out on a piece paused too.
            let fading = ready.transport.read(cx).flashes().borrow().any(now);
            if playing || fading {
                window.request_animation_frame();
            }
            if playing && !self.marked {
                self.mark_played(cx);
            }
        }
        let header = self.header(cx);
        let mut hands = None;
        let body = match &self.state {
            State::Loading => div()
                .flex_1()
                .p(space::XXL)
                .child(Skeleton::new("play-skeleton").w(px(640.)).h(px(320.)))
                .into_any_element(),
            State::Failed(error) => div()
                .flex_1()
                .p(space::XXL)
                .child(EmptyLine::new("play-failed", icons::MUSIC, error.clone()))
                .into_any_element(),
            State::Ready(ready) => {
                if self.hands_open && ready.score.is_none() {
                    hands = Some(self.hands(ready, cx));
                }
                let flashes = ready.transport.read(cx).flashes().borrow().clone();
                let notes = || {
                    falling(
                        ready.timeline.clone(),
                        ready.keyboard,
                        ready.clock.position_ms(),
                        self.lights.clone(),
                        flashes.clone(),
                        now,
                        &p,
                    )
                };
                let score = ready.score.clone();
                let body = div().flex_1().min_h_0().flex().flex_col();
                match (self.view, score) {
                    (View::Both, Some(score)) => {
                        body.child(div().flex_1().min_h_0().child(score)).child(
                            div()
                                .flex_none()
                                .h(relative(0.42))
                                .border_t(look().border)
                                .border_color(p.line)
                                .child(notes()),
                        )
                    }
                    (View::Score, Some(score)) => body.child(div().flex_1().min_h_0().child(score)),
                    _ => body.child(div().flex_1().min_h_0().child(notes())),
                }
                .into_any_element()
            }
        };
        let transport = match &self.state {
            State::Ready(ready) => Some(ready.transport.clone()),
            _ => None,
        };
        let end = transport.as_ref().and_then(|transport| {
            let (summary, shown) = transport.read(cx).end()?;
            let names = transport.read(cx).names();
            let act = |what: fn(&mut Transport, &mut Context<Transport>)| {
                let transport = transport.clone();
                Box::new(move |_: &mut Window, cx: &mut App| transport.update(cx, what))
                    as run_end::Action
            };
            let actions = run_end::Actions {
                rework: act(Transport::rework),
                again: act(Transport::play_again),
                close: act(Transport::close_end),
            };
            Some(run_end::view(
                summary,
                names,
                self.outcome.as_ref(),
                shown,
                actions,
                cx,
            ))
        });
        div()
            .id("play")
            .size_full()
            .bg(p.bg)
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .flex_none()
                    .px(space::XXL)
                    .pt(space::XL)
                    .pb(space::LG)
                    .border_b(look().border)
                    .border_color(p.line)
                    .child(header),
            )
            .children(hands)
            .child(body)
            .children(transport)
            .children(end)
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's own `test` attribute would come with it.
    use super::{Kept, Opening, View};
    use crate::practice::{Choice, Playing};

    fn parse(line: &str) -> Result<Opening, String> {
        let mut words = line.split_whitespace().map(str::to_owned);
        Opening::parse(words.next().unwrap(), words)
    }

    #[test]
    fn a_piece_opens_at_a_position_in_a_view_still_or_playing() {
        let still = parse("piece.musicxml").unwrap();
        assert_eq!((still.at, still.play, still.view), (0., false, View::Both));
        assert!(still.engraved());
        assert_eq!(still.title(), "piece");
        let playing = parse("piece.mxl --at 12.5 --play --view falling").unwrap();
        assert_eq!(
            (playing.at, playing.play, playing.view),
            (12_500., true, View::Falling)
        );
        for bad in [
            "p --at",
            "p --at -1",
            "p --at soon",
            "p --loud",
            "p --view",
            "p --view all",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(!parse("tune.MID").unwrap().engraved());
    }

    #[test]
    fn the_views_come_round_in_order() {
        assert_eq!(View::Both.next(), View::Falling);
        assert_eq!(View::Falling.next(), View::Score);
        assert_eq!(View::Score.next(), View::Both);
        assert_eq!(View::parse("score"), Some(View::Score));
    }

    #[test]
    fn only_the_latest_practice_chosen_is_written() {
        let kept = Kept::default();
        let choice = |playing| Choice {
            playing,
            wait: false,
        };
        // Three choices before the first write runs: one write, of the last.
        kept.wants(choice(Playing::Right));
        kept.wants(choice(Playing::Left));
        kept.wants(choice(Playing::Both));
        let mut written = Vec::new();
        for _ in 0..3 {
            kept.write(|choice| written.push(choice.playing));
        }
        assert_eq!(written, [Playing::Both]);
        kept.wants(choice(Playing::Listen));
        kept.write(|choice| written.push(choice.playing));
        assert_eq!(written, [Playing::Both, Playing::Listen]);
    }
}
