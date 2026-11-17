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
//! releases every key): a note-off for each key of the piece that sounds, then All Notes Off; and
//! when the output closes (a different port is chosen, the player goes): every key, the sustain
//! pedal, All Sound Off and All Notes Off. The player's own keys ([`Live`]) are kept apart from
//! the piece's: a pause or a seek does not cut a key the player holds.
//!
//! A port that disappears is told by a failed send, or by the port missing from the list (an
//! unplugged USB piano does not always fail a send), which a thread of its own looks at: `lost` is
//! set, nothing is sent any more, and the player goes on with the built-in piano and says so.
//! Each output has its own `lost` flag (`player.rs`), so that one that is closing cannot say the
//! output that took its place is lost.
//!
//! The player's keys (MIDI in, `midi_in.rs`) go out through [`Audio::live`], except those of the
//! very piano the piece goes to: it sounds its own keys already. They are still counted as held,
//! so that the piece does not cut them.
//!
//! A known limit: the player's sustain pedal goes out on the piece's channel (the first: which
//! channels a piano answers on is not known), so while he holds it the piece's notes ring on too,
//! through a pause or a seek, as they would on the piano itself.
//!
//! The MIDI library (`midir`, on ALSA) is behind [`Ports`] and [`Wire`], so that the tests open
//! no real port. [`Ports`] connects the input ports too, for MIDI in.

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
pub const CLIENT: &str = "Gamme";
/// The MIDI channel the piece is on (the first).
const CHANNEL: u8 = 0;
/// The renderer runs at this rate: one frame is one millisecond.
const FRAME_RATE: u32 = 1_000;
const FRAME: Duration = Duration::from_millis(1);
/// How often the thread looks at the clock while it plays, and while it does not (a pause is
/// let go of, and a play heard, within this).
const PLAYING_TICK: Duration = Duration::from_millis(1);
const IDLE_TICK: Duration = Duration::from_millis(50);
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

/// The client of a port, from its id ("20:0"): the device it belongs to.
fn client(id: &str) -> &str {
    id.split_once(':').map_or(id, |(client, _)| client)
}

/// An open connection to a MIDI output.
pub trait Wire: Send {
    fn send(&mut self, message: &[u8]) -> Result<(), String>;
}

/// What an input port sends, as it comes, on the MIDI library's own thread.
pub type OnMessage = Box<dyn FnMut(&[u8]) + Send>;

/// The MIDI ports of the machine.
pub trait Ports: Send + Sync {
    fn outputs(&self) -> Vec<PortInfo>;
    /// The input ports; none when the system's MIDI cannot be asked: not knowing is never taken
    /// for there being none.
    fn input_list(&self) -> Option<Vec<PortInfo>>;
    fn inputs(&self) -> Vec<PortInfo> {
        self.input_list().unwrap_or_default()
    }
    fn connect_output(&self, port: &PortInfo) -> Result<Box<dyn Wire>, String>;
    /// Listens to an input port: `on_message` is given each message until the connection that
    /// comes back is dropped, which waits for the library's thread to be done with it.
    fn connect_input(
        &self,
        port: &PortInfo,
        on_message: OnMessage,
    ) -> Result<Box<dyn Send>, String>;
    /// Whether `port` is still listed; none when that cannot be told (the system's MIDI cannot be
    /// asked): not knowing is never taken for the port being gone.
    fn present(&self, port: &PortInfo) -> Option<bool>;
}

/// The real ports, through ALSA. Listing is done through one client of each kind, kept: opening
/// one is a sequencer client other MIDI software sees come and go. A connection to an input
/// takes a client of its own (`midir` wants it so), given back when it closes and kept for the
/// next one.
#[derive(Default)]
pub struct Alsa {
    output: Mutex<Option<midir::MidiOutput>>,
    input: Mutex<Option<midir::MidiInput>>,
    spare_inputs: Arc<Mutex<Vec<midir::MidiInput>>>,
}

/// A connection to an input port; dropped, it gives its client back.
struct AlsaInput {
    connection: Option<midir::MidiInputConnection<()>>,
    spare: Arc<Mutex<Vec<midir::MidiInput>>>,
}

impl Drop for AlsaInput {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let (client, ()) = connection.close();
            if let Ok(mut spare) = self.spare.lock() {
                spare.push(client);
            }
        }
    }
}

