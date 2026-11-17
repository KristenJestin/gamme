//! MIDI in: the player's keyboards. Every input port is listened to as it appears, and again
//! once it is plugged back after it was unplugged; or only the one the settings choose
//! (`Settings::midi_in`; none is the settings page's "Every port"). Each key struck or let go is
//! stamped with the instant it arrived ([`Played`]) and handed to what shows the keyboard
//! ([`Keys`], the play screen), which lights the key, sounds it on the player's output and keeps it
//! for the game.
//!
//! One thread watches the ports for as long as the app runs: it lists them every second through
//! the one listing client [`Ports`] keeps (a client made for each look is one that other MIDI
//! software sees come and go), connects what is new and lets go of what is gone. A list that
//! cannot be read changes nothing: not knowing is not a port gone. A port that cannot be
//! connected to is not tried again until it is unplugged or the choice changes: trying each
//! second would make a client each second.
//!
//! The messages come on the MIDI library's threads, one per port, never the window's: nothing
//! there calls GPUI. A key held on two keyboards is let go when the last one lets go of it, and
//! the keys held on a port that goes are let go then.
//!
//! Gamme's own ports (its MIDI out) are never listened to, and "Midi Through" only when it is
//! chosen: it echoes what other software sends it, and the piece itself when it is the output.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::midi_out::{self, OnMessage, PortInfo, Ports};

/// How often the ports are listed.
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// The client ALSA's loopback port belongs to.
const THROUGH: &str = "Midi Through";

/// A key struck (`velocity` above 0) or let go, and the instant it arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Played {
    pub key: u8,
    pub velocity: u8,
    pub at: Instant,
}

/// Where the keys go: what shows the keyboard.
pub trait Keys: Send + Sync {
    /// A key played on the input port `from`. Called on MIDI's threads, never the window's, one
    /// at a time: it must not wait for the window.
    fn played(&self, played: Played, from: &PortInfo);
}

/// The key of a message, and its velocity (0: let go); none for what is not a key.
fn note(message: &[u8]) -> Option<(u8, u8)> {
    match *message {
        [status, key, velocity, ..] if status & 0xF0 == 0x90 => Some((key & 127, velocity & 127)),
        [status, key, _, ..] if status & 0xF0 == 0x80 => Some((key & 127, 0)),
        _ => None,
    }
}

/// Whether the client of the port named so is `client` ("Gamme:Gamme out 130:0").
fn of_client(name: &str, client: &str) -> bool {
    name.strip_prefix(client)
        .is_some_and(|rest| rest.starts_with(':'))
}

/// The ports to listen to among `list`: the one chosen, or every one.
fn wanted<'a>(list: &'a [PortInfo], choice: Option<&str>) -> Vec<&'a PortInfo> {
    let own = |port: &PortInfo| of_client(&port.name, midi_out::CLIENT);
    match choice {
        Some(saved) => midi_out::find(list, saved)
            .filter(|port| !own(port))
            .into_iter()
            .collect(),
        None => list
            .iter()
            .filter(|port| !own(port) && !of_client(&port.name, THROUGH))
            .collect(),
    }
}

/// The keys held on each connection, and where they go.
struct Hub {
    keys: Option<(u64, Arc<dyn Keys>)>,
    /// Each open connection: its number, its port, the keys held on it.
    connections: Vec<(u64, PortInfo, [bool; 128])>,
    /// On how many connections each key is held.
    count: [u8; 128],
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            keys: None,
            connections: Vec::new(),
            count: [0; 128],
        }
    }
}

impl Hub {
    fn message(&mut self, serial: u64, message: &[u8], at: Instant) {
        let Some((key, velocity)) = note(message) else {
            return;
        };
        // A message that comes as its connection closes.
        let Some((_, from, held)) = self.connections.iter_mut().find(|c| c.0 == serial) else {
            return;
        };
        let at_key = usize::from(key);
        let count = &mut self.count[at_key];
        if velocity > 0 {
            if !std::mem::replace(&mut held[at_key], true) {
                *count = count.saturating_add(1);
            }
        } else {
            // Not struck here (struck before Gamme listened): nothing to let go.
            if !std::mem::take(&mut held[at_key]) {
                return;
            }
            *count = count.saturating_sub(1);
            // Another keyboard holds it.
            if *count > 0 {
                return;
            }
        }
        if let Some((_, keys)) = &self.keys {
            keys.played(Played { key, velocity, at }, from);
        }
    }

