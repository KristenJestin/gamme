//! The library: the pieces the player imported, kept under `$XDG_DATA_HOME/gamme/library`.
//!
//! One folder per piece, named by the first bytes of the sum of its file, so that the same score
//! imported twice is the same piece: `<id>/score.<ext>` is the file as it came, `<id>/piece.json`
//! its metadata. A piece is put in place whole (written aside, then renamed), so that a crash
//! leaves no half piece. Everything here is blocking work: the window runs it off its main thread.

use std::cmp::Reverse;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::piece_file::{self, Kind};

const METADATA: &str = "piece.json";

/// One piece of the library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Piece {
    /// The folder's name: 24 hex digits of the file's sum.
    pub id: String,
    pub title: String,
    pub composer: Option<String>,
    pub kind: Kind,
    /// The score's file name inside the piece's folder.
    pub file: String,
    /// When it was imported, in seconds since the epoch.
    pub imported_at: u64,
    /// When it was last played: `None` for a piece never played.
    pub last_played: Option<u64>,
}

/// Why a file was not imported, as a sentence the player can read.
#[derive(Debug, PartialEq, Eq)]
pub struct ImportError(String);

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImportError {}

/// The library folder; clones are cheap, and nothing is held open.
#[derive(Clone, Debug)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    /// The library of the data folder, `None` when there is no home to keep it in.
    pub fn open() -> Option<Self> {
        Some(Self::at(crate::paths::library()?))
    }

    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// The pieces, the last played first, then the latest imported. A folder that is not a piece
    /// (a leftover of a crash, a file a player dropped there) is skipped.
    pub fn list(&self) -> Vec<Piece> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut pieces: Vec<Piece> = entries
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|entry| read_piece(&entry.path()))
            .collect();
        pieces.sort_by(|a, b| {
            (Reverse(a.last_played), Reverse(a.imported_at), &a.title).cmp(&(
                Reverse(b.last_played),
                Reverse(b.imported_at),
                &b.title,
            ))
        });
        pieces
    }

    /// Copies a file into the library, with its metadata. The error is the sentence to show.
    pub fn import(&self, source: &Path) -> Result<Piece, ImportError> {
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_string_lossy().into_owned());
        let refuse = |why: String| ImportError(format!("“{name}” {why}."));

        let metadata = fs::metadata(source).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => refuse("does not exist".into()),
            _ => refuse(format!("cannot be read ({error})")),
        })?;
        if metadata.is_dir() {
            return Err(refuse(
                "is a folder: import the score files in it one by one".into(),
            ));
        }
        if metadata.len() > piece_file::MAX_BYTES {
            return Err(refuse("is too large to be a score".into()));
        }
        let bytes =
            fs::read(source).map_err(|error| refuse(format!("cannot be read ({error})")))?;
        let found = piece_file::read(&name, &bytes).map_err(refuse)?;

        let id: String = Sha256::digest(&bytes)
            .iter()
            .take(12)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let folder = self.root.join(&id);
        if let Some(already) = read_piece(&folder) {
            return Err(refuse(format!(
                "is already in the library, as “{}”",
                already.title
            )));
        }
        let title = found.title.unwrap_or_else(|| stem(&name));
        let piece = Piece {
            id,
            title,
            composer: found.composer,
            kind: found.kind,
            file: format!("score.{}", found.extension),
            imported_at: now(),
            last_played: None,
        };
        self.keep(&piece, &bytes)
            .map_err(|error| refuse(format!("could not be kept in the library ({error})")))?;
        Ok(piece)
    }

    /// Puts a piece in place whole: written in a folder of its own that the listing skips, then
    /// renamed.
    fn keep(&self, piece: &Piece, bytes: &[u8]) -> io::Result<()> {
        let staging = self.root.join(format!(".import-{}", piece.id));
        // A leftover of an import that crashed.
        match fs::remove_dir_all(&staging) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        fs::create_dir_all(&staging)?;
        let written = fs::write(staging.join(&piece.file), bytes)
            .and_then(|()| write_metadata(&staging, piece));
        let placed = written.and_then(|()| {
            // A folder with no piece in it (a crash between the two renames) is in the way.
            let folder = self.root.join(&piece.id);
            match fs::remove_dir_all(&folder) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
            fs::rename(&staging, folder)
        });
        if placed.is_err() {
            fs::remove_dir_all(&staging).ok();
        }
        placed
    }

    /// Removes a piece and its file. A piece that is not there is already removed.
    pub fn remove(&self, id: &str) -> io::Result<()> {
        if !is_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{id:?} is not a piece's id"),
            ));
        }
        match fs::remove_dir_all(self.root.join(id)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Says a piece was played, at `at` seconds since the epoch: the home screen lists it first.
    #[allow(dead_code)] // The play screen calls it; the library is complete without waiting for it.
    pub fn mark_played(&self, id: &str, at: u64) -> io::Result<()> {
        if !is_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{id:?} is not a piece's id"),
            ));
        }
        let folder = self.root.join(id);
        let mut piece = read_piece(&folder)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such piece"))?;
        piece.last_played = Some(at);
        write_metadata(&folder, &piece)
    }
}

