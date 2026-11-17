//! The progress of each piece (design §5.4), kept locally, never sent anywhere: the stars it has
//! earned, its best score at each speed, and the history of its runs.
//!
//! - **Where**: `$XDG_STATE_HOME/gamme/progress/<id>.json`, one file per piece, named by the
//!   piece's id in the library (`library.rs`): a file that cannot be read costs one piece's
//!   progress, not all. The folders are `0700` and the files `0600` (`config::write_private`),
//!   each written beside and renamed over the old one. A removed piece keeps its file: the same
//!   score imported again is the same piece (same id), and gets its stars back.
//! - **What**: `{"version": 1, "stars": 2, "best": {"50": 2300, "75": 4100}, "history": [...]}`.
//!   - `stars` is the best a run ever earned (`stars.rs`) and is never lowered: a worse run does
//!     not take a star away.
//!   - `best` is the highest score at each speed (the run's slowest, in percent), of the runs that
//!     stand for the piece: the whole piece, with the wait off. A score from a part of the piece,
//!     or one the wait made free, would not compare with another.
//!   - `history` is every run that ended, oldest first, the last [`HISTORY`] of them: about
//!     150 bytes a run, so 30 KB at most per piece.
//! - **A change** reads the file, applies the run and writes it again, one at a time in the
//!   process ([`Store::record`]), as the library's changes do (`library.rs`).
//! - **What cannot be read** never loses the piece nor stops a run: a file that is not JSON, or
//!   whose parts are not what they should be, shows what could be read and nothing more. Before
//!   the next change replaces it, its content is kept beside it as
//!   `<id>.json.unreadable-<seconds>`. Keys this version does not know are written back as they
//!   were. A file of a newer format (`version` above [`FORMAT`]) is read the same way for what is
//!   shown, but never written: another version's meanings are not ours to change.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::run_end::Summary;
use crate::{config, library, paths, stars};

/// The format this version writes.
pub const FORMAT: u64 = 1;

/// How many runs a piece keeps: the latest ones.
pub const HISTORY: usize = 200;

/// One run that ended, as it is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// When it ended, in seconds since the epoch.
    pub at: u64,
    /// The slowest speed it was played at, in percent.
    pub speed: u32,
    pub score: u64,
    /// The share of its timed notes perfect or good, in percent; none when nothing was timed.
    #[serde(default)]
    pub accuracy: Option<u32>,
    #[serde(default)]
    pub best_combo: u32,
    /// The stars it earned.
    pub stars: u8,
    /// Whether a note was judged under the wait.
    #[serde(default)]
    pub waited: bool,
    /// Whether the falling notes were shown while it played.
    #[serde(default)]
    pub aided: bool,
    /// Whether it went over the whole piece.
    #[serde(default)]
    pub whole: bool,
}

impl Run {
    /// The run `summary` is the end of, ended at `at` (seconds since the epoch).
    pub fn of(summary: &Summary, at: u64) -> Self {
        Self {
            at,
            speed: summary.feat.speed,
            score: summary.standing.score,
            accuracy: summary.feat.accuracy,
            best_combo: summary.standing.best_combo,
            stars: summary.stars,
            waited: summary.feat.waited,
            aided: summary.feat.aided,
            whole: summary.feat.whole,
        }
    }

    /// Whether its score is a candidate for the best of its speed (see the module).
    fn counts_for_best(&self) -> bool {
        self.whole && !self.waited && self.score > 0 && self.speed > 0
    }
}

/// What a run did to the progress it was added to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// It earned more stars than any run before: the piece's stars are now its.
    pub new_stars: bool,
    /// Its score beat the best kept at its speed (the first score at a speed beats nothing).
    pub new_best: bool,
}

/// What a file held that this version leaves as it was, or could not read.
#[derive(Clone, Debug, Default, PartialEq)]
struct Held {
    /// The format the file says; none when it says nothing, or something unreadable.
    version: Option<u64>,
    /// The keys this version does not know.
    extra: Map<String, Value>,
    /// Whether something was left out for want of being readable.
    lossy: bool,
}

/// One piece's progress.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    /// The most stars a run earned, 0 to [`stars::MOST`].
    pub stars: u8,
    /// The best score at each speed, in percent.
    pub best: BTreeMap<u32, u64>,
    /// The runs, oldest first.
    pub history: Vec<Run>,
    held: Held,
}