    /// The connection `serial` is closed: the keys held on it are let go.
    fn close(&mut self, serial: u64, at: Instant) {
        let Some(index) = self.connections.iter().position(|c| c.0 == serial) else {
            return;
        };
        let (_, from, held) = self.connections.remove(index);
        for key in 0..128u8 {
            let at_key = usize::from(key);
            if !held[at_key] {
                continue;
            }
            self.count[at_key] = self.count[at_key].saturating_sub(1);
            if self.count[at_key] == 0
                && let Some((_, keys)) = &self.keys
            {
                keys.played(
                    Played {
                        key,
                        velocity: 0,
                        at,
                    },
                    &from,
                );
            }
        }
    }
}

/// The hub's lock; a panic of a [`Keys`] does not stop the keys after it.
fn lock(hub: &Mutex<Hub>) -> MutexGuard<'_, Hub> {
    hub.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum Wake {
    Choose(Option<String>),
}

struct Inner {
    hub: Arc<Mutex<Hub>>,
    wake: Mutex<Option<Sender<Wake>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    attachments: AtomicU64,
    #[cfg(test)]
    listening: Arc<Mutex<Vec<PortInfo>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The thread ends with its channel, and closes its connections.
        if let Ok(mut wake) = self.wake.lock() {
            *wake = None;
        }
        if let Some(thread) = self.thread.lock().ok().and_then(|mut thread| thread.take()) {
            let _ = thread.join();
        }
    }
}

/// MIDI in, for the whole app: the thread that watches the ports, and where the keys go. Clones
/// share it.
#[derive(Clone)]
pub struct MidiIn(Arc<Inner>);

impl MidiIn {
    /// Starts watching `ports`; `choice` is the port chosen in the settings, read on the thread
    /// (it reads a file).
    pub fn start(
        ports: Arc<dyn Ports>,
        choice: impl FnOnce() -> Option<String> + Send + 'static,
    ) -> Self {
        Self::watching(ports, choice, WATCH_EVERY)
    }

    fn watching(
        ports: Arc<dyn Ports>,
        choice: impl FnOnce() -> Option<String> + Send + 'static,
        every: Duration,
    ) -> Self {
        let hub = Arc::new(Mutex::new(Hub::default()));
        let (wake, woken) = mpsc::channel();
        #[cfg(test)]
        let listening = Arc::new(Mutex::new(Vec::new()));
        let watcher = Watcher {
            ports,
            hub: hub.clone(),
            every,
            connections: Vec::new(),
            refused: Vec::new(),
            serials: 0,
            #[cfg(test)]
            listening: listening.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("gamme-midi-in".into())
            .spawn(move || watcher.run(choice(), &woken));
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(error) => {
                eprintln!("gamme: MIDI in cannot start: {error}");
                None
            }
        };
        Self(Arc::new(Inner {
            hub,
            wake: Mutex::new(Some(wake)),
            thread: Mutex::new(thread),
            attachments: AtomicU64::new(0),
            #[cfg(test)]
            listening,
        }))
    }

    /// Listens to `port` only (as the settings keep it), or to every port.
    pub fn choose(&self, port: Option<String>) {
        if let Ok(wake) = self.0.wake.lock()
            && let Some(wake) = wake.as_ref()
        {
            let _ = wake.send(Wake::Choose(port));
        }
    }

    /// The keys go to `keys` until what comes back is dropped, or another is attached.
    pub fn attach(&self, keys: Arc<dyn Keys>) -> Attached {
        let id = self.0.attachments.fetch_add(1, Relaxed) + 1;
        lock(&self.0.hub).keys = Some((id, keys));
        Attached {
            hub: self.0.hub.clone(),
            id,
        }
    }

    /// The ports listened to now.
    #[cfg(test)]
    fn listening(&self) -> Vec<String> {
        let listening = self.0.listening.lock().unwrap();
        listening.iter().map(|port| port.name.clone()).collect()
    }
}