impl Alsa {
    /// Runs `list` on the kept output client, made the first time; none when there is no
    /// sequencer (a machine with no ALSA MIDI).
    fn on_output<T>(&self, list: impl FnOnce(&midir::MidiOutput) -> T) -> Option<T> {
        let mut client = self.output.lock().ok()?;
        if client.is_none() {
            *client = midir::MidiOutput::new(CLIENT).ok();
        }
        client.as_ref().map(list)
    }
}

fn output_ports(client: &midir::MidiOutput) -> Vec<PortInfo> {
    client
        .ports()
        .iter()
        .filter_map(|port| {
            Some(PortInfo {
                id: port.id(),
                name: client.port_name(port).ok()?,
            })
        })
        .collect()
}

impl Ports for Alsa {
    fn outputs(&self) -> Vec<PortInfo> {
        self.on_output(output_ports).unwrap_or_default()
    }

    fn input_list(&self) -> Option<Vec<PortInfo>> {
        let mut client = self.input.lock().ok()?;
        if client.is_none() {
            *client = midir::MidiInput::new(CLIENT).ok();
        }
        let client = client.as_ref()?;
        // A port whose name cannot be read went between the two calls: it alone is left out
        // (the `?` is the closure's), the list is still read.
        Some(
            client
                .ports()
                .iter()
                .filter_map(|port| {
                    Some(PortInfo {
                        id: port.id(),
                        name: client.port_name(port).ok()?,
                    })
                })
                .collect(),
        )
    }

    fn connect_input(
        &self,
        port: &PortInfo,
        mut on_message: OnMessage,
    ) -> Result<Box<dyn Send>, String> {
        let spare = self
            .spare_inputs
            .lock()
            .ok()
            .and_then(|mut spare| spare.pop());
        let mut input = match spare {
            Some(input) => input,
            None => {
                midir::MidiInput::new(CLIENT).map_err(|e| format!("cannot start MIDI in: {e}"))?
            }
        };
        // Only what a key does: no clock, no active sensing, no SysEx to wake for.
        input.ignore(midir::Ignore::All);
        let Some(found) = input.find_port_by_id(&port.id) else {
            if let Ok(mut spare) = self.spare_inputs.lock() {
                spare.push(input);
            }
            return Err(format!(
                "the MIDI input “{}” is not there any more",
                port.name
            ));
        };
        match input.connect(
            &found,
            "Gamme in",
            move |_, message, _| on_message(message),
            (),
        ) {
            Ok(connection) => Ok(Box::new(AlsaInput {
                connection: Some(connection),
                spare: self.spare_inputs.clone(),
            })),
            Err(error) => {
                let why = format!("cannot connect to “{}”: {error}", port.name);
                if let Ok(mut spare) = self.spare_inputs.lock() {
                    spare.push(error.into_inner());
                }
                Err(why)
            }
        }
    }

    fn present(&self, port: &PortInfo) -> Option<bool> {
        self.on_output(|client| output_ports(client).contains(port))
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
    /// The device the port belongs to (`client`).
    device: String,
}

struct State {
    wire: Box<dyn Wire>,
    /// The keys the piece sounds, and the keys the player holds (see [`Live`]).
    piece: [bool; 128],
    live: [bool; 128],
    /// The piece's keys whose note-off was held back for a key the player holds: it goes with
    /// his, even when his note-on was not sent (his piano's own key).
    owed: [bool; 128],
    /// Nothing is sent once the link is closed: a key that comes late stays silent.
    closed: bool,
}

impl Link {
    fn new(wire: Box<dyn Wire>, lost: Arc<AtomicBool>, port: &PortInfo) -> Self {
        Self {
            state: Mutex::new(State {
                wire,
                piece: [false; 128],
                live: [false; 128],
                owed: [false; 128],
                closed: false,
            }),
            lost,
            device: client(&port.id).to_owned(),
        }
    }

    /// A port that is gone is not written to again.
    fn is_lost(&self) -> bool {
        self.lost.load(Relaxed)
    }

    fn note_on(&self, key: u8, velocity: u8) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        state.piece[usize::from(key & 127)] = true;
        self.send(
            &mut state,
            &[0x90 | CHANNEL, key & 127, velocity.clamp(1, 127)],
        );
    }

