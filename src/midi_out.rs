//! MIDI out: the piece, played on a MIDI output (a piano with its own speakers) instead of the
//! built-in one, as another implementation of [`Audio`], the seam `player.rs` opens its sound
//! through.
//!
//! Nothing renders here, so nothing paces the clock as the audio callback does: a thread of its
//! own does, by the wall clock, and feeds the very same [`Renderer`] as the built-in piano (the
//! hands, the held keys, the seeks and the pauses behave alike), at a virtual rate of 1 kHz so
//! that a note goes out within a millisecond of its time. The thread is not the audio callback's:
//! the callback never calls MIDI, and this one may block on the port as it likes.
//!
//! What sounds is always let go of: on a pause, a seek or the end of the piece (the renderer
//! releases every key), and when the output closes (a different port is chosen, the player goes):
//! a note-off for each key that sounds, then All Notes Off.
//!
//! A port that disappears is told by a failed send, or by the port missing from the list (an
//! unplugged USB piano does not always fail a send): `lost` is set, and the player goes on with
//! the built-in piano and says so.
//!
//! The MIDI library (`midir`, on ALSA) is behind [`Ports`] and [`Wire`], so that the tests open
//! no real port.

use std::any::Any;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::clock::Clock;
use crate::piano::{self, Hands, Renderer, Sink};
use crate::player::Audio;
use crate::timeline::Timeline;

/// The name Gamme gives its MIDI clients.
const CLIENT: &str = "Gamme";
/// The MIDI channel the piece is on (the first).
const CHANNEL: u8 = 0;
/// The renderer runs at this rate: one frame is one millisecond.
const FRAME_RATE: u32 = 1_000;
const FRAME: Duration = Duration::from_millis(1);
/// How often the thread looks at the clock while it plays, and while it does not.
const PLAYING_TICK: Duration = Duration::from_millis(1);
const IDLE_TICK: Duration = Duration::from_millis(10);
/// The longest time the thread catches up on after a stall (a suspended laptop, a busy machine):
/// what is past it is dropped, not played all at once.
const MAX_CATCH_UP: usize = 50;
/// How often the thread looks for its port in the list.
const WATCH_EVERY: Duration = Duration::from_secs(1);

/// A MIDI port, as the system lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    /// What connects to it, for as long as the port is there.
    pub id: String,
    pub name: String,
}

impl PortInfo {
    /// What is kept of it in the settings: its name without the numbers ALSA changes.
    pub fn key(&self) -> String {
        key(&self.name)
    }
}

/// A port's name as the settings keep it: ALSA ends the names with the "client:port" numbers
/// ("Digital Piano MIDI 1 20:0"), which are not the same at the next start.
pub fn key(name: &str) -> String {
    let numbers = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    match name.rsplit_once(' ') {
        Some((head, tail))
            if tail
                .split_once(':')
                .is_some_and(|(client, port)| numbers(client) && numbers(port)) =>
        {
            head.to_owned()
        }
        _ => name.to_owned(),
    }
}

/// The port a saved name stands for: the same name, or the same name past the numbers.
pub fn find<'a>(ports: &'a [PortInfo], saved: &str) -> Option<&'a PortInfo> {
    let saved = key(saved);
    ports
        .iter()
        .find(|port| port.name == saved)
        .or_else(|| ports.iter().find(|port| port.key() == saved))
}

/// An open connection to a MIDI output.
pub trait Wire: Send {
    fn send(&mut self, message: &[u8]) -> Result<(), String>;
}

/// The MIDI ports of the machine.
pub trait Ports: Send + Sync {
    fn outputs(&self) -> Vec<PortInfo>;
    fn inputs(&self) -> Vec<PortInfo>;
    fn connect_output(&self, port: &PortInfo) -> Result<Box<dyn Wire>, String>;
}

/// The real ports, through ALSA.
pub struct Alsa;

impl Ports for Alsa {
    fn outputs(&self) -> Vec<PortInfo> {
        // No sequencer (a machine with no ALSA MIDI): no port.
        let Ok(output) = midir::MidiOutput::new(CLIENT) else {
            return Vec::new();
        };
        output
            .ports()
            .iter()
            .filter_map(|port| {
                Some(PortInfo {
                    id: port.id(),
                    name: output.port_name(port).ok()?,
                })
            })
            .collect()
    }