/// The keys go to a [`Keys`] while it lives.
pub struct Attached {
    hub: Arc<Mutex<Hub>>,
    id: u64,
}

impl Drop for Attached {
    fn drop(&mut self) {
        let mut hub = lock(&self.hub);
        if hub.keys.as_ref().is_some_and(|(id, _)| *id == self.id) {
            hub.keys = None;
        }
    }
}

struct Connection {
    port: PortInfo,
    serial: u64,
    /// Dropped, the port is not listened to any more.
    link: Box<dyn Send>,
}

/// The thread that watches the ports.
struct Watcher {
    ports: Arc<dyn Ports>,
    hub: Arc<Mutex<Hub>>,
    every: Duration,
    connections: Vec<Connection>,
    /// The ports that could not be connected to, not tried again while they are listed.
    refused: Vec<PortInfo>,
    serials: u64,
    #[cfg(test)]
    listening: Arc<Mutex<Vec<PortInfo>>>,
}

impl Watcher {
    fn run(mut self, mut choice: Option<String>, woken: &Receiver<Wake>) {
        loop {
            self.look(choice.as_deref());
            match woken.recv_timeout(self.every) {
                Ok(Wake::Choose(chosen)) => {
                    choice = chosen;
                    // A new choice tries every port again.
                    self.refused.clear();
                }
                Err(RecvTimeoutError::Timeout) => {}
                // MIDI in is gone: the app is.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        for connection in std::mem::take(&mut self.connections) {
            self.close(connection);
        }
    }

    /// Lists the ports, lets go of those gone or not wanted, connects those new.
    fn look(&mut self, choice: Option<&str>) {
        let Some(list) = self.ports.input_list() else {
            return;
        };
        self.refused.retain(|port| list.contains(port));
        let wanted = wanted(&list, choice);
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.connections)
            .into_iter()
            .partition(|connection| wanted.contains(&&connection.port));
        self.connections = kept;
        for connection in gone {
            self.close(connection);
        }
        for port in wanted {
            let known = self.connections.iter().any(|c| &c.port == port);
            if known || self.refused.contains(port) {
                continue;
            }
            match self.connect(port) {
                Ok(connection) => self.connections.push(connection),
                Err(error) => {
                    eprintln!("gamme: MIDI in: {error}");
                    self.refused.push(port.clone());
                }
            }
        }
        #[cfg(test)]
        {
            *self.listening.lock().unwrap() =
                self.connections.iter().map(|c| c.port.clone()).collect();
        }
    }

    fn connect(&mut self, port: &PortInfo) -> Result<Connection, String> {
        self.serials += 1;
        let serial = self.serials;
        // Known before the first message can come.
        lock(&self.hub)
            .connections
            .push((serial, port.clone(), [false; 128]));
        let hub = self.hub.clone();
        let on_message: OnMessage = Box::new(move |message| {
            // On arrival, before anything waits.
            let at = Instant::now();
            lock(&hub).message(serial, message, at);
        });
        match self.ports.connect_input(port, on_message) {
            Ok(link) => Ok(Connection {
                port: port.clone(),
                serial,
                link,
            }),
            Err(error) => {
                lock(&self.hub).close(serial, Instant::now());
                Err(error)
            }
        }
    }

