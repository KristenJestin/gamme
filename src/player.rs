//! A piece being played: its timeline, its clock, and the output that sounds it: the built-in
//! piano, or a MIDI output (`midi_out.rs`), both an [`Audio`].
//!
//! The window holds a [`Player`] and calls it: `play`, `pause`, `seek_measure`, `set_speed`. The
//! first `play` has the output made ready on a thread of its own (for the built-in piano, the
//! SoundFont fetched if it is not there yet, loaded, the audio output opened) and the clock starts
//! when it is; what the window shows meanwhile is [`Player::sound`] and [`Player::progress`]. The
//! thread keeps the output (a `cpal` stream is not `Send`) until the player goes.
//!
//! An output that cannot be opened, or that is lost while the piece plays (a MIDI port unplugged),
//! gives way to its [`Audio::fallback`] when it has one: the piece goes on from where it stood and
//! [`Player::notice`] says what happened.
//!
//! The keys the player strikes (MIDI in) sound on the same output, through [`Player::keys`]:
//! [`Player::warm`] opens it without playing, so that they sound before the piece does.

use std::any::Any;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rustysynth::SoundFont as RustySoundFont;

use crate::clock::{Clock, Speed};
use crate::piano::{self, Hands};
use crate::practice::Chords;
use crate::soundfont::{self, Fetcher, Font, Progress};
use crate::timeline::Timeline;
use crate::touch::Touch;

/// How long a player that goes waits for its output to be closed: a port that does not answer
/// must not hold the window.
const CLOSE_WAIT: Duration = Duration::from_millis(500);

/// Where the sound of the piece stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sound {
    /// Nothing asked for yet: the first `play` prepares it.
    Off,
    /// The SoundFont is being fetched, checked, unpacked or loaded: see [`Player::progress`].
    Preparing,
    Ready,
    Failed(String),
}

/// What opens the sound once the SoundFont is there; the real one is [`Builtin`].
pub trait Audio: Send + Sync {
    /// The open output, which must stay alive for as long as the piece may sound. It stays on
    /// the thread that made it. `lost` is set when the output stops working (a device unplugged).
    fn open(
        &self,
        soundfont: &Path,
        timeline: &Timeline,
        clock: &Clock,
        hands: &Hands,
        lost: &Arc<AtomicBool>,
    ) -> Result<Box<dyn Any>, String>;

    /// Whether `open` is given a SoundFont to play: the built-in piano's, not a MIDI output's.
    fn needs_soundfont(&self) -> bool {
        true
    }

    /// What the output is called, when it is not the built-in piano ("MIDI output “Piano”").
    fn label(&self) -> Option<String> {
        None
    }

    /// What plays instead when this output cannot be opened or is lost.
    fn fallback(&self) -> Option<Arc<dyn Audio>> {
        None
    }

    /// A key the player strikes (`velocity` above 0) or lets go, on the input port `from` (its
    /// id), sounded on the output open now; nothing when none is. Called on MIDI's threads.
    fn live(&self, _key: u8, _velocity: u8, _from: &str) {}

    /// The player's sustain pedal (its value, 64 and above down), as [`Audio::live`].
    fn live_pedal(&self, _value: u8, _from: &str) {}
}

/// The built-in piano: `rustysynth` on the default output.
#[derive(Default)]
pub struct Builtin {
    /// The player's keys, read by the output each one opens.
    keys: piano::LiveKeys,
    /// How the player's keys are struck (the settings'): the piece's notes are not touched by it.
    touch: Touch,
}

impl Builtin {
    pub fn new(touch: Touch) -> Self {
        Self {
            keys: piano::LiveKeys::default(),
            touch,
        }
    }
}

impl Audio for Builtin {
    fn open(
        &self,
        soundfont: &Path,
        timeline: &Timeline,
        clock: &Clock,
        hands: &Hands,
        lost: &Arc<AtomicBool>,
    ) -> Result<Box<dyn Any>, String> {
        let file = File::open(soundfont).map_err(|e| format!("cannot open the SoundFont: {e}"))?;
        let font = RustySoundFont::new(&mut BufReader::new(file))
            .map_err(|e| format!("cannot read the SoundFont: {e}"))?;
        let output = piano::start(&Arc::new(font), timeline, clock, hands, lost, &self.keys)?;
        Ok(Box::new(output))
    }

