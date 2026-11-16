//! A piece being played: its timeline, its clock, and the built-in piano that sounds it.
//!
//! The window holds a [`Player`] and calls it: `play`, `pause`, `seek_measure`, `set_speed`. The
//! first `play` has the piano made ready on a thread of its own (the SoundFont fetched if it is
//! not there yet, loaded, the audio output opened) and the clock starts when it is; what the
//! window shows meanwhile is [`Player::sound`] and [`Player::progress`]. The thread keeps the
//! output (a `cpal` stream is not `Send`) until the player goes.

use std::any::Any;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use rustysynth::SoundFont as RustySoundFont;

use crate::clock::{Clock, Speed};
use crate::piano::{self, Hands};
use crate::soundfont::{self, Fetcher, Font, Progress};
use crate::timeline::Timeline;

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
}

/// The built-in piano: `rustysynth` on the default output.
pub struct Builtin;

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
        let output = piano::start(&Arc::new(font), timeline, clock, hands, lost)?;
        Ok(Box::new(output))
    }
}

struct Shared {
    sound: Mutex<Sound>,
    /// The player pressed play and the clock has not started yet.
    play_when_ready: AtomicBool,
    /// Set by the output's error callback (an atomic: nothing else is allowed there).
    device_lost: Arc<AtomicBool>,
    progress: Progress,
}

pub struct Player {
    pub clock: Clock,
    pub timeline: Arc<Timeline>,
    pub hands: Hands,
    data: PathBuf,
    font: Font,
    fetcher: Arc<dyn Fetcher>,
    audio: Arc<dyn Audio>,
    shared: Arc<Shared>,
    /// Dropping it ends the thread that holds the output.
    stop: Option<Sender<()>>,
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
        Self {
            clock: Clock::new(timeline.duration_ms()),
            timeline: Arc::new(timeline),
            hands: Hands::both(),
            data,
            font,
            fetcher,
            audio,
            shared: Arc::new(Shared {
                sound: Mutex::new(Sound::Off),
                play_when_ready: AtomicBool::new(false),
                device_lost: Arc::new(AtomicBool::new(false)),
                progress: Progress::default(),
            }),
            stop: None,
        }
    }

    pub fn sound(&self) -> Sound {
        let mut sound = self.shared.sound.lock().expect("the sound's lock");
        if *sound == Sound::Ready && self.shared.device_lost.load(Relaxed) {
            // The piece would run on in silence: stop it and say why.
            *sound = Sound::Failed("the audio output was lost".into());
            self.shared.play_when_ready.store(false, Relaxed);
            self.clock.pause();
        }
        sound.clone()
    }

    pub fn progress(&self) -> &Progress {
        &self.shared.progress
    }

    pub fn font(&self) -> &Font {
        &self.font
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

    /// Starts the thread that makes the piano ready; the state is `Preparing` already.
    fn prepare(&mut self) {
        // A new job: what the last one met does not apply to it.
        self.shared.progress.reset();
        self.shared.device_lost.store(false, Relaxed);
        let (stop, stopped) = mpsc::channel::<()>();
        self.stop = Some(stop);
        let job = Job {
            shared: self.shared.clone(),
            data: self.data.clone(),
            font: self.font,
            fetcher: self.fetcher.clone(),
            audio: self.audio.clone(),
            timeline: self.timeline.clone(),
            clock: self.clock.clone(),
            hands: self.hands.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("gamme-audio".into())
            .spawn(move || {
                let output = job.run();
                // The output lives until the player goes (the channel closes).
                let _ = stopped.recv();
                drop(output);
            });
        if let Err(error) = spawned {
            self.shared
                .fail(format!("cannot start the audio thread: {error}"));
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // A window closed during the download does not leave curl running; the part is kept.
        self.shared.progress.cancel();
    }
}

struct Job {
    shared: Arc<Shared>,
    data: PathBuf,
    font: Font,
    fetcher: Arc<dyn Fetcher>,
    audio: Arc<dyn Audio>,
    timeline: Arc<Timeline>,
    clock: Clock,
    hands: Hands,
}

impl Job {
    fn run(self) -> Option<Box<dyn Any>> {
        let opened = soundfont::install(
            &self.font,
            &self.data,
            &*self.fetcher,
            &self.shared.progress,
        )
        .map_err(|e| e.to_string())
        .and_then(|path| {
            self.audio.open(
                &path,
                &self.timeline,
                &self.clock,
                &self.hands,
                &self.shared.device_lost,
            )
        });
        match opened {
            Ok(output) => {
                // Ready and the start of a piece asked for meanwhile, in one go.
                let mut sound = self.shared.sound.lock().expect("the sound's lock");
                *sound = Sound::Ready;
                if self.shared.play_when_ready.swap(false, Relaxed) {
                    self.clock.play();
                }
                Some(output)
            }
            Err(message) => {
                self.shared.fail(message);
                None
            }
        }
    }
}

impl Shared {
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
    }

    impl Opened {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                paths: Mutex::new(Vec::new()),
                lost: Mutex::new(None),
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
}