    fn close(&self, connection: Connection) {
        let Connection { serial, link, .. } = connection;
        // Without the hub's lock: closing waits for the library's thread, which may be waiting
        // for the lock with a message.
        drop(link);
        lock(&self.hub).close(serial, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_out::testing::FakePorts;

    const EVERY: Duration = Duration::from_millis(10);
    const KEYSTATION: &str = "Keystation 49:Keystation 49 MIDI 1 40:0";
    const VMPK: &str = "VMPK Output:out 128:0";

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

    /// What the keys were, from where.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<(Played, String)>>);

    impl Keys for Recorder {
        fn played(&self, played: Played, from: &PortInfo) {
            self.0.lock().unwrap().push((played, from.name.clone()));
        }
    }

    impl Recorder {
        /// Each key and velocity, in order.
        fn keys(&self) -> Vec<(u8, u8)> {
            let played = self.0.lock().unwrap();
            played.iter().map(|(p, _)| (p.key, p.velocity)).collect()
        }
    }

    fn start(ports: &Arc<FakePorts>, choice: Option<&str>) -> MidiIn {
        let choice = choice.map(str::to_owned);
        MidiIn::watching(ports.clone(), move || choice, EVERY)
    }

    fn listening(midi: &MidiIn, names: &[&str]) {
        until(&format!("listening to {names:?}"), || {
            let mut listening = midi.listening();
            listening.sort();
            let mut names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
            names.sort();
            listening == names
        });
    }

    #[test]
    fn a_message_is_a_key_struck_or_let_go_or_nothing() {
        assert_eq!(note(&[0x90, 60, 100]), Some((60, 100)));
        // Any channel, and a note on at velocity 0 is a release.
        assert_eq!(note(&[0x93, 61, 0]), Some((61, 0)));
        assert_eq!(note(&[0x85, 62, 64]), Some((62, 0)));
        assert_eq!(note(&[0xB0, 64, 127]), None);
        assert_eq!(note(&[0xFE]), None);
        assert_eq!(note(&[0x90, 60]), None);
    }

    #[test]
    fn every_port_is_listened_to_as_it_appears_and_again_after_it_was_unplugged() {
        let ports = Arc::new(FakePorts::new(&[], &[KEYSTATION]));
        let midi = start(&ports, None);
        listening(&midi, &[KEYSTATION]);
        ports.plug_input(VMPK, "128:0");
        listening(&midi, &[KEYSTATION, VMPK]);
        ports.unplug_input(KEYSTATION);
        listening(&midi, &[VMPK]);
        assert_eq!(ports.listened(), [VMPK], "its connection is closed");
        ports.plug_input(KEYSTATION, "40:0");
        listening(&midi, &[KEYSTATION, VMPK]);
        assert_eq!(ports.input_connections(), 3);
    }

    #[test]
    fn the_keys_come_with_the_instant_they_arrived() {
        let ports = Arc::new(FakePorts::new(&[], &[VMPK]));
        let midi = start(&ports, None);
        let keys = Arc::new(Recorder::default());
        let _attached = midi.attach(keys.clone());
        listening(&midi, &[VMPK]);
        let before = Instant::now();
        assert!(ports.send_from(VMPK, &[0x90, 60, 100]));
        let after = Instant::now();
        ports.send_from(VMPK, &[0xB0, 64, 127]);
        ports.send_from(VMPK, &[0x90, 60, 0]);
        // Let go of a key never struck here: nothing.
        ports.send_from(VMPK, &[0x80, 61, 0]);
        assert_eq!(keys.keys(), [(60, 100), (60, 0)]);
        let played = keys.0.lock().unwrap();
        let (first, from) = &played[0];
        assert!(before <= first.at && first.at <= after);
        assert!(played[1].0.at >= first.at);
        assert_eq!(from, VMPK);
    }

    #[test]
    fn a_key_held_on_two_keyboards_is_let_go_with_the_last() {
        let ports = Arc::new(FakePorts::new(&[], &[KEYSTATION, VMPK]));
        let midi = start(&ports, None);
        let keys = Arc::new(Recorder::default());
        let _attached = midi.attach(keys.clone());
        listening(&midi, &[KEYSTATION, VMPK]);
        ports.send_from(KEYSTATION, &[0x90, 60, 100]);
        ports.send_from(VMPK, &[0x90, 60, 80]);
        ports.send_from(KEYSTATION, &[0x80, 60, 0]);
        assert_eq!(keys.keys(), [(60, 100), (60, 80)]);
        ports.send_from(VMPK, &[0x80, 60, 0]);
        assert_eq!(keys.keys()[2..], [(60, 0)]);
    }

    #[test]
    fn the_keys_held_on_a_port_that_goes_are_let_go() {
        let ports = Arc::new(FakePorts::new(&[], &[KEYSTATION]));
        let midi = start(&ports, None);
        let keys = Arc::new(Recorder::default());
        let _attached = midi.attach(keys.clone());
        listening(&midi, &[KEYSTATION]);
        ports.send_from(KEYSTATION, &[0x90, 60, 100]);
        ports.send_from(KEYSTATION, &[0x90, 64, 100]);
        ports.unplug_input(KEYSTATION);
        until("the keys let go", || keys.keys().len() == 4);
        let mut released = keys.keys()[2..].to_vec();
        released.sort();
        assert_eq!(released, [(60, 0), (64, 0)]);
    }

    #[test]
    fn a_list_that_cannot_be_read_is_not_a_port_that_is_gone() {
        let ports = Arc::new(FakePorts::new(&[], &[KEYSTATION]));
        let midi = start(&ports, None);
        listening(&midi, &[KEYSTATION]);
        ports.cannot_look();
        ports.unplug_input(KEYSTATION);
        std::thread::sleep(EVERY * 10);
        assert_eq!(midi.listening(), [KEYSTATION]);
        ports.can_look();
        listening(&midi, &[]);
    }

    #[test]
    fn only_the_port_chosen_is_listened_to_and_the_choice_changes_at_once() {
        let ports = Arc::new(FakePorts::new(&[], &[KEYSTATION, VMPK]));
        // As the settings keep it: without ALSA's numbers.
        let midi = start(&ports, Some("VMPK Output:out"));
        listening(&midi, &[VMPK]);
        midi.choose(None);
        listening(&midi, &[KEYSTATION, VMPK]);
        midi.choose(Some("Digital Piano".into()));
        listening(&midi, &[]);
        ports.plug_input("Digital Piano:Digital Piano MIDI 1 24:0", "24:0");
        // The settings keep the name past ALSA's numbers; a port is found by it.
        midi.choose(Some("Digital Piano:Digital Piano MIDI 1".into()));
        listening(&midi, &["Digital Piano:Digital Piano MIDI 1 24:0"]);
    }

    #[test]
    fn gammes_own_ports_are_never_listened_to_and_midi_through_only_when_chosen() {
        let through = "Midi Through:Midi Through Port-0 14:0";
        let own = "Gamme:Gamme out 130:0";
        let ports = Arc::new(FakePorts::new(&[], &[through, own, VMPK]));
        let midi = start(&ports, None);
        listening(&midi, &[VMPK]);
        midi.choose(Some("Midi Through:Midi Through Port-0".into()));
        listening(&midi, &[through]);
        midi.choose(Some("Gamme:Gamme out".into()));
        listening(&midi, &[]);
    }

    #[test]
    fn a_port_that_cannot_be_connected_to_is_not_tried_at_each_look() {
        let ports = Arc::new(FakePorts::new(&[], &[KEYSTATION]));
        ports.refuse_inputs(true);
        let midi = start(&ports, None);
        until("a try", || ports.input_connections() == 1);
        std::thread::sleep(EVERY * 10);
        assert_eq!(ports.input_connections(), 1);
        // Plugged again, it is tried again.
        ports.refuse_inputs(false);
        ports.unplug_input(KEYSTATION);
        std::thread::sleep(EVERY * 5);
        ports.plug_input(KEYSTATION, "40:0");
        listening(&midi, &[KEYSTATION]);
        assert_eq!(ports.input_connections(), 2);
    }

    #[test]
    fn the_keys_go_only_to_what_is_attached_now() {
        let ports = Arc::new(FakePorts::new(&[], &[VMPK]));
        let midi = start(&ports, None);
        listening(&midi, &[VMPK]);
        let first = Arc::new(Recorder::default());
        let second = Arc::new(Recorder::default());
        let attached = midi.attach(first.clone());
        ports.send_from(VMPK, &[0x90, 60, 100]);
        let second_attached = midi.attach(second.clone());
        // The first one going does not take the second one's place away.
        drop(attached);
        ports.send_from(VMPK, &[0x80, 60, 0]);
        assert_eq!(first.keys(), [(60, 100)]);
        assert_eq!(second.keys(), [(60, 0)]);
        drop(second_attached);
        ports.send_from(VMPK, &[0x90, 62, 100]);
        assert_eq!(second.keys().len(), 1);
    }

    #[test]
    fn midi_in_going_lets_go_of_its_ports() {
        let ports = Arc::new(FakePorts::new(&[], &[VMPK]));
        let midi = start(&ports, None);
        listening(&midi, &[VMPK]);
        drop(midi);
        assert!(ports.listened().is_empty());
    }
}