    fn note_off(&self, key: u8) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        let at = usize::from(key & 127);
        state.piece[at] = false;
        // The player holds that key too: it is his to let go.
        if state.live[at] {
            state.owed[at] = true;
        } else {
            self.send(&mut state, &[0x80 | CHANNEL, key & 127, 0]);
        }
    }

    /// A key the player strikes; `echo`: sent, which a piano's own key is not (it sounds it).
    fn live_on(&self, key: u8, velocity: u8, echo: bool) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        state.live[usize::from(key & 127)] = true;
        if echo {
            self.send(
                &mut state,
                &[0x90 | CHANNEL, key & 127, velocity.clamp(1, 127)],
            );
        }
    }

    fn live_off(&self, key: u8, echo: bool) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        let at = usize::from(key & 127);
        state.live[at] = false;
        let owed = std::mem::take(&mut state.owed[at]);
        if !state.piece[at] && (echo || owed) {
            self.send(&mut state, &[0x80 | CHANNEL, key & 127, 0]);
        }
    }

    /// The player's sustain pedal; the close lets it up.
    fn live_pedal(&self, value: u8) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        self.send(&mut state, &[0xB0 | CHANNEL, 64, value & 127]);
    }

    fn send(&self, state: &mut State, message: &[u8]) {
        if state.closed || self.is_lost() {
            return;
        }
        if let Err(error) = state.wire.send(message) {
            eprintln!("gamme: MIDI out: {error}");
            self.lost.store(true, Relaxed);
        }
    }

    /// The piece's keys let go (a pause, a seek, its end): a note-off for each, then All Notes Off
    /// for what a device kept of its own, unless the player holds a key, which it would cut.
    fn release_piece(&self) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        for key in 0..128u8 {
            let at = usize::from(key);
            if !std::mem::take(&mut state.piece[at]) {
                continue;
            }
            if state.live[at] {
                state.owed[at] = true;
            } else {
                self.send(&mut state, &[0x80 | CHANNEL, key, 0]);
            }
        }
        if !state.live.iter().any(|held| *held) {
            self.send(&mut state, &[0xB0 | CHANNEL, 123, 0]);
        }
    }

    /// The output closes: every key, the sustain pedal, All Sound Off and All Notes Off (a note
    /// held by the pedal is not ended by All Notes Off), and nothing after.
    fn close(&self) {
        let mut state = self.state.lock().expect("the MIDI port's lock");
        for key in 0..128u8 {
            let at = usize::from(key);
            state.owed[at] = false;
            if std::mem::take(&mut state.piece[at]) | std::mem::take(&mut state.live[at]) {
                self.send(&mut state, &[0x80 | CHANNEL, key, 0]);
            }
        }
        for (controller, value) in [(64, 0), (120, 0), (123, 0)] {
            self.send(&mut state, &[0xB0 | CHANNEL, controller, value]);
        }
        state.closed = true;
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
        self.0.release_piece();
    }

    // The player's keys go out through `Live`, at once, not at the pace of this thread.
    fn live_on(&mut self, _: u8, _: u8) {}
    fn live_off(&mut self, _: u8) {}
    fn live_pedal(&mut self, _: u8) {}

    fn render(&mut self, _: &mut [f32], _: &mut [f32]) {}
}

/// The player's own keys, sent to the open MIDI output, so that the piano with its speakers
/// answers the keyboard too. It is valid for every output the [`MidiOut`] opens, one after the
/// other; with none open, nothing is sent.
#[derive(Clone, Default)]
pub struct Live(Arc<Mutex<LiveState>>);

#[derive(Default)]
struct LiveState {
    link: Option<Arc<Link>>,
    /// The player's pedal as he left it last, and the device it came from: an output that
    /// opens later starts with it.
    pedal: u8,
    pedal_from: String,
}

impl Live {
    fn set(&self, link: Option<Arc<Link>>) {
        let mut state = self.0.lock().expect("the live keys' lock");
        if let Some(link) = &link
            && state.pedal > 0
            && link.device != state.pedal_from
        {
            link.live_pedal(state.pedal);
        }
        state.link = link;
    }

    fn link(&self) -> Option<Arc<Link>> {
        self.0.lock().expect("the live keys' lock").link.clone()
    }

    #[cfg(test)]
    pub fn note_on(&self, key: u8, velocity: u8) {
        if let Some(link) = self.link() {
            link.live_on(key, velocity, true);
        }
    }

