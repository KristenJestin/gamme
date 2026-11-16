//! The engraver: the one Verovio toolkit of the process, on a thread of its own, with its
//! resources unpacked once into the cache folder.
//!
//! `verovio::Toolkit::new` unpacks the resources into a new temporary folder it never deletes,
//! once per process (12 MB a launch), and offers no other place: the toolkit is made here from
//! the bridge the `verovio` crate is built on, pointed at `$XDG_CACHE_HOME/gamme/verovio-<version>`.
//! The toolkit is not `Send`: it is made and used on the engraver's thread only, and a piece is
//! asked of it through a channel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, mpsc};

use base64::Engine as _;
use resvg::usvg::roxmltree;
use serde::Deserialize;
use verovio_sys::ffi;

use crate::timeline::Written;

/// Verovio's layout: pages 2200 units wide drawn at half size, systems broken to fit, each page
/// as tall as what it holds, and no footer of Verovio's own.
const LAYOUT: &str = r#"{"pageWidth": 2200, "adjustPageHeight": true, "scale": 50, "breaks": "auto", "footer": "none"}"#;

/// Written last in a resource folder: a folder without it was never finished.
const UNPACKED: &str = ".unpacked";

pub type Result<T> = std::result::Result<T, String>;

/// A piece as Verovio engraved it: its pages, and when each of its notes and measures sounds.
pub struct Engraved {
    /// Each page as SVG, in order.
    pub pages: Vec<String>,
    /// The timemap, its ids those of the engraved elements: a repeat played through names the
    /// notes and measures it plays again by their copies' ids, mapped back here.
    pub events: Vec<Event>,
    /// Each measure's number as the score prints it (its `@n`), by the measure's id.
    pub numbers: HashMap<String, String>,
    /// The piece as Verovio plays it, a Standard MIDI File: one track per staff (track n for
    /// staff n), ties joined, repeats played through.
    pub midi: Vec<u8>,
    /// What each engraved note is written as, by its id: what the timeline finds the notes of
    /// the MIDI by (`timeline.rs`). Read from Verovio's MEI of the piece: its SVG would say it
    /// with `svgAdditionalAttribute`, but Verovio's names of elements are kept per thread, and
    /// only the main thread's are filled.
    pub written: HashMap<String, Written>,
}

/// A moment of the timemap.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Event {
    /// Milliseconds from the start, at the piece's own tempo.
    pub tstamp: f64,
    /// The notes that start here.
    #[serde(default)]
    pub on: Vec<String>,
    /// The notes that stop here.
    #[serde(default)]
    pub off: Vec<String>,
    /// The measure that starts here.
    #[serde(rename = "measureOn")]
    pub measure_on: Option<String>,
}

struct Job {
    piece: PathBuf,
    reply: mpsc::Sender<Result<Engraved>>,
}

/// The way to the engraver's thread.
pub struct Engraver {
    jobs: Mutex<mpsc::Sender<Job>>,
}

impl Engraver {
    /// Starts the engraver's thread. Its toolkit is made at the first piece, with its resources
    /// in `cache` (unpacked there the first time): no piece, no toolkit and nothing written.
    pub fn start(cache: Option<PathBuf>) -> Self {
        let (jobs, received) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("engraver".into())
            .spawn(move || {
                let mut toolkit: Option<Toolkit> = None;
                for job in received {
                    let engraved = match &mut toolkit {
                        Some(toolkit) => toolkit.engrave(&job.piece),
                        None => match Toolkit::new(cache.as_deref()) {
                            Ok(made) => toolkit.insert(made).engrave(&job.piece),
                            // Not kept: the next piece tries again (a cache that was full, say).
                            Err(error) => Err(error),
                        },
                    };
                    let _ = job.reply.send(engraved);
                }
            })
            .expect("the engraver's thread starts");
        Self {
            jobs: Mutex::new(jobs),
        }
    }

    /// Engraves `piece` (MusicXML, compressed or not, or MEI). Blocks until it is done: call it
    /// off the main thread.
    pub fn engrave(&self, piece: &Path) -> Result<Engraved> {
        let (reply, answer) = mpsc::channel();
        let job = Job {
            piece: piece.to_owned(),
            reply,
        };
        self.jobs
            .lock()
            .map_err(|_| "the engraver stopped")?
            .send(job)
            .map_err(|_| "the engraver stopped")?;
        answer.recv().map_err(|_| "the engraver stopped")?
    }
}

