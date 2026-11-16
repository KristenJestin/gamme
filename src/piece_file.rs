//! Reading what the library imports: MusicXML (`.musicxml`, or `.mxl` compressed) and MEI, which
//! are engraved, and MIDI, which has falling notes only. A file is read for its kind, its title and
//! its composer, and checked to hold notes: what is not a score is refused with a reason that
//! reads after the file's name ("is not a MIDI file: ..."), which the library turns into the
//! sentence the player sees.

use std::io::{Cursor, Read as _};

use midly::{MetaMessage, MidiMessage, Smf, TrackEventKind};
use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};

/// What a piece is made of, which says what Gamme can show of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// MusicXML, plain or compressed: engraved, and falling notes.
    MusicXml,
    /// MEI: engraved, and falling notes.
    Mei,
    /// MIDI: falling notes only, no score.
    Midi,
}

impl Kind {
    /// How the player reads it: the tag of a row.
    pub fn label(self) -> &'static str {
        match self {
            Kind::MusicXml => "MusicXML",
            Kind::Mei => "MEI",
            Kind::Midi => "MIDI",
        }
    }

    /// Whether the piece has a score to engrave (MIDI has not).
    pub fn engraved(self) -> bool {
        !matches!(self, Kind::Midi)
    }
}

/// What a file turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub kind: Kind,
    pub title: Option<String>,
    pub composer: Option<String>,
    /// The extension the library keeps the file under: its content decides it, not the name it
    /// came with.
    pub extension: &'static str,
}

/// The most a score may weigh, plain or inflated: an archive that unpacks to more is refused.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The sentence that ends a refusal for a file with no known extension.
const KNOWN: &str = "Gamme reads .mxl, .musicxml, .mei, .mid and .midi files";

/// Reads a file given its name (for its extension) and its content. The error says why it is
/// refused, as a phrase that goes after the file's name.
pub fn read(name: &str, bytes: &[u8]) -> Result<Read, String> {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("mxl") => read_compressed(bytes),
        Some("musicxml" | "xml" | "mei") => read_xml(bytes),
        Some("mid" | "midi") => read_midi(bytes),
        Some(other) => Err(format!("is not a score Gamme reads (.{other}): {KNOWN}")),
        None => Err(format!("has no extension to tell what it is: {KNOWN}")),
    }
}

fn read_midi(bytes: &[u8]) -> Result<Read, String> {
    if !bytes.starts_with(b"MThd") {
        return Err("is not a MIDI file: it does not start with a MIDI header".into());
    }
    let smf = Smf::parse(bytes).map_err(|error| format!("is not a valid MIDI file ({error})"))?;
    let mut notes = false;
    let mut title = None;
    for track in &smf.tracks {
        for event in track {
            match &event.kind {
                TrackEventKind::Midi {
                    message: MidiMessage::NoteOn { vel, .. },
                    ..
                } if vel.as_int() > 0 => notes = true,
                TrackEventKind::Meta(MetaMessage::TrackName(name)) if title.is_none() => {
                    title = clean(&String::from_utf8_lossy(name));
                }
                _ => {}
            }
        }
    }
    if !notes {
        return Err("is a MIDI file with no notes in it".into());
    }
    Ok(Read {
        kind: Kind::Midi,
        title,
        composer: None,
        extension: "mid",
    })
}

/// A compressed MusicXML is a zip: `META-INF/container.xml` names the score inside.
fn read_compressed(bytes: &[u8]) -> Result<Read, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
        format!("is not a compressed MusicXML file: it is not a zip archive ({error})")
    })?;
    let named = match inflate(&mut archive, "META-INF/container.xml")? {
        Some(container) => root_file(&container),
        None => None,
    };
    // Some exporters leave the container out: the first XML file that is not metadata will do.
    let path = named.or_else(|| {
        archive
            .file_names()
            .filter_map(Result::ok)
            .filter(|name| !name.starts_with("META-INF/"))
            .find(|name| {
                let name = name.to_ascii_lowercase();
                name.ends_with(".xml") || name.ends_with(".musicxml")
            })
            .map(std::borrow::Cow::into_owned)
    });
    let Some(path) = path else {
        return Err("is a zip archive with no MusicXML score in it".into());
    };
    let Some(score) = inflate(&mut archive, &path)? else {
        return Err(format!(
            "is a compressed MusicXML file whose score, {path}, is missing from the archive"
        ));
    };
    let read = read_xml(&score)?;
    match read.kind {
        Kind::MusicXml => Ok(Read {
            extension: "mxl",
            ..read
        }),
        _ => Err("is a zip archive whose score is not MusicXML".into()),
    }
}