    fn inputs(&self) -> Vec<PortInfo> {
        let Ok(input) = midir::MidiInput::new(CLIENT) else {
            return Vec::new();
        };
        input
            .ports()
            .iter()
            .filter_map(|port| {
                Some(PortInfo {
                    id: port.id(),
                    name: input.port_name(port).ok()?,
                })
            })
            .collect()
    }

    fn connect_output(&self, port: &PortInfo) -> Result<Box<dyn Wire>, String> {
        let output =
            midir::MidiOutput::new(CLIENT).map_err(|e| format!("cannot start MIDI out: {e}"))?;
        let found = output
            .find_port_by_id(&port.id)
            .ok_or_else(|| format!("the MIDI output “{}” is not there any more", port.name))?;
        let connection = output
            .connect(&found, "Gamme out")
            .map_err(|e| format!("cannot connect to “{}”: {e}", port.name))?;
        Ok(Box::new(connection))
    }
}

impl Wire for midir::MidiOutputConnection {
    fn send(&mut self, message: &[u8]) -> Result<(), String> {
        midir::MidiOutputConnection::send(self, message).map_err(|e| e.to_string())
    }
}

/// An open port and the keys that sound on it. Whoever sends (the clock's thread, the player's
/// own keys) goes through it, so that nothing is left sounding.
struct Link {
    state: Mutex<State>,
    lost: Arc<AtomicBool>,
}

struct State {
    wire: Box<dyn Wire>,
    sounding: [bool; 128],
}

impl Link {
    fn new(wire: Box<dyn Wire>, lost: Arc<AtomicBool>) -> Self {
        Self {
            state: Mutex::new(State {
                wire,
                sounding: [false; 128],
            }),
            lost,
        }
    }

    fn note_on(&self, key: u8, velocity: u8) {
        self.send(
            &[0x90 | CHANNEL, key & 127, velocity.clamp(1, 127)],
            Some((key, true)),
        );
    }

    fn note_off(&self, key: u8) {
        self.send(&[0x80 | CHANNEL, key & 127, 0], Some((key, false)));
    }

    fn send(&self, message: &[u8], keys: Option<(u8, bool)>) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        self.send_locked(&mut state, message, keys);
    }

    fn send_locked(&self, state: &mut State, message: &[u8], keys: Option<(u8, bool)>) {
        // A port that is gone is not written to again.
        if self.lost.load(Relaxed) {
            return;
        }
        if let Some((key, on)) = keys {
            state.sounding[usize::from(key & 127)] = on;
        }
        if let Err(error) = state.wire.send(message) {
            eprintln!("gamme: MIDI out: {error}");
            self.lost.store(true, Relaxed);
        }
    }

    /// A note-off for each key that sounds, then All Notes Off for what a device kept of its own.
    fn silence(&self) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        for key in 0..128u8 {
            if state.sounding[usize::from(key)] {
                self.send_locked(&mut state, &[0x80 | CHANNEL, key, 0], Some((key, false)));
            }
        }
        self.send_locked(&mut state, &[0xB0 | CHANNEL, 123, 0], None);
    }
}

/// The renderer's sink: it renders no sound, it sends the notes.
struct MidiSink(Arc<Link>);

impl Sink for MidiSink {
    fn note_on(&mut self, key: u8, velocity: u8) {
        self.0.note_on(key, velocity);
    }

    fn note_off(&mut self, key: u8) {
        self.0.note_off(key);
    }

    fn release_all(&mut self) {
        self.0.silence();
    }

    fn render(&mut self, _: &mut [f32], _: &mut [f32]) {}
}

/// The player's own keys, sent to the open MIDI output, so that the piano with its speakers
/// answers the keyboard too. It is valid for every output the [`MidiOut`] opens, one after the
/// other; with none open, nothing is sent.
#[derive(Clone, Default)]
pub struct Live(Arc<Mutex<Option<Arc<Link>>>>);

impl Live {
    fn set(&self, link: Option<Arc<Link>>) {
        *self.0.lock().expect("the live keys' lock") = link;
    }

    fn link(&self) -> Option<Arc<Link>> {
        self.0.lock().expect("the live keys' lock").clone()
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "MIDI in (#14) sends the player's keys through it")
    )]
    pub fn note_on(&self, key: u8, velocity: u8) {
        if let Some(link) = self.link() {
            link.note_on(key, velocity);
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "MIDI in (#14) sends the player's keys through it")
    )]
    pub fn note_off(&self, key: u8) {
        if let Some(link) = self.link() {
            link.note_off(key);
        }
    }
}