/// The engraver of the process, started at its first use.
pub fn shared() -> &'static Engraver {
    static SHARED: OnceLock<Engraver> = OnceLock::new();
    SHARED.get_or_init(|| Engraver::start(crate::paths::cache()))
}

struct Toolkit(cxx::UniquePtr<ffi::Toolkit>);

impl Toolkit {
    fn new(cache: Option<&Path>) -> Result<Self> {
        let cache = cache.ok_or("no cache folder: HOME is not set")?;
        // Without its fonts: they are read from the resources, once they are in place.
        let mut inner = ffi::new_toolkit(false);
        if inner.is_null() {
            return Err("Verovio did not start".into());
        }
        let resources = unpack(cache, &ffi::get_version(&inner))
            .map_err(|error| format!("cannot unpack Verovio's resources: {error}"))?;
        let path = resources
            .to_str()
            .ok_or("the cache folder's path is not UTF-8")?;
        if !ffi::set_resource_path(inner.pin_mut(), path) {
            return Err(format!("Verovio cannot read its resources in {path}"));
        }
        if !ffi::set_options(inner.pin_mut(), LAYOUT) {
            return Err("Verovio refused its layout".into());
        }
        Ok(Self(inner))
    }

    fn engrave(&mut self, piece: &Path) -> Result<Engraved> {
        let file = piece.to_str().ok_or("the piece's path is not UTF-8")?;
        let tk = &mut self.0;
        if !ffi::load_file(tk.pin_mut(), file) {
            return Err(format!("Verovio cannot read {}", piece.display()));
        }
        // Verovio aborts the process when it is asked to draw a piece with no page.
        let count = ffi::page_count(tk.pin_mut());
        if count <= 0 {
            return Err(format!("{} has nothing to engrave", piece.display()));
        }
        let pages = (1..=count)
            .map(|page| ffi::render_to_svg(tk.pin_mut(), page, false))
            .collect();
        let timemap = ffi::render_to_timemap(tk.pin_mut(), r#"{"includeMeasures": true}"#);
        let mut events: Vec<Event> = serde_json::from_str(&timemap)
            .map_err(|error| format!("Verovio's timemap cannot be read: {error}"))?;
        let mut notated: HashMap<String, String> = HashMap::new();
        let mut engraved = |id: &mut String| {
            let original = notated
                .entry(id.clone())
                .or_insert_with(|| ffi::get_notated_id_for_element(tk.pin_mut(), id));
            if !original.is_empty() {
                id.clone_from(original);
            }
        };
        for event in &mut events {
            event.on.iter_mut().for_each(&mut engraved);
            event.off.iter_mut().for_each(&mut engraved);
            event.measure_on.iter_mut().for_each(&mut engraved);
        }
        let mut numbers = HashMap::new();
        for id in events.iter().filter_map(|event| event.measure_on.as_ref()) {
            if numbers.contains_key(id) {
                continue;
            }
            let attributes = ffi::get_element_attr(tk.pin_mut(), id);
            let number = serde_json::from_str::<HashMap<String, serde_json::Value>>(&attributes)
                .ok()
                .and_then(|mut attributes| match attributes.remove("n")? {
                    serde_json::Value::String(n) => Some(n),
                    other => Some(other.to_string()),
                });
            if let Some(number) = number {
                numbers.insert(id.clone(), number);
            }
        }
        let midi = base64::engine::general_purpose::STANDARD
            .decode(ffi::render_to_midi(tk.pin_mut()).trim())
            .map_err(|error| format!("Verovio's MIDI cannot be read: {error}"))?;
        let written = written(&ffi::get_mei(tk.pin_mut(), "{}"))?;
        Ok(Engraved {
            pages,
            events,
            numbers,
            midi,
            written,
        })
    }
}

/// Each note of an MEI document, by its id: the `@n` of its staff, and its `@pname` and `@oct`.
fn written(mei: &str) -> Result<HashMap<String, Written>> {
    const XML: &str = "http://www.w3.org/XML/1998/namespace";
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    let document = roxmltree::Document::parse_with_options(mei, options)
        .map_err(|error| format!("Verovio's MEI cannot be read: {error}"))?;
    let mut written = HashMap::new();
    for note in document
        .descendants()
        .filter(|node| node.tag_name().name() == "note")
    {
        let Some(id) = note.attribute((XML, "id")) else {
            continue;
        };
        let staff = note
            .ancestors()
            .find(|above| above.tag_name().name() == "staff")
            .and_then(|staff| staff.attribute("n")?.parse().ok());
        let Some(staff) = staff else {
            continue;
        };
        let key = note
            .attribute("pname")
            .zip(note.attribute("oct"))
            .and_then(|(pname, oct)| Written::key(pname, oct));
        written.insert(id.to_owned(), Written { staff, key });
    }
    Ok(written)
}

/// The folder of Verovio's resources for `version`, unpacked into `cache` the first time and
/// found there after. Unpacked beside it first, then renamed: another Gamme starting at the
/// same time never reads half a folder.
fn unpack(cache: &Path, version: &str) -> std::io::Result<PathBuf> {
    let name: String = version
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' => c,
            _ => '_',
        })
        .collect();
    let folder = cache.join(format!("verovio-{name}"));
    if folder.join(UNPACKED).is_file() {
        return Ok(folder);
    }
    std::fs::create_dir_all(cache)?;
    let work = cache.join(format!(".verovio-{name}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir(&work)?;
    let unpacked = verovio_data::extract(&work)
        .and_then(|()| std::fs::write(work.join(UNPACKED), version))
        .and_then(|()| {
            // A folder left half-made by a run that stopped is made again.
            if folder.exists() && !folder.join(UNPACKED).is_file() {
                std::fs::remove_dir_all(&folder)?;
            }
            std::fs::rename(&work, &folder)
        });
    match unpacked {
        Ok(()) => Ok(folder),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&work);
            // Another Gamme unpacked it first.
            if folder.join(UNPACKED).is_file() {
                return Ok(folder);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resources_are_unpacked_once_into_the_cache() {
        let cache = tempfile::tempdir().unwrap();
        let cache = cache.path().join("gamme");
        let folder = unpack(&cache, "5.0.0-dev+abc").unwrap();
        assert_eq!(folder, cache.join("verovio-5.0.0-dev_abc"));
        assert!(folder.join("Bravura.xml").is_file(), "the default font");

        // Found, not unpacked again: a file taken away stays away.
        std::fs::remove_file(folder.join("Bravura.xml")).unwrap();
        assert_eq!(unpack(&cache, "5.0.0-dev+abc").unwrap(), folder);
        assert!(!folder.join("Bravura.xml").exists());

        // Nothing else is left in the cache.
        let entries: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["verovio-5.0.0-dev_abc"]);
    }

    #[test]
    fn the_notes_of_an_mei_say_their_staff_and_written_pitch() {
        let mei = r#"<mei xmlns="http://www.music-encoding.org/ns/mei"><music><body><mdiv><score><section>
            <measure xml:id="m1"><staff n="1"><layer n="1">
              <chord><note xml:id="n1" pname="e" oct="5"/><note xml:id="n2" pname="g" oct="5"><accid accid="s"/></note></chord>
            </layer></staff><staff n="2"><layer n="1"><note xml:id="n3" pname="c" oct="3"/><note pname="d" oct="3"/></layer></staff></measure>
            </section></score></mdiv></body></music></mei>"#;
        let found = written(mei).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(
            found["n1"],
            Written {
                staff: 1,
                key: Some(76)
            }
        );
        assert_eq!(
            found["n2"].key,
            Some(79),
            "the written pitch, without its sharp"
        );
        assert_eq!(
            found["n3"],
            Written {
                staff: 2,
                key: Some(48)
            }
        );
        assert!(written("<mei").is_err());
    }

    #[test]
    fn a_folder_left_half_made_is_made_again() {
        let cache = tempfile::tempdir().unwrap();
        let half = cache.path().join("verovio-1.0");
        std::fs::create_dir_all(&half).unwrap();
        std::fs::write(half.join("stray"), "").unwrap();
        let folder = unpack(cache.path(), "1.0").unwrap();
        assert!(folder.join("Bravura.xml").is_file());
        assert!(!folder.join("stray").exists());
    }
}