    #[cfg(test)]
    pub fn note_off(&self, key: u8) {
        if let Some(link) = self.link() {
            link.live_off(key, true);
        }
    }

    /// A key the player struck (`velocity` above 0) or let go on the input port `from`: sent,
    /// unless the port is of the output's own device, and counted as held either way.
    fn key(&self, key: u8, velocity: u8, from: &str) {
        let Some(link) = self.link() else {
            return;
        };
        let echo = link.device != client(from);
        if velocity > 0 {
            link.live_on(key, velocity, echo);
        } else {
            link.live_off(key, echo);
        }
    }

    /// The player's pedal moved, on the input port `from`: sent, unless the port is of the
    /// output's own device (its pedal is its own already).
    fn pedal(&self, value: u8, from: &str) {
        let link = {
            let mut state = self.0.lock().expect("the live keys' lock");
            state.pedal = value;
            client(from).clone_into(&mut state.pedal_from);
            state.link.clone()
        };
        if let Some(link) = link
            && link.device != client(from)
        {
            link.live_pedal(value);
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

/// The open output: the thread that paces the clock and sends the notes, and the one that looks
/// for the port in the list. Dropping it closes the port: the live keys are cut off first, then
/// the threads end, the pacing one letting every key go.
pub struct Output {
    stop: Option<Sender<()>>,
    watch_stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    watch: Option<JoinHandle<()>>,
    live: Live,
}

impl Drop for Output {
    fn drop(&mut self) {
        // First: a key of the player's that comes now goes nowhere, and cannot sound after the
        // silence and stay on.
        self.live.set(None);
        self.stop.take();
        self.watch_stop.take();
        for thread in [self.thread.take(), self.watch.take()]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
    }
}

/// Looks for the port in the list, on a thread of its own (the pacing one is not made to wait for
/// the system), until it is gone or the output closes.
fn watch(ports: &dyn Ports, port: &PortInfo, lost: &AtomicBool, stopped: &mpsc::Receiver<()>) {
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(WATCH_EVERY) {
        if lost.load(Relaxed) {
            return;
        }
        // Unknown is not gone.
        if ports.present(port) == Some(false) {
            eprintln!("gamme: MIDI out: “{}” is gone", port.name);
            lost.store(true, Relaxed);
            return;
        }
    }
}

fn drive(
    mut renderer: Renderer<MidiSink>,
    link: Arc<Link>,
    clock: Clock,
    stopped: mpsc::Receiver<()>,
) {
    // The sink renders nothing: these are only what the renderer is handed.
    let mut left = [0f32; MAX_CATCH_UP];
    let mut right = [0f32; MAX_CATCH_UP];
    let mut pacer = Pacer::default();
    let mut last = Instant::now();
    let mut was_playing = false;
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
        // A port that is gone renders no more, and claims the clock no more: the output that
        // takes over is the one that drives it.
        if link.is_lost() {
            break;
        }
        let now = Instant::now();
        let playing = clock.playing();
        // A play starts from now, not from the last look, which was a pause ago.
        let elapsed = if was_playing {
            now - last
        } else {
            Duration::ZERO
        };
        let frames = pacer.frames(elapsed, playing);
        last = now;
        was_playing = playing;
        if playing {
            if frames > 0 {
                renderer.render(&mut left[..frames], &mut right[..frames]);
            }
        } else {
            // Not playing renders nothing, and tells the renderer to let go of what sounds.
            renderer.render(&mut left[..1], &mut right[..1]);
        }
    }
    link.close();
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

    /// What `Audio::live` sends through, for the tests to hold apart from the output.
    #[cfg(test)]
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
        let link = Arc::new(Link::new(wire, lost.clone(), &port));
        let renderer = Renderer::new(
            MidiSink(link.clone()),
            piano::events(timeline),
            clock.clone(),
            hands.clone(),
            FRAME_RATE,
        );
        let (stop, stopped) = mpsc::channel();
        let (clock, thread_link) = (clock.clone(), link.clone());
        let thread = std::thread::Builder::new()
            .name("gamme-midi".into())
            .spawn(move || drive(renderer, thread_link, clock, stopped))
            .map_err(|e| format!("cannot start the MIDI thread: {e}"))?;
        let (watch_stop, watch_stopped) = mpsc::channel();
        let (ports, flag) = (self.ports.clone(), lost.clone());
        let watcher = std::thread::Builder::new()
            .name("gamme-midi-watch".into())
            .spawn(move || watch(&*ports, &port, &flag, &watch_stopped));
        let watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                // The output is closed again, its port with it.
                drop(stop);
                let _ = thread.join();
                return Err(format!("cannot start the MIDI thread: {error}"));
            }
        };
        self.live.set(Some(link));
        Ok(Box::new(Output {
            stop: Some(stop),
            watch_stop: Some(watch_stop),
            thread: Some(thread),
            watch: Some(watcher),
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

    fn live(&self, key: u8, velocity: u8, from: &str) {
        // A digital piano that is both the keyboard and the output sounds its keys itself: sent
        // back, each would sound twice. They are held all the same (`Live::key`).
        self.live.key(key, velocity, from);
    }

    fn live_pedal(&self, value: u8, from: &str) {
        self.live.pedal(value, from);
    }
}

/// Ports that are not real, for the tests of this module and of the ones that use it.
#[cfg(test)]
pub mod testing {
    use std::sync::atomic::AtomicU32;

    use super::*;

    /// An input connected to, as `connect_input` left it.
    struct Listening {
        port: PortInfo,
        on_message: OnMessage,
        /// Cleared when the connection is dropped.
        open: Arc<AtomicBool>,
    }

    /// What `connect_input` hands back: dropped, the input is not listened to any more.
    struct FakeConnection(Arc<AtomicBool>);

    impl Drop for FakeConnection {
        fn drop(&mut self) {
            self.0.store(false, Relaxed);
        }
    }

    /// A machine with these ports (names as ALSA says them), whose wires record what is sent.
    /// Its inputs are numbered from 40 ("40:0"), its outputs from 20.
    pub struct FakePorts {
        outputs: Mutex<Vec<String>>,
        inputs: Mutex<Vec<PortInfo>>,
        listening: Mutex<Vec<Listening>>,
        input_connections: Mutex<u32>,
        /// Whether connecting to an input fails.
        refuse_inputs: AtomicBool,
        looks: AtomicU32,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        /// Whether the wires refuse what they are sent, as a port that went does.
        broken: Arc<AtomicBool>,
        connections: Mutex<u32>,
        /// Whether the system cannot be asked which ports are there.
        unknown: AtomicBool,
    }

    impl FakePorts {
        pub fn new(outputs: &[&str], inputs: &[&str]) -> Self {
            let inputs = inputs
                .iter()
                .enumerate()
                .map(|(at, name)| PortInfo {
                    id: format!("{}:0", 40 + at),
                    name: (*name).to_owned(),
                })
                .collect();
            Self {
                outputs: Mutex::new(outputs.iter().map(|name| (*name).to_owned()).collect()),
                inputs: Mutex::new(inputs),
                listening: Mutex::new(Vec::new()),
                input_connections: Mutex::new(0),
                refuse_inputs: AtomicBool::new(false),
                looks: AtomicU32::new(0),
                sent: Arc::default(),
                broken: Arc::default(),
                connections: Mutex::new(0),
                unknown: AtomicBool::new(false),
            }
        }

        /// The list of ports cannot be read from now on, or again.
        pub fn cannot_look(&self) {
            self.unknown.store(true, Relaxed);
        }

        pub fn can_look(&self) {
            self.unknown.store(false, Relaxed);
        }

        /// An input port appears, with this id ("client:port").
        pub fn plug_input(&self, name: &str, id: &str) {
            self.inputs.lock().unwrap().push(PortInfo {
                id: id.to_owned(),
                name: name.to_owned(),
            });
        }

        /// The input port named so is unplugged: not listed any more, and silent.
        pub fn unplug_input(&self, name: &str) {
            self.inputs.lock().unwrap().retain(|port| port.name != name);
        }

        /// Connecting to an input fails from now on, or works again.
        pub fn refuse_inputs(&self, refuse: bool) {
            self.refuse_inputs.store(refuse, Relaxed);
        }

        /// How many times the inputs were listed.
        pub fn looks(&self) -> u32 {
            self.looks.load(Relaxed)
        }

        pub fn input_connections(&self) -> u32 {
            *self.input_connections.lock().unwrap()
        }

        /// The names of the inputs listened to now.
        pub fn listened(&self) -> Vec<String> {
            let mut listening = self.listening.lock().unwrap();
            listening.retain(|input| input.open.load(Relaxed));
            listening
                .iter()
                .map(|input| input.port.name.clone())
                .collect()
        }

        /// The input port named so sends `message`, if it is listened to; whether it was.
        pub fn send_from(&self, name: &str, message: &[u8]) -> bool {
            let mut listening = self.listening.lock().unwrap();
            listening.retain(|input| input.open.load(Relaxed));
            let Some(input) = listening.iter_mut().find(|input| input.port.name == name) else {
                return false;
            };
            (input.on_message)(message);
            true
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

        fn input_list(&self) -> Option<Vec<PortInfo>> {
            self.looks.fetch_add(1, Relaxed);
            if self.unknown.load(Relaxed) {
                return None;
            }
            Some(self.inputs.lock().unwrap().clone())
        }

        fn connect_input(
            &self,
            port: &PortInfo,
            on_message: OnMessage,
        ) -> Result<Box<dyn Send>, String> {
            *self.input_connections.lock().unwrap() += 1;
            if self.refuse_inputs.load(Relaxed) {
                return Err(format!("cannot connect to “{}”", port.name));
            }
            let open = Arc::new(AtomicBool::new(true));
            self.listening.lock().unwrap().push(Listening {
                port: port.clone(),
                on_message,
                open: open.clone(),
            });
            Ok(Box::new(FakeConnection(open)))
        }

        fn present(&self, port: &PortInfo) -> Option<bool> {
            if self.unknown.load(Relaxed) {
                return None;
            }
            Some(self.outputs().contains(port))
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
        let midi = MidiOut::new(
            ports.clone(),
            saved.to_owned(),
            Arc::new(Builtin::default()),
        );
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
            Arc::new(Builtin::default()),
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
        // What a note held by the pedal needs: the pedal up, and All Sound Off.
        assert!(sent.iter().any(|m| m == &[0xB0, 64, 0]), "{sent:?}");
        assert!(sent.iter().any(|m| m == &[0xB0, 120, 0]), "{sent:?}");
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
    fn a_key_the_player_holds_is_not_cut_by_a_pause_or_a_seek() {
        let opened = opened();
        let live = opened.midi.live();
        opened.clock.play();
        until("the note on", || opened.count(NOTE_ON, 60) == 1);
        live.note_on(72, 100);
        opened.clock.pause();
        until("the piece's key to go", || opened.count(NOTE_OFF, 60) == 1);
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(opened.count(NOTE_OFF, 72), 0, "{:?}", opened.sent());
        // All Notes Off would cut it too: it is not sent while a key is held.
        assert!(!opened.silenced(), "{:?}", opened.sent());
        opened.clock.seek_ms(12_000.);
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(opened.count(NOTE_OFF, 72), 0);
        // Letting it go is the player's.
        live.note_off(72);
        assert_eq!(opened.count(NOTE_OFF, 72), 1);
    }

    #[test]
    fn the_players_keys_go_out_but_not_back_to_the_piano_they_come_from() {
        let opened = opened();
        // A keyboard of its own (client 40): the piano answers it.
        Audio::live(&opened.midi, 72, 100, "40:0");
        Audio::live(&opened.midi, 72, 0, "40:0");
        assert_eq!(opened.count(NOTE_ON, 72), 1);
        assert_eq!(opened.count(NOTE_OFF, 72), 1);
        // The piano's own keys (its input is client 20 too, as the output): it sounds them.
        Audio::live(&opened.midi, 74, 100, "20:1");
        Audio::live(&opened.midi, 74, 0, "20:1");
        assert_eq!(opened.count(NOTE_ON, 74), 0);
        assert_eq!(opened.count(NOTE_OFF, 74), 0);
        // The pedal likewise: its own is not sent back, another keyboard's is.
        opened.midi.live_pedal(127, "20:1");
        assert_eq!(opened.count(0xB0, 64), 0);
        opened.midi.live_pedal(127, "40:0");
        opened.midi.live_pedal(0, "40:0");
        let pedal: Vec<_> = opened
            .sent()
            .into_iter()
            .filter(|m| m[..2] == [0xB0, 64])
            .collect();
        assert_eq!(pedal, [vec![0xB0, 64, 127], vec![0xB0, 64, 0]]);
    }

    #[test]
    fn the_pianos_own_key_held_is_not_cut_by_the_piece_nor_a_pause() {
        let opened = opened();
        opened.clock.play();
        until("the note on", || opened.count(NOTE_ON, 60) == 1);
        // The player holds the piece's key on the piano itself (client 20, as the output).
        Audio::live(&opened.midi, 60, 100, "20:1");
        assert_eq!(opened.count(NOTE_ON, 60), 1, "not sent back");
        opened.clock.pause();
        std::thread::sleep(Duration::from_millis(150));
        // Neither the piece's note-off nor All Notes Off: either would cut his key.
        assert_eq!(opened.count(NOTE_OFF, 60), 0, "{:?}", opened.sent());
        assert!(!opened.silenced(), "{:?}", opened.sent());
        // He lets go: the piece's note-off held back goes now.
        Audio::live(&opened.midi, 60, 0, "20:1");
        assert_eq!(opened.count(NOTE_OFF, 60), 1);
        // A key of his alone, let go: the piano ended it itself, nothing is sent.
        Audio::live(&opened.midi, 62, 100, "20:1");
        Audio::live(&opened.midi, 62, 0, "20:1");
        assert_eq!(opened.count(NOTE_OFF, 62), 0);
    }

    #[test]
    fn an_output_that_opens_with_the_pedal_down_starts_with_it() {
        let ports = Arc::new(FakePorts::new(&["Digital Piano MIDI 1 20:0"], &[]));
        let midi = MidiOut::new(
            ports.clone(),
            "Digital Piano MIDI 1".into(),
            Arc::new(Builtin::default()),
        );
        // Before any output: kept.
        midi.live_pedal(127, "40:0");
        assert!(ports.sent().is_empty());
        let timeline = timeline();
        let clock = Clock::new(timeline.duration_ms());
        let lost = Arc::new(AtomicBool::new(false));
        let output = midi
            .open(Path::new(""), &timeline, &clock, &Hands::both(), &lost)
            .unwrap();
        assert_eq!(ports.sent(), [vec![0xB0, 64, 127]]);
        drop(output);
        // The piano's own pedal is not sent to it, at the open either.
        midi.live_pedal(127, "20:1");
        let sent = ports.sent().len();
        let _output = midi
            .open(Path::new(""), &timeline, &clock, &Hands::both(), &lost)
            .unwrap();
        assert_eq!(ports.sent().len(), sent);
    }

    #[test]
    fn closing_the_output_lets_the_players_pedal_up() {
        let opened = opened();
        opened.midi.live_pedal(127, "40:0");
        let Opened { ports, output, .. } = opened;
        drop(output);
        let sent = ports.sent();
        let down = sent.iter().position(|m| m == &[0xB0, 64, 127]).unwrap();
        let up = sent.iter().rposition(|m| m == &[0xB0, 64, 0]).unwrap();
        assert!(down < up, "{sent:?}");
    }

    #[test]
    fn a_live_key_that_comes_late_after_the_close_sounds_nowhere() {
        let opened = opened();
        let live = opened.midi.live();
        let Opened { ports, output, .. } = opened;
        drop(output);
        let count = ports.sent().len();
        live.note_on(80, 100);
        assert_eq!(ports.sent().len(), count);
    }

    #[test]
    fn a_lost_output_renders_and_claims_the_clock_no_more() {
        let opened = opened();
        opened.ports.unplug();
        until("the output to be lost", || opened.lost.load(Relaxed));
        // The thread ends at its next look; what takes over is released the clock.
        std::thread::sleep(Duration::from_millis(150));
        opened.clock.release();
        std::thread::sleep(Duration::from_millis(150));
        assert!(!opened.clock.claimed());
    }

    #[test]
    fn a_port_list_that_cannot_be_read_is_not_a_port_that_is_gone() {
        let opened = opened();
        opened.ports.unplug();
        opened.ports.cannot_look();
        std::thread::sleep(WATCH_EVERY * 2 + Duration::from_millis(300));
        assert!(!opened.lost.load(Relaxed));
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
        let midi = MidiOut::new(
            ports,
            "Digital Piano MIDI 1".into(),
            Arc::new(Builtin::default()),
        );
        assert!(midi.fallback().is_some());
        assert!(!midi.needs_soundfont());
        assert_eq!(midi.label().unwrap(), "MIDI output “Digital Piano MIDI 1”");
    }
}