/// A 24-digit hex id: nothing that could leave the library's folder.
fn is_id(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn read_piece(folder: &Path) -> Option<Piece> {
    let text = fs::read_to_string(folder.join(METADATA)).ok()?;
    match serde_json::from_str::<Piece>(&text) {
        Ok(piece) if folder.file_name()?.to_str()? == piece.id => Some(piece),
        Ok(_) => None,
        Err(error) => {
            eprintln!(
                "gamme: {} is not a piece's metadata: {error}",
                folder.display()
            );
            None
        }
    }
}

/// Written aside and renamed, so that a metadata file is never half there.
fn write_metadata(folder: &Path, piece: &Piece) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(piece).map_err(io::Error::other)?;
    let aside = folder.join(format!("{METADATA}.new"));
    fs::write(&aside, json)?;
    fs::rename(aside, folder.join(METADATA))
}

/// The file's name without its extension: the title of a score that names none.
fn stem(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let stem = stem.replace(['_', '-'], " ");
    let stem = stem.split_whitespace().collect::<Vec<_>>().join(" ");
    if stem.is_empty() {
        name.to_owned()
    } else {
        stem
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (tempfile::TempDir, Library) {
        let home = tempfile::tempdir().unwrap();
        let library = Library::at(home.path().join("data/gamme/library"));
        (home, library)
    }

    /// The fixtures are written by hand and free to use: `tests/fixtures/`.
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn nothing_is_listed_before_a_piece_is_imported() {
        let (_home, library) = library();
        assert_eq!(library.list(), Vec::new());
    }

    #[test]
    fn one_file_of_each_kind_is_imported_and_listed_again_by_a_new_library() {
        let (home, library) = library();
        let wanted = [
            (
                "little-song.musicxml",
                Kind::MusicXml,
                "Little Song",
                Some("A. Writer"),
            ),
            (
                "little-song.mxl",
                Kind::MusicXml,
                "Little Song",
                Some("A. Writer"),
            ),
            (
                "short-dance.mei",
                Kind::Mei,
                "Short Dance",
                Some("C. Maker"),
            ),
            ("a-tune.mid", Kind::Midi, "A tune", None),
        ];
        // The .mxl is the .musicxml compressed, so it is the same piece but another file.
        for (file, kind, title, composer) in wanted {
            let piece = library
                .import(&fixture(file))
                .unwrap_or_else(|error| panic!("{file}: {error}"));
            assert_eq!(piece.kind, kind, "{file}");
            assert_eq!(piece.title, title, "{file}");
            assert_eq!(piece.composer.as_deref(), composer, "{file}");
            assert_eq!(piece.last_played, None);
            assert!(library.root.join(&piece.id).join(&piece.file).is_file());
        }
        // A restart: a library opened again on the same folder lists them all.
        let again = Library::at(home.path().join("data/gamme/library")).list();
        assert_eq!(again.len(), 4);
        let mut kinds: Vec<_> = again.iter().map(|p| p.kind.label()).collect();
        kinds.sort_unstable();
        assert_eq!(kinds, ["MEI", "MIDI", "MusicXML", "MusicXML"]);
    }

    #[test]
    fn the_file_is_copied_as_it_came() {
        let (_home, library) = library();
        let piece = library.import(&fixture("a-tune.mid")).unwrap();
        let kept = fs::read(library.root.join(&piece.id).join(&piece.file)).unwrap();
        assert_eq!(kept, fs::read(fixture("a-tune.mid")).unwrap());
        assert_eq!(piece.file, "score.mid");
    }

    #[test]
    fn a_broken_file_is_refused_with_a_sentence_that_says_why() {
        let (_home, library) = library();
        let error = library.import(&fixture("broken.mid")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "“broken.mid” is not a MIDI file: it does not start with a MIDI header."
        );
        let error = library.import(&fixture("broken.musicxml")).unwrap_err();
        let sentence = error.to_string();
        assert!(
            sentence.starts_with("“broken.musicxml” is not well-formed XML (")
                && sentence.ends_with(")."),
            "{sentence}"
        );
        assert_eq!(library.list(), Vec::new());
        // Nothing was left behind in the folder either.
        let left: Vec<_> = fs::read_dir(&library.root)
            .map(|entries| entries.filter_map(Result::ok).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn a_file_that_is_missing_or_a_folder_is_refused() {
        let (home, library) = library();
        let error = library.import(&home.path().join("gone.mid")).unwrap_err();
        assert_eq!(error.to_string(), "“gone.mid” does not exist.");
        let error = library.import(home.path()).unwrap_err();
        assert!(error.to_string().contains("is a folder"), "{error}");
    }

    #[test]
    fn the_same_file_twice_is_refused_by_naming_the_piece() {
        let (_home, library) = library();
        library.import(&fixture("short-dance.mei")).unwrap();
        let error = library.import(&fixture("short-dance.mei")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "“short-dance.mei” is already in the library, as “Short Dance”."
        );
        assert_eq!(library.list().len(), 1);
    }

    #[test]
    fn a_piece_with_no_title_takes_its_file_name() {
        let (home, library) = library();
        let untitled = home.path().join("moonlit_walk-2.mid");
        // One note, no track name: the header, then a track of 12 bytes.
        let mut bytes = b"MThd\0\0\0\x06\0\0\0\x01\x01\xE0MTrk\0\0\0\x0C".to_vec();
        bytes.extend([0, 0x90, 60, 64, 0x60, 0x80, 60, 0, 0, 0xFF, 0x2F, 0]);
        fs::write(&untitled, bytes).unwrap();
        let piece = library.import(&untitled).unwrap();
        assert_eq!(piece.title, "moonlit walk 2");
    }

    #[test]
    fn the_last_played_comes_first_then_the_latest_imported() {
        let (_home, library) = library();
        let song = library.import(&fixture("little-song.musicxml")).unwrap();
        let dance = library.import(&fixture("short-dance.mei")).unwrap();
        let tune = library.import(&fixture("a-tune.mid")).unwrap();
        library.mark_played(&dance.id, 2_000).unwrap();
        library.mark_played(&song.id, 1_000).unwrap();
        let order: Vec<_> = library.list().into_iter().map(|p| p.id).collect();
        assert_eq!(order, [dance.id.clone(), song.id.clone(), tune.id.clone()]);
        library.mark_played(&tune.id, 3_000).unwrap();
        let first = library.list().remove(0);
        assert_eq!((first.id, first.last_played), (tune.id, Some(3_000)));
    }

    #[test]
    fn a_piece_is_removed_with_its_file_and_the_rest_stays() {
        let (_home, library) = library();
        let dance = library.import(&fixture("short-dance.mei")).unwrap();
        let tune = library.import(&fixture("a-tune.mid")).unwrap();
        library.remove(&dance.id).unwrap();
        assert!(!library.root.join(&dance.id).exists());
        let left: Vec<_> = library.list().into_iter().map(|p| p.id).collect();
        assert_eq!(left, [tune.id]);
        // Removed twice is still removed.
        library.remove(&dance.id).unwrap();
        // It can be imported again afterwards.
        library.import(&fixture("short-dance.mei")).unwrap();
    }

    #[test]
    fn an_id_that_leaves_the_folder_is_not_removed() {
        let (home, library) = library();
        let outside = home.path().join("data/gamme/keep");
        fs::create_dir_all(&outside).unwrap();
        assert!(library.remove("../keep").is_err());
        assert!(library.mark_played("../keep", 1).is_err());
        assert!(outside.exists());
    }

    #[test]
    fn a_folder_with_a_bad_or_misplaced_metadata_is_not_a_piece() {
        let (_home, library) = library();
        let piece = library.import(&fixture("a-tune.mid")).unwrap();
        let stray = library.root.join("0123456789abcdef01234567");
        fs::create_dir_all(&stray).unwrap();
        fs::write(stray.join(METADATA), "not json").unwrap();
        fs::create_dir_all(library.root.join(".import-leftover")).unwrap();
        fs::write(library.root.join("notes.txt"), "x").unwrap();
        // A metadata copied into another piece's folder says another id.
        let copied = library.root.join("fedcba9876543210fedcba98");
        fs::create_dir_all(&copied).unwrap();
        fs::copy(
            library.root.join(&piece.id).join(METADATA),
            copied.join(METADATA),
        )
        .unwrap();
        assert_eq!(library.list(), [piece]);
    }
}