/// What the home screen shows of a piece's progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Glance {
    pub stars: u8,
    /// The best score at the fastest speed that has one: `(speed in percent, score)`.
    pub top: Option<(u32, u64)>,
    /// Whether the piece was ever played to the end of a run.
    pub played: bool,
}

impl Progress {
    /// What a file says, as far as it can be read (see the module).
    pub fn parse(bytes: &[u8]) -> Self {
        let Ok(Value::Object(mut file)) = serde_json::from_slice::<Value>(bytes) else {
            return Self {
                held: Held {
                    lossy: true,
                    ..Held::default()
                },
                ..Self::default()
            };
        };
        let mut lossy = false;
        let version = file.remove("version").and_then(|value| {
            let number = value.as_u64();
            lossy |= number.is_none();
            number
        });
        let stars = file
            .remove("stars")
            .map_or(0, |value| match value.as_u64() {
                Some(number) => number.min(u64::from(stars::MOST)) as u8,
                None => {
                    lossy = true;
                    0
                }
            });
        let mut best = BTreeMap::new();
        match file.remove("best") {
            None => {}
            Some(Value::Object(entries)) => {
                for (speed, score) in entries {
                    match (speed.parse::<u32>(), score.as_u64()) {
                        (Ok(speed), Some(score)) => {
                            best.insert(speed, score);
                        }
                        _ => lossy = true,
                    }
                }
            }
            Some(_) => lossy = true,
        }
        let mut history = Vec::new();
        match file.remove("history") {
            None => {}
            Some(Value::Array(runs)) => {
                for run in runs {
                    match serde_json::from_value::<Run>(run) {
                        Ok(run) => history.push(Run {
                            stars: run.stars.min(stars::MOST),
                            ..run
                        }),
                        Err(_) => lossy = true,
                    }
                }
            }
            Some(_) => lossy = true,
        }
        history.drain(..history.len().saturating_sub(HISTORY));
        Self {
            stars,
            best,
            history,
            held: Held {
                version,
                extra: file,
                lossy,
            },
        }
    }

    /// Whether the file said it is of a format newer than this version's.
    fn newer(&self) -> bool {
        self.held.version.is_some_and(|version| version > FORMAT)
    }

    /// The file as it is kept: what was not read as it was, what is known as it stands now.
    fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        let mut file = self.held.extra.clone();
        file.insert("version".into(), FORMAT.into());
        file.insert("stars".into(), self.stars.into());
        let best = self
            .best
            .iter()
            .map(|(speed, score)| (speed.to_string(), Value::from(*score)))
            .collect();
        file.insert("best".into(), Value::Object(best));
        file.insert("history".into(), serde_json::to_value(&self.history)?);
        serde_json::to_vec_pretty(&Value::Object(file))
    }

    /// Adds a run that ended: the stars never go down, the best score of its speed goes up when
    /// it is beaten, and the history keeps the last [`HISTORY`] runs.
    pub fn add(&mut self, run: Run) -> Outcome {
        let mut outcome = Outcome {
            new_stars: run.stars > self.stars,
            new_best: false,
        };
        self.stars = self.stars.max(run.stars);
        if run.counts_for_best() {
            match self.best.get(&run.speed) {
                None => {
                    self.best.insert(run.speed, run.score);
                }
                Some(&kept) if run.score > kept => {
                    self.best.insert(run.speed, run.score);
                    outcome.new_best = true;
                }
                Some(_) => {}
            }
        }
        self.history.push(run);
        self.history
            .drain(..self.history.len().saturating_sub(HISTORY));
        outcome
    }

    pub fn glance(&self) -> Glance {
        Glance {
            stars: self.stars,
            top: self
                .best
                .iter()
                .next_back()
                .map(|(speed, score)| (*speed, *score)),
            played: !self.history.is_empty(),
        }
    }
}

/// The progress folder; clones are cheap, and nothing is held open.
#[derive(Clone, Debug)]
pub struct Store {
    folder: PathBuf,
}

/// The changes of progress files take turns ([`Store::record`]).
static CHANGES: Mutex<()> = Mutex::new(());

impl Store {
    /// The store of the state folder, `None` when there is no home to keep it in.
    pub fn open() -> Option<Self> {
        Some(Self::at(&paths::state()?))
    }