    fn live(&self, key: u8, velocity: u8, _from: &str) {
        // On MIDI's thread, before the ring: the audio callback only plays what it finds.
        self.keys.push(key, self.touch.velocity(velocity));
    }

    fn live_pedal(&self, value: u8, _from: &str) {
        self.keys.push_pedal(value);
    }
}

/// The keys the player strikes, to the player's output, whichever it is now (the one chosen or
/// the built-in piano that took over). Cloned to MIDI in's threads.
#[derive(Clone)]
pub struct Keys(Arc<Shared>);

impl Keys {
    pub fn note(&self, key: u8, velocity: u8, from: &str) {
        self.0.audio().live(key, velocity, from);
    }

    pub fn pedal(&self, value: u8, from: &str) {
        self.0.audio().live_pedal(value, from);
    }
}

struct Shared {
    /// What sounds the piece: the one the player was given, then its fallback if it gave way.
    audio: Mutex<Arc<dyn Audio>>,
    /// What the window is to say once: the output was replaced, and why.
    notice: Mutex<Option<String>>,
    /// An output gave way and its fallback is not open yet: what is to be said of it, which is
    /// not said before the fallback plays (it may fail to open too).
    pending: Mutex<Option<String>>,
    sound: Mutex<Sound>,
    /// The player pressed play and the clock has not started yet.
    play_when_ready: AtomicBool,
    /// The flag of the output opened last, set by its error callback (an atomic: nothing else is
    /// allowed there). Each output has its own: one that is closing, and says it is lost on its
    /// way out, must not condemn the output that took its place.
    device_lost: Mutex<Arc<AtomicBool>>,
    progress: Progress,
}