/// The file `META-INF/container.xml` names as the score.
fn root_file(container: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(container).ok()?;
    let document = Document::parse(text).ok()?;
    document
        .descendants()
        .find(|node| is(node, "rootfile"))?
        .attribute("full-path")
        .map(str::to_owned)
}

/// One file of the archive, whole; `None` when it is not there. What inflates past the limit is
/// refused rather than read.
fn inflate(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "is a compressed MusicXML file that cannot be unpacked ({error})"
            ));
        }
    };
    let mut content = Vec::new();
    entry
        .take(MAX_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|error| {
            format!("is a compressed MusicXML file that cannot be unpacked ({error})")
        })?;
    if content.len() as u64 > MAX_BYTES {
        return Err("is too large once unpacked to be a score".into());
    }
    Ok(Some(content))
}

/// An XML file: its root says whether it is MusicXML or MEI, whatever its extension.
fn read_xml(bytes: &[u8]) -> Result<Read, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "is not a UTF-8 text file: Gamme reads XML scores in UTF-8".to_owned())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // A MusicXML file opens with a DOCTYPE; the parser does not fetch what it names.
    let options = ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    };
    let document = Document::parse_with_options(text, options)
        .map_err(|error| format!("is not well-formed XML ({error})"))?;
    let root = document.root_element();
    match root.tag_name().name() {
        "score-partwise" | "score-timewise" => musicxml(&document),
        "mei" => mei(&document),
        other => Err(format!(
            "is XML, but not a score: its root element is <{other}>, not MusicXML's <score-partwise> or MEI's <mei>"
        )),
    }
}

fn musicxml(document: &Document) -> Result<Read, String> {
    let sounding = document
        .descendants()
        .any(|node| is(&node, "note") && !node.children().any(|child| is(&child, "rest")));
    if !sounding {
        return Err("is MusicXML with no notes in it".into());
    }
    let title = first_text(document, "work-title")
        .or_else(|| first_text(document, "movement-title"))
        .or_else(|| credit_title(document));
    let composer = document
        .descendants()
        .filter(|node| is(node, "creator") && node.attribute("type") == Some("composer"))
        .find_map(|node| text(&node));
    Ok(Read {
        kind: Kind::MusicXml,
        title,
        composer,
        extension: "musicxml",
    })
}

/// The words of the first credit typed as the title, for a score with no title element.
fn credit_title(document: &Document) -> Option<String> {
    document
        .descendants()
        .filter(|node| is(node, "credit"))
        .find(|credit| {
            credit
                .children()
                .any(|child| is(&child, "credit-type") && text(&child).as_deref() == Some("title"))
        })
        .and_then(|credit| {
            credit
                .children()
                .find(|child| is(child, "credit-words"))
                .and_then(|words| text(&words))
        })
}

fn mei(document: &Document) -> Result<Read, String> {
    if !document.descendants().any(|node| is(&node, "music")) {
        return Err("is MEI with no <music> in it: only its header is there".into());
    }
    if !document.descendants().any(|node| is(&node, "note")) {
        return Err("is MEI with no notes in it".into());
    }
    let title = document
        .descendants()
        .find(|node| is(node, "titleStmt"))
        .and_then(|statement| {
            statement
                .descendants()
                .filter(|node| is(node, "title"))
                .find_map(|node| text(&node))
        })
        .or_else(|| first_text(document, "title"));
    let composer = document
        .descendants()
        .filter(|node| {
            is(node, "composer")
                || (is(node, "persName") && node.attribute("role") == Some("composer"))
        })
        .find_map(|node| text(&node));
    Ok(Read {
        kind: Kind::Mei,
        title,
        composer,
        extension: "mei",
    })
}