/// How many frames of the virtual 1 kHz rate a tick is owed, without losing the time under one:
/// a tick that comes late gets what it missed, one that comes early gets none.
#[derive(Default)]
struct Pacer {
    owed_us: u64,
}

impl Pacer {
    fn frames(&mut self, elapsed: Duration, playing: bool) -> usize {
        if !playing {
            // The next play starts from now.
            self.owed_us = 0;
            return 0;
        }
        self.owed_us += elapsed.as_micros().min(u128::from(u64::MAX / 2)) as u64;
        let frame = FRAME.as_micros() as u64;
        let frames = (self.owed_us / frame) as usize;
        // What is past a stall is dropped, the time under a frame kept.
        self.owed_us %= frame;
        frames.min(MAX_CATCH_UP)
    }
}

/// The open output: the thread that paces the clock and sends the notes. Dropping it stops the
/// thread, which lets every key go and closes the port.
pub struct Output {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    live: Live,
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.live.set(None);
    }
}

/// What the thread looks for in the list of ports: its own.
struct Watch {
    ports: Arc<dyn Ports>,
    port: PortInfo,
}

impl Watch {
    fn present(&self) -> bool {
        self.ports.outputs().contains(&self.port)
    }
}

fn drive(
    mut renderer: Renderer<MidiSink>,
    link: Arc<Link>,
    clock: Clock,
    watch: Watch,
    stopped: mpsc::Receiver<()>,
) {
    // The sink renders nothing: these are only what the renderer is handed.
    let mut left = [0f32; MAX_CATCH_UP];
    let mut right = [0f32; MAX_CATCH_UP];
    let mut pacer = Pacer::default();
    let mut last = Instant::now();
    let mut watched = last;
    loop {
        let tick = if clock.playing() {
            PLAYING_TICK
        } else {
            IDLE_TICK
        };
        match stopped.recv_timeout(tick) {
            Err(RecvTimeoutError::Timeout) => {}
            // Sent to, or dropped: either way it is over.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        let playing = clock.playing();
        let frames = pacer.frames(now - last, playing);
        last = now;
        if playing {
            if frames > 0 {
                renderer.render(&mut left[..frames], &mut right[..frames]);
            }
        } else {
            // Not playing renders nothing, and tells the renderer to let go of what sounds.
            renderer.render(&mut left[..1], &mut right[..1]);
        }
        if now.duration_since(watched) >= WATCH_EVERY {
            watched = now;
            if !watch.present() {
                eprintln!("gamme: MIDI out: “{}” is gone", watch.port.name);
                link.lost.store(true, Relaxed);
            }
        }
    }
    link.silence();
}

/// MIDI out as the player's sound: the piece on the port the player chose.
pub struct MidiOut {
    ports: Arc<dyn Ports>,
    /// As the settings keep it.
    port: String,
    /// What plays when the port is not there, or goes.
    fallback: Arc<dyn Audio>,
    live: Live,
}

impl MidiOut {
    pub fn new(ports: Arc<dyn Ports>, port: String, fallback: Arc<dyn Audio>) -> Self {
        Self {
            ports,
            port,
            fallback,
            live: Live::default(),
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "MIDI in (#14) sends the player's keys through it")
    )]
    pub fn live(&self) -> Live {
        self.live.clone()
    }
}

impl Audio for MidiOut {
    fn open(
        &self,
        _soundfont: &Path,
        timeline: &Timeline,
        clock: &Clock,
        hands: &Hands,
        lost: &Arc<AtomicBool>,
    ) -> Result<Box<dyn Any>, String> {
        let all = self.ports.outputs();
        let port = find(&all, &self.port)
            .ok_or_else(|| "it is not connected".to_owned())?
            .clone();
        let wire = self.ports.connect_output(&port)?;
        let link = Arc::new(Link::new(wire, lost.clone()));
        let renderer = Renderer::new(
            MidiSink(link.clone()),
            piano::events(timeline),
            clock.clone(),
            hands.clone(),
            FRAME_RATE,
        );
        let watch = Watch {
            ports: self.ports.clone(),
            port,
        };
        let (stop, stopped) = mpsc::channel();
        let (clock, thread_link) = (clock.clone(), link.clone());
        let thread = std::thread::Builder::new()
            .name("gamme-midi".into())
            .spawn(move || drive(renderer, thread_link, clock, watch, stopped))
            .map_err(|e| format!("cannot start the MIDI thread: {e}"))?;
        self.live.set(Some(link));
        Ok(Box::new(Output {
            stop: Some(stop),
            thread: Some(thread),
            live: self.live.clone(),
        }))
    }