    /// The store of a state folder: its files are in `progress/` there.
    pub fn at(state: &Path) -> Self {
        Self {
            folder: state.join("progress"),
        }
    }

    fn name(id: &str) -> io::Result<String> {
        if !library::is_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{id:?} is not a piece's id"),
            ));
        }
        Ok(format!("{id}.json"))
    }

    /// A piece's progress: nothing yet when there is no file, and what could be read when there
    /// is one that could not be read whole. Blocking.
    pub fn load(&self, id: &str) -> Progress {
        let Ok(name) = Self::name(id) else {
            return Progress::default();
        };
        match fs::read(self.folder.join(&name)) {
            Ok(bytes) => Progress::parse(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Progress::default(),
            Err(error) => {
                eprintln!("gamme: the progress {name} cannot be read: {error}");
                Progress::default()
            }
        }
    }

    /// The progress of each of `ids`, for the home screen. Blocking.
    pub fn glances<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> HashMap<String, Glance> {
        ids.into_iter()
            .map(|id| (id.to_owned(), self.load(id).glance()))
            .collect()
    }

    /// Adds a run to a piece's progress and keeps it: one change at a time in the process, the
    /// file read again each time. A file that was not read whole is kept aside first, and one of
    /// a newer format is refused (see the module).
    pub fn record(&self, id: &str, run: Run) -> io::Result<Outcome> {
        let name = Self::name(id)?;
        let _turn = CHANGES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.folder.join(&name);
        let (mut progress, original) = match fs::read(&path) {
            Ok(bytes) => (Progress::parse(&bytes), Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Progress::default(), None),
            Err(error) => return Err(error),
        };
        if progress.newer() {
            return Err(io::Error::other(format!(
                "{name} is of a format newer than this version's ({FORMAT}): left as it is"
            )));
        }
        if let Some(bytes) = original.filter(|_| progress.held.lossy) {
            let at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs());
            config::write_private(&self.folder, &format!("{name}.unreadable-{at}"), &bytes)?;
        }
        let outcome = progress.add(run);
        let json = progress.to_json().map_err(io::Error::other)?;
        config::write_private(&self.folder, &name, &json)?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    const ID: &str = "0123456789abcdef01234567";
    const OTHER: &str = "fedcba9876543210fedcba98";

    fn run(speed: u32, score: u64, stars: u8) -> Run {
        Run {
            at: 1_700_000_000,
            speed,
            score,
            accuracy: Some(95),
            best_combo: 12,
            stars,
            waited: false,
            aided: true,
            whole: true,
        }
    }

    /// A state folder of its own in a temporary one, never the real home's.
    fn state() -> (tempfile::TempDir, Store) {
        let home = tempfile::tempdir().unwrap();
        let store = Store::at(&home.path().join("state/gamme"));
        (home, store)
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn file(store: &Store, id: &str) -> PathBuf {
        store.folder.join(format!("{id}.json"))
    }

    #[test]
    fn a_star_earned_is_there_after_a_restart() {
        let (home, store) = state();
        assert_eq!(store.load(ID), Progress::default());
        let outcome = store.record(ID, run(50, 2_300, 1)).unwrap();
        assert_eq!(
            outcome,
            Outcome {
                new_stars: true,
                new_best: false
            }
        );
        // A restart: a store opened again on the same folder.
        let again = Store::at(&home.path().join("state/gamme"));
        let progress = again.load(ID);
        assert_eq!(progress.stars, 1);
        assert_eq!(progress.best.get(&50), Some(&2_300));
        assert_eq!(progress.history, [run(50, 2_300, 1)]);
        assert_eq!(
            progress.glance(),
            Glance {
                stars: 1,
                top: Some((50, 2_300)),
                played: true
            }
        );
        // Another piece has its own.
        assert_eq!(again.load(OTHER), Progress::default());
    }

    #[test]
    fn stars_are_never_lowered_and_the_best_score_is_per_speed() {
        let (_home, store) = state();
        store.record(ID, run(75, 4_000, 2)).unwrap();
        // A worse run: fewer stars, a lower score at the same speed.
        let worse = store.record(ID, run(75, 3_000, 0)).unwrap();
        assert_eq!(worse, Outcome::default());
        // The same stars again are no new best, a higher score is.
        let same = store.record(ID, run(75, 4_500, 2)).unwrap();
        assert_eq!(
            same,
            Outcome {
                new_stars: false,
                new_best: true
            }
        );
        // Another speed has its own best, and a first score beats nothing.
        let slower = store.record(ID, run(50, 100, 1)).unwrap();
        assert_eq!(slower, Outcome::default());
        let progress = store.load(ID);
        assert_eq!(progress.stars, 2);
        assert_eq!(progress.best, BTreeMap::from([(50, 100), (75, 4_500)]));
        assert_eq!(progress.history.len(), 4);
        assert_eq!(
            progress.glance().top,
            Some((75, 4_500)),
            "the fastest speed"
        );
    }

    #[test]
    fn only_a_whole_run_with_the_wait_off_sets_a_best_score() {
        let mut progress = Progress::default();
        let part = Run {
            whole: false,
            ..run(100, 900, 0)
        };
        let waited = Run {
            waited: true,
            ..run(100, 900, 0)
        };
        let nothing = run(100, 0, 0);
        for skipped in [part, waited, nothing] {
            progress.add(skipped);
        }
        assert!(progress.best.is_empty());
        assert_eq!(progress.history.len(), 3, "all of them are in the history");
        assert!(progress.glance().played);
        assert_eq!(progress.glance().top, None);
    }

    #[test]
    fn the_history_keeps_the_last_runs() {
        let (_home, store) = state();
        for n in 0..HISTORY as u64 + 5 {
            store.record(ID, run(100, n + 1, 0)).unwrap();
        }
        let history = store.load(ID).history;
        assert_eq!(history.len(), HISTORY);
        assert_eq!(
            history.first().unwrap().score,
            6,
            "the oldest five are gone"
        );
        assert_eq!(history.last().unwrap().score, HISTORY as u64 + 5);
    }

    #[test]
    fn the_folders_are_0700_and_the_file_0600_and_no_temporary_file_is_left() {
        let (home, store) = state();
        store.record(ID, run(50, 1, 0)).unwrap();
        store.record(ID, run(50, 2, 0)).unwrap();
        assert_eq!(mode(&home.path().join("state/gamme")), 0o700);
        assert_eq!(mode(&store.folder), 0o700);
        assert_eq!(mode(&file(&store, ID)), 0o600);
        let names: Vec<_> = fs::read_dir(&store.folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [format!("{ID}.json").as_str()]);
    }

    #[test]
    fn a_folder_that_was_there_with_another_mode_is_made_0700() {
        let (_home, store) = state();
        fs::create_dir_all(&store.folder).unwrap();
        fs::set_permissions(&store.folder, fs::Permissions::from_mode(0o755)).unwrap();
        store.record(ID, run(50, 1, 0)).unwrap();
        assert_eq!(mode(&store.folder), 0o700);
    }

    #[test]
    fn a_file_that_is_not_progress_is_nothing_and_is_kept_aside_when_replaced() {
        for text in [
            "not json",
            "[1, 2]",
            "\"text\"",
            "",
            "{\"stars\": \"three\"}",
        ] {
            let (_home, store) = state();
            fs::create_dir_all(&store.folder).unwrap();
            fs::write(file(&store, ID), text).unwrap();
            let progress = store.load(ID);
            assert_eq!((progress.stars, progress.history.len()), (0, 0), "{text}");
            store.record(ID, run(50, 700, 1)).unwrap();
            // Replaced, with the old content kept beside it.
            assert_eq!(store.load(ID).stars, 1, "{text}");
            let aside: Vec<_> = fs::read_dir(&store.folder)
                .unwrap()
                .map(|entry| entry.unwrap())
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&format!("{ID}.json.unreadable-"))
                })
                .collect();
            assert_eq!(aside.len(), 1, "{text}");
            assert_eq!(fs::read_to_string(aside[0].path()).unwrap(), text);
            assert_eq!(mode(&aside[0].path()), 0o600);
            // A second run finds a file that reads: nothing more is put aside.
            store.record(ID, run(50, 800, 1)).unwrap();
            assert_eq!(fs::read_dir(&store.folder).unwrap().count(), 2, "{text}");
        }
    }

    #[test]
    fn what_can_be_read_is_read_and_the_rest_is_kept_aside() {
        let (_home, store) = state();
        fs::create_dir_all(&store.folder).unwrap();
        let text = r#"{"version":1,"stars":2,"best":{"75":4100,"fast":9,"50":"x"},
            "history":[{"at":5,"speed":75,"score":4100,"stars":2},{"at":"then"},7]}"#;
        fs::write(file(&store, ID), text).unwrap();
        let progress = store.load(ID);
        assert_eq!(progress.stars, 2);
        assert_eq!(progress.best, BTreeMap::from([(75, 4_100)]));
        assert_eq!(progress.history.len(), 1, "a run that reads is kept");
        assert_eq!(
            progress.history[0].best_combo, 0,
            "a missing key is its default"
        );
        assert!(progress.held.lossy);
        store.record(ID, run(75, 100, 1)).unwrap();
        let after = store.load(ID);
        assert_eq!(after.stars, 2);
        assert_eq!(after.history.len(), 2);
        assert!(!after.held.lossy);
        let kept = fs::read_dir(&store.folder)
            .unwrap()
            .filter(|entry| {
                let name = entry.as_ref().unwrap().file_name();
                name.to_string_lossy().contains(".unreadable-")
            })
            .count();
        assert_eq!(kept, 1);
    }

    #[test]
    fn keys_this_version_does_not_know_are_written_back() {
        let (_home, store) = state();
        fs::create_dir_all(&store.folder).unwrap();
        fs::write(
            file(&store, ID),
            r#"{"version":1,"stars":1,"ladder":{"step":3,"of":[60,75]},"note":"hi"}"#,
        )
        .unwrap();
        store.record(ID, run(50, 10, 1)).unwrap();
        let after: Value = serde_json::from_slice(&fs::read(file(&store, ID)).unwrap()).unwrap();
        assert_eq!(
            after["ladder"],
            serde_json::json!({"step": 3, "of": [60, 75]})
        );
        assert_eq!(after["note"], "hi");
        assert_eq!(after["stars"], 1);
        assert_eq!(after["version"], FORMAT);
        assert_eq!(after["history"].as_array().unwrap().len(), 1);
        // Nothing was lost, so nothing was put aside.
        assert_eq!(fs::read_dir(&store.folder).unwrap().count(), 1);
    }

    #[test]
    fn a_file_of_a_newer_format_is_read_and_never_written() {
        let (_home, store) = state();
        fs::create_dir_all(&store.folder).unwrap();
        let text = r#"{"version":2,"stars":3,"best":{"100":9000},"history":[],"ladder":[1]}"#;
        fs::write(file(&store, ID), text).unwrap();
        let progress = store.load(ID);
        assert!(progress.newer());
        assert_eq!(progress.stars, 3);
        assert_eq!(progress.best.get(&100), Some(&9_000));
        let error = store.record(ID, run(100, 1, 3)).unwrap_err();
        assert!(error.to_string().contains("newer"), "{error}");
        assert_eq!(fs::read_to_string(file(&store, ID)).unwrap(), text);
        assert_eq!(fs::read_dir(&store.folder).unwrap().count(), 1);
    }

    #[test]
    fn an_unreadable_path_is_an_error_and_an_id_that_is_not_one_is_refused() {
        let (_home, store) = state();
        // The file is a folder: it cannot be read, so it is not replaced.
        fs::create_dir_all(file(&store, ID)).unwrap();
        assert!(store.record(ID, run(50, 1, 1)).is_err());
        assert_eq!(store.load(ID), Progress::default());
        for bad in ["../keep", "", "short", "zzzzzzzzzzzzzzzzzzzzzzzz"] {
            assert!(store.record(bad, run(50, 1, 1)).is_err(), "{bad}");
            assert_eq!(store.load(bad), Progress::default(), "{bad}");
        }
    }

    #[test]
    fn the_home_reads_the_glance_of_each_piece() {
        let (_home, store) = state();
        store.record(ID, run(100, 5_000, 3)).unwrap();
        let glances = store.glances([ID, OTHER]);
        assert_eq!(glances[ID].stars, 3);
        assert_eq!(glances[ID].top, Some((100, 5_000)));
        assert_eq!(glances[OTHER], Glance::default());
    }

    #[test]
    fn runs_recorded_together_are_all_kept() {
        let (_home, store) = state();
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let store = store.clone();
                std::thread::spawn(move || store.record(ID, run(100, n + 1, 0)).unwrap())
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(store.load(ID).history.len(), 8);
    }
}