/// Whether the node is an element of this name, whatever its namespace.
fn is(node: &Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn first_text(document: &Document, name: &str) -> Option<String> {
    document
        .descendants()
        .filter(|node| is(node, name))
        .find_map(|node| text(&node))
}

/// The text under a node, its whitespace gathered, or `None` when there is none.
fn text(node: &Node) -> Option<String> {
    let all: String = node
        .descendants()
        .filter(|descendant| descendant.is_text())
        .filter_map(|descendant| descendant.text())
        .collect::<Vec<_>>()
        .join(" ");
    clean(&all)
}

/// A name a file gave itself, on one line and without control characters; `None` when nothing
/// of it is left.
fn clean(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A MIDI file with one note, a title and a tempo, written by hand: the header, then one track.
    fn midi(title: Option<&str>, note: bool) -> Vec<u8> {
        let mut track = Vec::new();
        if let Some(title) = title {
            track.extend([0x00, 0xFF, 0x03, title.len() as u8]);
            track.extend(title.as_bytes());
        }
        if note {
            track.extend([0x00, 0x90, 60, 64, 0x60, 0x80, 60, 0]);
        }
        track.extend([0x00, 0xFF, 0x2F, 0x00]);
        let mut file = b"MThd".to_vec();
        file.extend([0, 0, 0, 6, 0, 0, 0, 1, 0x01, 0xE0]);
        file.extend(b"MTrk");
        file.extend((track.len() as u32).to_be_bytes());
        file.extend(track);
        file
    }

    const MUSICXML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE score-partwise PUBLIC "-//Recordare//DTD MusicXML 4.0 Partwise//EN" "http://www.musicxml.org/dtds/partwise.dtd">
<score-partwise version="4.0">
  <work><work-title>Little   Song</work-title></work>
  <identification><creator type="composer">A. Writer</creator><creator type="lyricist">B. Poet</creator></identification>
  <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
  <part id="P1"><measure number="1"><note><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration></note></measure></part>
</score-partwise>"#;

    #[test]
    fn musicxml_gives_its_title_and_composer() {
        let read = read("song.musicxml", MUSICXML.as_bytes()).unwrap();
        assert_eq!(read.kind, Kind::MusicXml);
        assert_eq!(read.title.as_deref(), Some("Little Song"));
        assert_eq!(read.composer.as_deref(), Some("A. Writer"));
        assert_eq!(read.extension, "musicxml");
    }

    #[test]
    fn the_content_decides_the_kind_not_the_extension() {
        let read = read("song.xml", MUSICXML.as_bytes()).unwrap();
        assert_eq!(read.kind, Kind::MusicXml);
    }

    #[test]
    fn a_title_falls_back_to_the_movement_then_the_credit() {
        let movement = MUSICXML.replace("work-title", "movement-title");
        let found = read("a.musicxml", movement.as_bytes()).unwrap();
        assert_eq!(found.title.as_deref(), Some("Little Song"));
        let credit = MUSICXML.replace(
            "<work><work-title>Little   Song</work-title></work>",
            "<credit><credit-type>title</credit-type><credit-words>From a credit</credit-words></credit>",
        );
        let found = read("a.musicxml", credit.as_bytes()).unwrap();
        assert_eq!(found.title.as_deref(), Some("From a credit"));
    }

    #[test]
    fn musicxml_of_rests_only_is_refused() {
        let rests = MUSICXML.replace("<pitch><step>C</step><octave>4</octave></pitch>", "<rest/>");
        let why = read("a.musicxml", rests.as_bytes()).unwrap_err();
        assert!(why.contains("no notes"), "{why}");
    }

    #[test]
    fn xml_that_is_not_a_score_says_what_its_root_is() {
        let why = read("page.xml", b"<html><body/></html>").unwrap_err();
        assert!(why.contains("<html>"), "{why}");
    }

    #[test]
    fn xml_that_is_cut_short_is_refused_with_where() {
        let cut = &MUSICXML[..MUSICXML.len() - 30];
        let why = read("a.musicxml", cut.as_bytes()).unwrap_err();
        assert!(why.starts_with("is not well-formed XML"), "{why}");
    }

    #[test]
    fn a_text_that_is_not_utf8_is_refused() {
        let why = read("a.musicxml", &[0xFF, 0xFE, b'<', 0, b'a', 0]).unwrap_err();
        assert!(why.contains("UTF-8"), "{why}");
    }

    #[test]
    fn mei_gives_its_title_and_composer() {
        let mei = r#"<?xml version="1.0" encoding="UTF-8"?>
<mei xmlns="http://www.music-encoding.org/ns/mei" meiversion="5.0">
  <meiHead><fileDesc><titleStmt><title>Short  Dance</title>
    <respStmt><persName role="composer">C. Maker</persName></respStmt></titleStmt>
    <pubStmt/></fileDesc></meiHead>
  <music><body><mdiv><score><section><measure><staff n="1"><layer><note pname="c" oct="4" dur="4"/></layer></staff></measure></section></score></mdiv></body></music>
</mei>"#;
        let found = read("dance.mei", mei.as_bytes()).unwrap();
        assert_eq!(found.kind, Kind::Mei);
        assert_eq!(found.title.as_deref(), Some("Short Dance"));
        assert_eq!(found.composer.as_deref(), Some("C. Maker"));
        assert_eq!(found.extension, "mei");
        let headless = mei.replace("<note pname=\"c\" oct=\"4\" dur=\"4\"/>", "");
        assert!(
            read("dance.mei", headless.as_bytes())
                .unwrap_err()
                .contains("no notes")
        );
    }

    #[test]
    fn midi_gives_the_name_of_its_track() {
        let found = read("tune.MID", &midi(Some("A tune"), true)).unwrap();
        assert_eq!(found.kind, Kind::Midi);
        assert!(!found.kind.engraved());
        assert_eq!(found.title.as_deref(), Some("A tune"));
        assert_eq!(found.composer, None);
        assert_eq!(found.extension, "mid");
        assert_eq!(read("tune.mid", &midi(None, true)).unwrap().title, None);
    }

    #[test]
    fn midi_with_no_notes_or_no_header_is_refused() {
        let why = read("tune.mid", &midi(Some("x"), false)).unwrap_err();
        assert!(why.contains("no notes"), "{why}");
        let why = read("tune.mid", b"not midi at all").unwrap_err();
        assert!(why.contains("MIDI header"), "{why}");
    }

    #[test]
    fn a_compressed_musicxml_is_read_through_its_container() {
        use std::io::Write as _;
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive
            .start_file("META-INF/container.xml", options)
            .unwrap();
        archive
            .write_all(
                br#"<container><rootfiles><rootfile full-path="inner/score.xml"/></rootfiles></container>"#,
            )
            .unwrap();
        archive.start_file("inner/score.xml", options).unwrap();
        archive.write_all(MUSICXML.as_bytes()).unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let read = read("song.mxl", &bytes).unwrap();
        assert_eq!(read.kind, Kind::MusicXml);
        assert_eq!(read.title.as_deref(), Some("Little Song"));
        assert_eq!(read.extension, "mxl");
    }

    #[test]
    fn an_mxl_that_is_no_archive_or_holds_no_score_is_refused() {
        let why = read("a.mxl", b"plain text").unwrap_err();
        assert!(why.contains("not a zip archive"), "{why}");
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("notes.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let why = read("a.mxl", &bytes).unwrap_err();
        assert!(why.contains("no MusicXML score"), "{why}");
    }

    #[test]
    fn other_extensions_are_refused_with_what_is_read() {
        let why = read("scan.pdf", b"%PDF").unwrap_err();
        assert!(why.contains(".pdf") && why.contains(".mxl"), "{why}");
        let why = read("README", b"x").unwrap_err();
        assert!(why.contains("no extension"), "{why}");
    }
}