    fn needs_soundfont(&self) -> bool {
        false
    }

    fn label(&self) -> Option<String> {
        Some(format!("MIDI output “{}”", key(&self.port)))
    }

    fn fallback(&self) -> Option<Arc<dyn Audio>> {
        Some(self.fallback.clone())
    }
}

/// Ports that are not real, for the tests of this module and of the ones that use it.
#[cfg(test)]
pub mod testing {
    use super::*;

    /// A machine with these ports (names as ALSA says them), whose wires record what is sent.
    pub struct FakePorts {
        outputs: Mutex<Vec<String>>,
        inputs: Vec<String>,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        /// Whether the wires refuse what they are sent, as a port that went does.
        broken: Arc<AtomicBool>,
        connections: Mutex<u32>,
    }

    impl FakePorts {
        pub fn new(outputs: &[&str], inputs: &[&str]) -> Self {
            Self {
                outputs: Mutex::new(outputs.iter().map(|name| (*name).to_owned()).collect()),
                inputs: inputs.iter().map(|name| (*name).to_owned()).collect(),
                sent: Arc::default(),
                broken: Arc::default(),
                connections: Mutex::new(0),
            }
        }

        /// What was sent to the wires so far.
        pub fn sent(&self) -> Vec<Vec<u8>> {
            self.sent.lock().unwrap().clone()
        }

        pub fn connections(&self) -> u32 {
            *self.connections.lock().unwrap()
        }

        /// The port is unplugged: it is not listed any more.
        pub fn unplug(&self) {
            self.outputs.lock().unwrap().clear();
        }

        /// The wires fail from now on.
        pub fn break_wires(&self) {
            self.broken.store(true, Relaxed);
        }
    }

    struct FakeWire {
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        broken: Arc<AtomicBool>,
    }

    impl Wire for FakeWire {
        fn send(&mut self, message: &[u8]) -> Result<(), String> {
            if self.broken.load(Relaxed) {
                return Err("the port is gone".into());
            }
            self.sent.lock().unwrap().push(message.to_vec());
            Ok(())
        }
    }

    fn infos(names: &[String]) -> Vec<PortInfo> {
        names
            .iter()
            .enumerate()
            .map(|(at, name)| PortInfo {
                id: format!("{}:0", 20 + at),
                name: name.clone(),
            })
            .collect()
    }

    impl Ports for FakePorts {
        fn outputs(&self) -> Vec<PortInfo> {
            infos(&self.outputs.lock().unwrap())
        }

        fn inputs(&self) -> Vec<PortInfo> {
            infos(&self.inputs)
        }