pub struct Player {
    /// Made with the piece's measures and the player's chords, for the loop and the wait.
    pub clock: Clock,
    pub timeline: Arc<Timeline>,
    /// The hands the computer plays.
    pub hands: Hands,
    /// The player's chords, the clock's to wait at and the window's to judge.
    pub chords: Arc<Chords>,
    data: PathBuf,
    font: Font,
    fetcher: Arc<dyn Fetcher>,
    shared: Arc<Shared>,
    /// Dropping it ends the thread that holds the output.
    stop: Mutex<Option<Sender<()>>>,
    /// That thread, so that the next one waits for the output to be closed before it opens its own.
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Player {
    /// `data` is `$XDG_DATA_HOME/gamme`.
    pub fn new(
        timeline: Timeline,
        data: PathBuf,
        font: Font,
        fetcher: Arc<dyn Fetcher>,
        audio: Arc<dyn Audio>,
    ) -> Self {
        let chords = Chords::of(&timeline);
        Self {
            clock: Clock::with_marks(timeline.duration_ms(), &chords.marks(&timeline)),
            timeline: Arc::new(timeline),
            hands: Hands::both(),
            chords: Arc::new(chords),
            data,
            font,
            fetcher,
            shared: Arc::new(Shared {
                audio: Mutex::new(audio),
                notice: Mutex::new(None),
                pending: Mutex::new(None),
                sound: Mutex::new(Sound::Off),
                play_when_ready: AtomicBool::new(false),
                device_lost: Mutex::new(Arc::new(AtomicBool::new(false))),
                progress: Progress::default(),
            }),
            stop: Mutex::new(None),
            thread: Mutex::new(None),
        }
    }

    pub fn sound(&self) -> Sound {
        let mut sound = self.shared.sound.lock().expect("the sound's lock");
        if *sound == Sound::Ready && self.shared.lost() {
            if self.shared.fall_back("was lost") {
                // The piece goes on from where it stood, on what takes over.
                let was_playing = self.clock.playing() || self.shared.play_when_ready.load(Relaxed);
                self.clock.pause();
                self.shared.play_when_ready.store(was_playing, Relaxed);
                *sound = Sound::Preparing;
                drop(sound);
                self.prepare();
                return Sound::Preparing;
            }
            // The piece would run on in silence: stop it and say why.
            *sound = Sound::Failed("the audio output was lost".into());
            self.shared.play_when_ready.store(false, Relaxed);
            self.clock.pause();
        }
        sound.clone()
    }

    /// What the window is to tell the player, once: the output was replaced by the built-in
    /// piano, and why.
    pub fn notice(&self) -> Option<String> {
        self.shared.notice.lock().expect("the notice's lock").take()
    }

    /// What the piece sounds on, when it is not the built-in piano ("MIDI output “Piano”").
    pub fn output_label(&self) -> Option<String> {
        self.shared.audio().label()
    }

    pub fn progress(&self) -> &Progress {
        &self.shared.progress
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    /// Where the keys the player strikes go to be heard.
    pub fn keys(&self) -> Keys {
        Keys(self.shared.clone())
    }

    /// Makes the output ready without playing, if nothing was asked of it yet: the keys the
    /// player strikes are heard before the piece plays.
    pub fn warm(&self) {
        let mut sound = self.shared.sound.lock().expect("the sound's lock");
        if *sound == Sound::Off {
            *sound = Sound::Preparing;
            drop(sound);
            self.prepare();
        }
    }

    /// Whether the piece is playing, or will as soon as the piano is ready.
    pub fn wants_to_play(&self) -> bool {
        self.clock.playing() || self.shared.play_when_ready.load(Relaxed)
    }

    pub fn play(&mut self) {
        // Looks first, so that a lost output is seen as lost.
        self.sound();
        // The state and the wish to play change together, under the lock the thread that makes
        // the piano ready takes to say it is: a play is never lost between the two.
        let mut sound = self.shared.sound.lock().expect("the sound's lock");
        match &*sound {
            Sound::Ready => self.clock.play(),
            Sound::Preparing => self.shared.play_when_ready.store(true, Relaxed),
            Sound::Off | Sound::Failed(_) => {
                self.shared.play_when_ready.store(true, Relaxed);
                *sound = Sound::Preparing;
                drop(sound);
                self.prepare();
            }
        }
    }

    pub fn pause(&mut self) {
        let _sound = self.shared.sound.lock().expect("the sound's lock");
        self.shared.play_when_ready.store(false, Relaxed);
        self.clock.pause();
    }

    pub fn toggle(&mut self) {
        if self.wants_to_play() {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn set_speed(&self, percent: u32) {
        self.clock.set_speed(Speed::new(percent));
    }

    pub fn seek_measure(&self, index: usize) -> bool {
        self.clock.seek_measure(&self.timeline, index)
    }

    /// Stops what `prepare` started; the part of a download is kept.
    pub fn cancel_download(&self) {
        self.shared.progress.cancel();
    }

    /// Lets the output go and waits for it to be closed (its keys let go), for `wait` at most:
    /// what the window does when it quits, since a thread that is detached is not waited for.
    pub fn shutdown(&self, wait: Duration) {
        self.shared.progress.cancel();
        // The thread ends with its sender, and drops the output.
        self.stop.lock().expect("the stop's lock").take();
        let Some(thread) = self.thread.lock().expect("the thread's lock").take() else {
            return;
        };
        let (done, finished) = mpsc::channel::<()>();
        let waiter = std::thread::Builder::new()
            .name("gamme-audio-wait".into())
            .spawn(move || {
                let _ = thread.join();
                let _ = done.send(());
            });
        if waiter.is_ok() {
            let _ = finished.recv_timeout(wait);
        }
    }

    /// Starts the thread that makes the output ready; the state is `Preparing` already.
    fn prepare(&self) {
        // A new job: what the last one met does not apply to it.
        self.shared.progress.reset();
        self.shared.fresh_lost();
        let previous = self.thread.lock().expect("the thread's lock").take();
        let (stop, stopped) = mpsc::channel::<()>();
        // The last output's thread ends with its sender, and lets its keys go.
        *self.stop.lock().expect("the stop's lock") = Some(stop);
        let job = Job {
            shared: self.shared.clone(),
            data: self.data.clone(),
            font: self.font,
            fetcher: self.fetcher.clone(),
            timeline: self.timeline.clone(),
            clock: self.clock.clone(),
            hands: self.hands.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("gamme-audio".into())
            .spawn(move || {
                // The last output is closed first: it renders no more, so it cannot take the
                // clock again after it is released, and a MIDI port is let go of before the
                // next one opens.
                if let Some(previous) = previous {
                    let _ = previous.join();
                }
                // An output lost or never made drives the clock no more: the wall clock does
                // until the new one renders.
                job.clock.release();
                let output = job.run();
                // The output lives until the player goes (the channel closes).
                let _ = stopped.recv();
                drop(output);
            });
        match spawned {
            Ok(thread) => *self.thread.lock().expect("the thread's lock") = Some(thread),
            Err(error) => self
                .shared
                .fail(format!("cannot start the audio thread: {error}")),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // A window closed during the download does not leave curl running; the part is kept.
        // The output is closed, and a MIDI one lets its keys go, before the player is gone.
        self.shutdown(CLOSE_WAIT);
    }
}

struct Job {
    shared: Arc<Shared>,
    data: PathBuf,
    font: Font,
    fetcher: Arc<dyn Fetcher>,
    timeline: Arc<Timeline>,
    clock: Clock,
    hands: Hands,
}

impl Job {
    /// Opens `audio`: the SoundFont first when it plays one.
    fn open(&self, audio: &dyn Audio) -> Result<Box<dyn Any>, String> {
        // The output's own flag: a dead one's cannot say this one is lost.
        let lost = self.shared.fresh_lost();
        let soundfont = if audio.needs_soundfont() {
            soundfont::install(
                &self.font,
                &self.data,
                &*self.fetcher,
                &self.shared.progress,
            )
            .map_err(|e| e.to_string())?
        } else {
            PathBuf::new()
        };
        audio.open(&soundfont, &self.timeline, &self.clock, &self.hands, &lost)
    }

    fn run(self) -> Option<Box<dyn Any>> {
        let mut opened = self.open(&*self.shared.audio());
        if let Err(why) = &opened
            && self.shared.fall_back(&format!("cannot be used ({why})"))
        {
            opened = self.open(&*self.shared.audio());
        }
        match opened {
            Ok(output) => {
                // An output gave way: it is said now that what takes over plays.
                if let Some(said) = self
                    .shared
                    .pending
                    .lock()
                    .expect("the notice's lock")
                    .take()
                {
                    *self.shared.notice.lock().expect("the notice's lock") =
                        Some(format!("{said} The built-in piano plays instead."));
                }
                // Ready and the start of a piece asked for meanwhile, in one go.
                let mut sound = self.shared.sound.lock().expect("the sound's lock");
                *sound = Sound::Ready;
                if self.shared.play_when_ready.swap(false, Relaxed) {
                    self.clock.play();
                }
                Some(output)
            }
            Err(message) => {
                // What gave way is said alone: the failure of what took over is the state's.
                if let Some(said) = self
                    .shared
                    .pending
                    .lock()
                    .expect("the notice's lock")
                    .take()
                {
                    *self.shared.notice.lock().expect("the notice's lock") = Some(said);
                }
                self.shared.fail(message);
                None
            }
        }
    }
}

impl Shared {
    fn audio(&self) -> Arc<dyn Audio> {
        self.audio.lock().expect("the audio's lock").clone()
    }

    /// The flag a new output is opened with, which is the one [`Shared::lost`] reads from now on.
    fn fresh_lost(&self) -> Arc<AtomicBool> {
        let fresh = Arc::new(AtomicBool::new(false));
        *self.device_lost.lock().expect("the lost flag's lock") = fresh.clone();
        fresh
    }

    /// Whether the output opened last says it is lost.
    fn lost(&self) -> bool {
        self.device_lost
            .lock()
            .expect("the lost flag's lock")
            .load(Relaxed)
    }

    /// Puts the output's fallback in its place, if it has one. What is to be told of it (`why`
    /// follows the output's name) waits for the fallback to open.
    fn fall_back(&self, why: &str) -> bool {
        let mut audio = self.audio.lock().expect("the audio's lock");
        let Some(next) = audio.fallback() else {
            return false;
        };
        let name = audio.label().unwrap_or_else(|| "The sound output".into());
        *self.pending.lock().expect("the notice's lock") = Some(format!("{name} {why}."));
        *audio = next;
        true
    }

    fn fail(&self, message: String) {
        self.play_when_ready.store(false, Relaxed);
        *self.sound.lock().expect("the sound's lock") = Sound::Failed(message);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::soundfont::{Archive, Error};
    use crate::timeline::{Hand, Note};

    #[test]
    fn the_built_in_piano_shapes_the_players_keys_by_the_touch_before_the_ring() {
        let strike = |touch: Touch| {
            let piano = Builtin::new(touch);
            piano.live(60, 30, "20:0");
            piano.live(60, 0, "20:0");
            piano.live_pedal(127, "20:0");
            piano.keys.take_keys()
        };
        assert_eq!(strike(Touch::Normal), [(60, 30), (60, 0)]);
        // A soft key lifted, a key let go still a release.
        assert_eq!(strike(Touch::Light), [(60, 62), (60, 0)]);
        assert_eq!(strike(Touch::Heavy), [(60, 15), (60, 0)]);
        assert_eq!(strike(Touch::Fixed), [(60, 100), (60, 0)]);
        // The settings' default.
        assert_eq!(strike(Builtin::default().touch), strike(Touch::Light));
    }

    /// A fetcher that writes a "SoundFont" of its own and waits to be let go.
    struct Gate {
        released: Arc<AtomicBool>,
        calls: Mutex<u32>,
    }

    impl Fetcher for Gate {
        fn fetch(&self, _: &str, partial: &Path, progress: &Progress) -> Result<(), Error> {
            *self.calls.lock().unwrap() += 1;
            while !self.released.load(Relaxed) {
                if progress.cancelled() {
                    return Err(Error::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            std::fs::write(partial, b"archive")?;
            progress.set_downloaded(7);
            Ok(())
        }
    }

    /// An output that records what it was opened on, and keeps the flag it would set when the
    /// device is lost.
    struct Opened {
        paths: Mutex<Vec<PathBuf>>,
        lost: Mutex<Option<Arc<AtomicBool>>>,
        /// The player's keys it was given.
        keys: Mutex<Vec<(u8, u8)>>,
    }

    impl Opened {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                paths: Mutex::new(Vec::new()),
                lost: Mutex::new(None),
                keys: Mutex::new(Vec::new()),
            })
        }
    }

    impl Audio for Opened {
        fn open(
            &self,
            path: &Path,
            _: &Timeline,
            _: &Clock,
            _: &Hands,
            lost: &Arc<AtomicBool>,
        ) -> Result<Box<dyn Any>, String> {
            self.paths.lock().unwrap().push(path.to_owned());
            *self.lost.lock().unwrap() = Some(lost.clone());
            Ok(Box::new(()))
        }

        fn live(&self, key: u8, velocity: u8, _: &str) {
            self.keys.lock().unwrap().push((key, velocity));
        }

        /// The pedal, as a key 128 can never be.
        fn live_pedal(&self, value: u8, _: &str) {
            self.keys.lock().unwrap().push((128, value));
        }
    }

    /// A font whose archive is the 7z fixture of `soundfont.rs`' tests, so that a real install
    /// runs: the fetcher below hands that archive out.
    const SEVEN_ZIP: &[u8] = include_bytes!("../tests/fixtures/fake.7z");

    struct Serves(Arc<AtomicBool>);

    impl Fetcher for Serves {
        fn fetch(&self, _: &str, partial: &Path, progress: &Progress) -> Result<(), Error> {
            while !self.0.load(Relaxed) {
                if progress.cancelled() {
                    return Err(Error::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            std::fs::write(partial, SEVEN_ZIP)?;
            progress.set_downloaded(SEVEN_ZIP.len() as u64);
            Ok(())
        }
    }

    fn font() -> Font {
        use sha2::{Digest as _, Sha256};
        let sum: String = Sha256::digest(SEVEN_ZIP)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Font {
            id: "fake",
            name: "Fake piano",
            author: "nobody",
            licence: "CC0 1.0",
            url: "https://example.test/fake.7z",
            archive: Archive::SevenZip,
            archive_bytes: SEVEN_ZIP.len() as u64,
            archive_sha256: Box::leak(sum.into_boxed_str()),
            member: "fake/fake.sf2",
            bytes: 16,
        }
    }

    fn timeline() -> Timeline {
        let note = Note {
            key: 60,
            velocity: 90,
            on_ms: 0.,
            off_ms: 500.,
            hand: Hand::Right,
            part: 0,
            measure: 0,
            element: None,
        };
        Timeline::new(vec![note], vec![0., 2000.], 4000.)
    }

    fn temp(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("gamme-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "waited for {what}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_first_play_fetches_the_soundfont_then_sounds() {
        let data = temp("player-first");
        let gate = Arc::new(AtomicBool::new(false));
        let audio = Opened::new();
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(gate.clone())),
            audio.clone(),
        );
        assert_eq!(player.sound(), Sound::Off);

        player.play();
        assert_eq!(player.sound(), Sound::Preparing);
        assert!(player.wants_to_play());
        // Nothing moves while the piano is not there.
        assert!(!player.clock.playing());

        gate.store(true, Relaxed);
        until("the piano", || player.sound() == Sound::Ready);
        assert!(player.clock.playing());
        assert_eq!(
            *audio.paths.lock().unwrap(),
            [data.join("soundfonts/fake.sf2")]
        );

        // The second play of the session needs nothing.
        player.pause();
        assert!(!player.clock.playing());
        player.play();
        assert!(player.clock.playing());
        drop(player);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_lost_output_stops_the_clock_and_is_said() {
        let data = temp("player-lost");
        let audio = Opened::new();
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            audio.clone(),
        );
        player.play();
        until("the piano", || player.sound() == Sound::Ready);
        assert!(player.clock.playing());

        // What the stream's error callback does.
        audio
            .lost
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store(true, Relaxed);
        assert_eq!(
            player.sound(),
            Sound::Failed("the audio output was lost".into())
        );
        assert!(!player.clock.playing());
        assert!(!player.wants_to_play());

        // A new play opens the output again.
        player.play();
        until("the piano again", || player.sound() == Sound::Ready);
        assert!(player.clock.playing());
        assert_eq!(audio.paths.lock().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_pause_while_it_downloads_keeps_the_clock_stopped_when_the_piano_is_ready() {
        let data = temp("player-pause");
        let gate = Arc::new(AtomicBool::new(false));
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(gate.clone())),
            Opened::new(),
        );
        player.play();
        player.pause();
        assert!(!player.wants_to_play());
        gate.store(true, Relaxed);
        until("the piano", || player.sound() == Sound::Ready);
        assert!(!player.clock.playing());
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_failure_is_said_and_the_next_play_tries_again() {
        let data = temp("player-failure");
        let released = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(Gate {
            released: released.clone(),
            calls: Mutex::new(0),
        });
        // `Gate` writes 7 bytes that are not the archive: the check fails.
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            gate.clone(),
            Opened::new(),
        );
        released.store(true, Relaxed);
        player.play();
        until("the failure", || matches!(player.sound(), Sound::Failed(_)));
        assert!(!player.wants_to_play());
        player.play();
        until("the second failure", || {
            matches!(player.sound(), Sound::Failed(_))
        });
        assert!(*gate.calls.lock().unwrap() >= 2);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_warm_player_opens_its_output_without_playing_and_once() {
        let data = temp("player-warm");
        let audio = Opened::new();
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            audio.clone(),
        );
        player.warm();
        until("the piano", || player.sound() == Sound::Ready);
        assert!(!player.clock.playing());
        assert!(!player.wants_to_play());
        // Warm again, or play: the output that is open stays.
        player.warm();
        player.play();
        assert!(player.clock.playing());
        assert_eq!(audio.paths.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn the_keys_struck_go_to_the_output_in_place_now() {
        let data = temp("player-keys");
        let piano = Opened::new();
        let midi = Midi::new(piano.clone(), false);
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            midi.clone(),
        );
        let keys = player.keys();
        player.play();
        until("the output", || player.sound() == Sound::Ready);
        keys.note(60, 90, "24:0");
        assert_eq!(*midi.keys.lock().unwrap(), [(60, 90)]);
        // The MIDI output is lost: the built-in piano that takes over gets them.
        let (lost, _) = midi.opened.lock().unwrap().clone().unwrap();
        lost.store(true, Relaxed);
        until("the built-in piano", || player.sound() == Sound::Ready);
        keys.note(60, 0, "24:0");
        keys.pedal(127, "24:0");
        assert_eq!(*piano.keys.lock().unwrap(), [(60, 0), (128, 127)]);
        assert_eq!(midi.keys.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn seek_and_speed_reach_the_clock() {
        let mut player = Player::new(
            timeline(),
            temp("player-seek"),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            Opened::new(),
        );
        assert!(player.seek_measure(1));
        assert_eq!(player.clock.snapshot().position_us, 2_000_000);
        assert!(!player.seek_measure(5));
        player.set_speed(10);
        assert_eq!(player.clock.speed(), Speed::MIN);
        player.set_speed(50);
        assert_eq!(player.clock.speed().factor(), 0.5);
        player.pause();
    }

    /// Says it is closed when the player lets go of it.
    struct Closes(Arc<AtomicBool>);

    impl Drop for Closes {
        fn drop(&mut self) {
            self.0.store(true, Relaxed);
        }
    }

    /// A MIDI output as the player sees it: it plays no SoundFont, and the built-in piano (here
    /// `Opened`) stands by.
    struct Midi {
        fallback: Arc<dyn Audio>,
        /// Whether the port is not there.
        absent: bool,
        opened: Mutex<Option<(Arc<AtomicBool>, Arc<AtomicBool>)>>,
        keys: Mutex<Vec<(u8, u8)>>,
    }

    impl Midi {
        fn new(fallback: Arc<dyn Audio>, absent: bool) -> Arc<Self> {
            Arc::new(Self {
                fallback,
                absent,
                opened: Mutex::new(None),
                keys: Mutex::new(Vec::new()),
            })
        }
    }

    impl Audio for Midi {
        fn open(
            &self,
            path: &Path,
            _: &Timeline,
            _: &Clock,
            _: &Hands,
            lost: &Arc<AtomicBool>,
        ) -> Result<Box<dyn Any>, String> {
            assert_eq!(path, Path::new(""), "no SoundFont for a MIDI output");
            if self.absent {
                return Err("it is not connected".into());
            }
            let closed = Arc::new(AtomicBool::new(false));
            *self.opened.lock().unwrap() = Some((lost.clone(), closed.clone()));
            Ok(Box::new(Closes(closed)))
        }

        fn needs_soundfont(&self) -> bool {
            false
        }

        fn label(&self) -> Option<String> {
            Some("MIDI output “Test piano”".into())
        }

        fn fallback(&self) -> Option<Arc<dyn Audio>> {
            Some(self.fallback.clone())
        }

        fn live(&self, key: u8, velocity: u8, _: &str) {
            self.keys.lock().unwrap().push((key, velocity));
        }
    }

    #[test]
    fn a_midi_output_plays_without_a_soundfont() {
        let data = temp("player-midi");
        // The fetcher would have been asked for the SoundFont, and it waits to be let go.
        let gate = Arc::new(Gate {
            released: Arc::new(AtomicBool::new(false)),
            calls: Mutex::new(0),
        });
        let midi = Midi::new(Opened::new(), false);
        let mut player = Player::new(timeline(), data.clone(), font(), gate.clone(), midi.clone());
        assert_eq!(player.output_label().unwrap(), "MIDI output “Test piano”");
        player.play();
        until("the output", || player.sound() == Sound::Ready);
        assert!(player.clock.playing());
        assert_eq!(*gate.calls.lock().unwrap(), 0);
        assert_eq!(player.notice(), None);

        // The player going closes the output: its keys are let go (see `midi_out`).
        let (_, closed) = midi.opened.lock().unwrap().clone().unwrap();
        assert!(!closed.load(Relaxed));
        drop(player);
        until("the output to close", || closed.load(Relaxed));
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_midi_output_that_is_not_there_gives_way_to_the_built_in_piano_and_says_so() {
        let data = temp("player-midi-absent");
        let piano = Opened::new();
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            Midi::new(piano.clone(), true),
        );
        player.play();
        until("the built-in piano", || player.sound() == Sound::Ready);
        assert!(
            player.clock.playing(),
            "the piece plays, on the other output"
        );
        assert_eq!(piano.paths.lock().unwrap().len(), 1);
        assert_eq!(player.output_label(), None);
        let notice = player.notice().unwrap();
        assert!(
            notice.starts_with("MIDI output “Test piano” cannot be used (it is not connected)")
        );
        assert!(
            notice.ends_with("The built-in piano plays instead."),
            "{notice}"
        );
        assert_eq!(player.notice(), None, "said once");
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_warm_player_whose_midi_output_is_not_there_says_so_and_the_piano_answers() {
        let data = temp("player-warm-midi-absent");
        let piano = Opened::new();
        let player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            Midi::new(piano.clone(), true),
        );
        // What the first key struck does.
        player.warm();
        until("the built-in piano", || player.sound() == Sound::Ready);
        assert!(!player.clock.playing());
        let notice = player.notice().unwrap();
        assert!(
            notice.starts_with("MIDI output “Test piano” cannot be used (it is not connected)"),
            "{notice}"
        );
        player.keys().note(60, 90, "40:0");
        assert_eq!(*piano.keys.lock().unwrap(), [(60, 90)]);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_player_that_shuts_down_has_closed_its_output_when_it_returns() {
        let data = temp("player-shutdown");
        let midi = Midi::new(Opened::new(), false);
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            midi.clone(),
        );
        player.play();
        until("the output", || player.sound() == Sound::Ready);
        let (_, closed) = midi.opened.lock().unwrap().clone().unwrap();
        player.shutdown(Duration::from_secs(5));
        assert!(closed.load(Relaxed), "closed, not closing");
        // Going on after it is not a second close.
        drop(player);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_fallback_that_cannot_open_is_not_said_to_play() {
        let data = temp("player-fallback-fails");
        // The built-in piano is asked for its SoundFont, and the fetcher is not there.
        let released = Arc::new(AtomicBool::new(true));
        let gate = Arc::new(Gate {
            released,
            calls: Mutex::new(0),
        });
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            gate,
            Midi::new(Opened::new(), true),
        );
        player.play();
        until("the failure", || matches!(player.sound(), Sound::Failed(_)));
        let notice = player.notice().unwrap();
        assert!(notice.starts_with("MIDI output “Test piano” cannot be used"));
        assert!(!notice.contains("plays instead"), "{notice}");
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn a_lost_midi_output_gives_way_to_the_built_in_piano_where_the_piece_stood() {
        let data = temp("player-midi-lost");
        let piano = Opened::new();
        let midi = Midi::new(piano.clone(), false);
        let mut player = Player::new(
            timeline(),
            data.clone(),
            font(),
            Arc::new(Serves(Arc::new(AtomicBool::new(true)))),
            midi.clone(),
        );
        player.play();
        until("the output", || player.sound() == Sound::Ready);
        player.clock.seek_ms(1_500.);
        let (lost, closed) = midi.opened.lock().unwrap().clone().unwrap();
        assert!(piano.paths.lock().unwrap().is_empty());

        // What the MIDI thread does when the port is gone.
        lost.store(true, Relaxed);
        assert_eq!(player.sound(), Sound::Preparing);
        until("the built-in piano", || player.sound() == Sound::Ready);
        assert!(player.clock.playing(), "it goes on");
        assert_eq!(player.clock.snapshot().position_us, 1_500_000);
        assert_eq!(piano.paths.lock().unwrap().len(), 1);
        until("the lost output to close", || closed.load(Relaxed));
        assert_eq!(player.output_label(), None);
        let notice = player.notice().unwrap();
        assert!(
            notice.starts_with("MIDI output “Test piano” was lost."),
            "{notice}"
        );
        assert_eq!(player.notice(), None);

        // The lost output goes on failing as it closes (its thread still looks at its port):
        // that is its own flag, and says nothing of the piano that took over.
        lost.store(true, Relaxed);
        assert_eq!(player.sound(), Sound::Ready);
        assert!(player.clock.playing());
        assert_eq!(player.notice(), None);

        // The built-in piano is not replaced by anything: its loss is a failure, as before.
        piano
            .lost
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store(true, Relaxed);
        assert!(matches!(player.sound(), Sound::Failed(_)));
        let _ = std::fs::remove_dir_all(data);
    }
}