        fn connect_output(&self, _: &PortInfo) -> Result<Box<dyn Wire>, String> {
            *self.connections.lock().unwrap() += 1;
            Ok(Box::new(FakeWire {
                sent: self.sent.clone(),
                broken: self.broken.clone(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakePorts;
    use super::*;
    use crate::clock::Speed;
    use crate::player::Builtin;
    use crate::timeline::{Hand, Note};

    const NOTE_ON: u8 = 0x90;
    const NOTE_OFF: u8 = 0x80;
    const ALL_NOTES_OFF: [u8; 3] = [0xB0, 123, 0];

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "waited for {what}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// One note, middle C, from the start to 5 s; a piece of 20 s.
    fn timeline() -> Timeline {
        let note = |key, on_ms, off_ms, hand| Note {
            key,
            velocity: 90,
            on_ms,
            off_ms,
            hand,
            part: 0,
            measure: 0,
            element: None,
        };
        Timeline::new(
            vec![note(60, 0., 5_000., Hand::Right)],
            vec![0., 10_000.],
            20_000.,
        )
    }

    struct Opened {
        ports: Arc<FakePorts>,
        clock: Clock,
        lost: Arc<AtomicBool>,
        output: Box<dyn Any>,
        midi: MidiOut,
    }

    fn open_with(ports: Arc<FakePorts>, saved: &str, hands: Hands) -> Result<Opened, String> {
        let midi = MidiOut::new(ports.clone(), saved.to_owned(), Arc::new(Builtin));
        let timeline = timeline();
        let clock = Clock::new(timeline.duration_ms());
        let lost = Arc::new(AtomicBool::new(false));
        let output = midi.open(Path::new(""), &timeline, &clock, &hands, &lost)?;
        Ok(Opened {
            ports,
            clock,
            lost,
            output,
            midi,
        })
    }

    fn opened() -> Opened {
        let ports = Arc::new(FakePorts::new(&["Digital Piano MIDI 1 20:0"], &[]));
        open_with(ports, "Digital Piano MIDI 1", Hands::both())
            .ok()
            .unwrap()
    }

    impl Opened {
        fn sent(&self) -> Vec<Vec<u8>> {
            self.ports.sent()
        }

        fn count(&self, status: u8, key: u8) -> usize {
            self.sent()
                .iter()
                .filter(|m| m.len() == 3 && m[0] == status && m[1] == key)
                .count()
        }

        fn silenced(&self) -> bool {
            self.sent().iter().any(|m| m == &ALL_NOTES_OFF)
        }
    }

    #[test]
    fn a_port_is_kept_by_its_name_without_the_numbers_alsa_changes() {
        assert_eq!(key("Digital Piano MIDI 1 20:0"), "Digital Piano MIDI 1");
        assert_eq!(key("Digital Piano MIDI 1 128:12"), "Digital Piano MIDI 1");
        assert_eq!(key("Midi Through 14:0"), "Midi Through");
        assert_eq!(key("Keystation 49"), "Keystation 49");
        assert_eq!(key("Port 1:x"), "Port 1:x");
        let ports = [
            PortInfo {
                id: "14:0".into(),
                name: "Midi Through 14:0".into(),
            },
            PortInfo {
                id: "24:0".into(),
                name: "Digital Piano MIDI 1 24:0".into(),
            },
        ];
        // The same piano at another number is found; another one is not.
        assert_eq!(find(&ports, "Digital Piano MIDI 1").unwrap().id, "24:0");
        assert_eq!(
            find(&ports, "Digital Piano MIDI 1 20:0").unwrap().id,
            "24:0"
        );
        assert!(find(&ports, "Other piano").is_none());
    }

    #[test]
    fn the_pacer_owes_the_time_it_was_late_and_drops_a_stall() {
        let mut pacer = Pacer::default();
        let ms = Duration::from_millis;
        assert_eq!(pacer.frames(ms(1), true), 1);
        assert_eq!(pacer.frames(Duration::from_micros(600), true), 0);
        // The 600 µs are not lost: 600 + 500 is a frame, and 100 µs are left.
        assert_eq!(pacer.frames(Duration::from_micros(500), true), 1);
        assert_eq!(pacer.frames(Duration::from_micros(900), true), 1);
        // A stall plays what it can catch up on, no more.
        assert_eq!(pacer.frames(Duration::from_secs(30), true), MAX_CATCH_UP);
        assert_eq!(pacer.frames(ms(1), true), 1);
        // Not playing owes nothing, and the next play starts from now.
        assert_eq!(pacer.frames(ms(500), false), 0);
        assert_eq!(pacer.frames(ms(1), true), 1);
    }

    #[test]
    fn the_piece_is_sent_to_the_port_as_the_clock_reaches_it() {
        let opened = opened();
        // Nothing before play.
        std::thread::sleep(Duration::from_millis(40));
        assert!(opened.sent().is_empty());
        opened.clock.play();
        until("the note on", || opened.count(NOTE_ON, 60) == 1);
        assert!(
            opened.sent().iter().any(|m| m == &[NOTE_ON, 60, 90]),
            "the key and its velocity: {:?}",
            opened.sent()
        );
        // The thread advances the clock by the wall clock.
        until("the clock to move", || {
            opened.clock.snapshot().position_us > 20_000
        });
        assert!(!opened.lost.load(Relaxed));
    }

    #[test]
    fn the_hands_the_computer_does_not_play_are_not_sent() {
        let hands = Hands::both();
        hands.set(false, true);
        let ports = Arc::new(FakePorts::new(&["Digital Piano MIDI 1 20:0"], &[]));
        let opened = open_with(ports, "Digital Piano MIDI 1", hands)
            .ok()
            .unwrap();
        opened.clock.play();
        until("the clock to move", || {
            opened.clock.snapshot().position_us > 50_000
        });
        assert_eq!(opened.count(NOTE_ON, 60), 0);
    }

    #[test]
    fn a_pause_lets_every_note_go() {
        let opened = opened();
        opened.clock.play();
        until("the note on", || opened.count(NOTE_ON, 60) == 1);
        assert!(!opened.silenced());
        opened.clock.pause();
        until("All Notes Off", || opened.silenced());
        assert_eq!(opened.count(NOTE_OFF, 60), 1, "the key itself is let go");
        // Nothing more is sent while it is paused.
        let sent = opened.sent().len();
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(opened.sent().len(), sent);
    }

    #[test]
    fn a_seek_lets_every_note_go_and_plays_on_from_there() {
        let opened = opened();
        opened.clock.play();
        until("the note on", || opened.count(NOTE_ON, 60) == 1);
        opened.clock.seek_ms(12_000.);
        until("All Notes Off", || opened.silenced());
        assert_eq!(opened.count(NOTE_OFF, 60), 1);
        // The note is behind the new position: it does not strike again.
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(opened.count(NOTE_ON, 60), 1);
    }

    #[test]
    fn the_end_of_the_piece_lets_every_note_go() {
        let timeline = Timeline::new(
            vec![Note {
                key: 62,
                velocity: 80,
                on_ms: 0.,
                off_ms: 100.,
                hand: Hand::Right,
                part: 0,
                measure: 0,
                element: None,
            }],
            vec![0.],
            100.,
        );
        let ports = Arc::new(FakePorts::new(&["Digital Piano MIDI 1 20:0"], &[]));
        let midi = MidiOut::new(
            ports.clone(),
            "Digital Piano MIDI 1".into(),
            Arc::new(Builtin),
        );
        let clock = Clock::new(timeline.duration_ms());
        let lost = Arc::new(AtomicBool::new(false));
        let _output = midi
            .open(Path::new(""), &timeline, &clock, &Hands::both(), &lost)
            .unwrap();
        clock.set_speed(Speed::FULL);
        clock.play();
        until("the end", || !clock.playing());
        until("All Notes Off", || {
            ports.sent().iter().any(|m| m == &ALL_NOTES_OFF)
        });
    }

    #[test]
    fn closing_the_output_lets_every_note_go_and_the_live_keys_too() {
        let opened = opened();
        let live = opened.midi.live();
        opened.clock.play();
        until("the note on", || opened.count(NOTE_ON, 60) == 1);
        live.note_on(72, 100);
        assert_eq!(opened.count(NOTE_ON, 72), 1);
        live.note_off(72);
        assert_eq!(opened.count(NOTE_OFF, 72), 1);
        live.note_on(73, 100);

        let Opened { ports, output, .. } = opened;
        drop(output);
        let sent = ports.sent();
        assert!(sent.iter().any(|m| m == &[NOTE_OFF, 60, 0]), "{sent:?}");
        assert!(sent.iter().any(|m| m == &[NOTE_OFF, 73, 0]), "{sent:?}");
        assert_eq!(sent.last().unwrap(), &ALL_NOTES_OFF);
        // The keys are sent nowhere once it is closed.
        let count = sent.len();
        live.note_on(74, 100);
        assert_eq!(ports.sent().len(), count);
    }

    #[test]
    fn a_wire_that_fails_is_a_lost_output_and_nothing_more_is_sent() {
        let opened = opened();
        opened.ports.break_wires();
        opened.clock.play();
        until("the output to be lost", || opened.lost.load(Relaxed));
        assert!(opened.sent().is_empty());
    }

    #[test]
    fn a_port_unplugged_is_a_lost_output_even_when_a_send_would_not_fail() {
        let opened = opened();
        opened.ports.unplug();
        until("the output to be lost", || opened.lost.load(Relaxed));
    }

    #[test]
    fn a_port_that_is_not_there_cannot_be_opened_and_a_built_in_piano_stands_by() {
        let ports = Arc::new(FakePorts::new(&["Other piano 20:0"], &[]));
        let error = open_with(ports.clone(), "Digital Piano MIDI 1", Hands::both())
            .err()
            .unwrap();
        assert_eq!(error, "it is not connected");
        assert_eq!(ports.connections(), 0);
        let midi = MidiOut::new(ports, "Digital Piano MIDI 1".into(), Arc::new(Builtin));
        assert!(midi.fallback().is_some());
        assert!(!midi.needs_soundfont());
        assert_eq!(midi.label().unwrap(), "MIDI output “Digital Piano MIDI 1”");
    }
}
