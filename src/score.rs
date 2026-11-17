//! A score engraved: its pages as SVG trees, where each measure, system and note sits on them,
//! and where the cursor is at each moment of the piece.
//!
//! Places are read from the trees by composing each group's transform down to what it draws:
//! usvg's absolute boxes are wrong for Verovio's SVG (a `<use>`'s offset counted twice, the
//! empty groups of notes and measures pulling boxes to the origin).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use resvg::usvg::{self, fontdb, roxmltree};

use crate::engraver::{Engraved, Event, Result};

/// A box on a page, in the page's own units (those of [`Score::page_size`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Area {
    fn of(rect: usvg::Rect) -> Self {
        Self {
            left: rect.left(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

/// Something engraved: its page (from 0) and its box there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub page: usize,
    pub area: Area,
}

/// A measure as it is played: a repeat plays the same engraved measure again.
#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    /// When it starts, in ms.
    pub at: f64,
    /// Its number in the order of play, from 1.
    pub number: usize,
    /// Its number as the score prints it; its number in the order of play when it has none.
    pub name: String,
    pub page: usize,
    /// Its own box, across the height of its system.
    pub area: Area,
}

/// Where the cursor is drawn: a line across a system.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cursor {
    pub page: usize,
    pub x: f32,
    pub top: f32,
    pub bottom: f32,
}

/// What the score shows at one moment.
#[derive(Debug, Default)]
pub struct Moment<'a> {
    pub measure: Option<&'a Measure>,
    pub cursor: Option<Cursor>,
    /// The noteheads that sound.
    pub sounding: &'a [Placed],
}

/// A moment of the timemap, placed.
struct Step {
    at: f64,
    /// The index in `measures` of the measure it is in.
    measure: Option<usize>,
    sounding: Vec<Placed>,
}

/// A point the cursor passes: the leftmost note starting at a step, or the start of a measure
/// that starts with nothing (a rest).
#[derive(Clone, Copy)]
struct Mark {
    at: f64,
    x: f32,
    measure: usize,
}

pub struct Score {
    pages: Vec<usvg::Tree>,
    /// Each note the timemap plays, by its engraved id: its head.
    heads: HashMap<String, Placed>,
    measures: Vec<Measure>,
    steps: Vec<Step>,
    marks: Vec<Mark>,
    duration: f64,
}

impl Score {
    /// Reads the pages Verovio drew and places its timemap on them.
    pub fn place(engraved: Engraved) -> Result<Self> {
        let options = usvg::Options {
            fontdb: fonts(),
            ..usvg::Options::default()
        };
        let mut pages = Vec::with_capacity(engraved.pages.len());
        let mut parts = Parts::default();
        for svg in &engraved.pages {
            let svg = parts.read(svg)?;
            pages.push(
                usvg::Tree::from_str(&svg, &options)
                    .map_err(|error| format!("a page cannot be read: {error}"))?,
            );
        }
        let wanted = parts.wanted(&engraved.events);
        let mut placed = HashMap::new();
        for (page, tree) in pages.iter().enumerate() {
            collect(
                tree.root(),
                usvg::Transform::default(),
                page,
                &wanted,
                &mut placed,
            );
        }
        let mut score = Self::timeline(&engraved, &parts, &placed);
        score.pages = pages;
        Ok(score)
    }

    /// The measures, steps and marks of the timemap, placed with `placed`.
    fn timeline(engraved: &Engraved, parts: &Parts, placed: &HashMap<String, Placed>) -> Self {
        let events = &engraved.events;
        let note = |id: &String| {
            let head = parts.heads.get(id).unwrap_or(id);
            placed.get(head).or_else(|| placed.get(id)).copied()
        };
        let mut measures: Vec<Measure> = Vec::new();
        let mut steps = Vec::with_capacity(events.len());
        let mut marks: Vec<Mark> = Vec::new();
        let mut sounding: Vec<&String> = Vec::new();
        let mut heads = HashMap::new();
        let mut current = None;
        for event in events {
            sounding.retain(|id| !event.off.contains(*id));
            sounding.extend(&event.on);
            for id in &event.on {
                if let Some(head) = note(id) {
                    heads.insert(id.clone(), head);
                }
            }
            if let Some(id) = &event.measure_on {
                let number = measures.len() + 1;
                let name = engraved
                    .numbers
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| number.to_string());
                current = measure(id, event.tstamp, number, name, parts, placed).map(|m| {
                    measures.push(m);
                    measures.len() - 1
                });
            }
            let onset = current.and_then(|index| {
                let page = measures[index].page;
                event
                    .on
                    .iter()
                    .filter_map(note)
                    .filter(|placed| placed.page == page)
                    .map(|placed| placed.area.left)
                    .min_by(f32::total_cmp)
            });
            match (onset, current) {
                (Some(x), Some(index)) => marks.push(Mark {
                    at: event.tstamp,
                    x,
                    measure: index,
                }),
                (None, Some(index)) if event.measure_on.is_some() => marks.push(Mark {
                    at: event.tstamp,
                    x: measures[index].area.left,
                    measure: index,
                }),
                _ => {}
            }
            steps.push(Step {
                at: event.tstamp,
                measure: current,
                sounding: sounding.iter().copied().filter_map(note).collect(),
            });
        }
        let duration = events.last().map_or(0., |event| event.tstamp);
        Self {
            pages: Vec::new(),
            heads,
            measures,
            steps,
            marks,
            duration,
        }
    }

    pub fn pages(&self) -> &[usvg::Tree] {
        &self.pages
    }

    /// A page's size, in its own units.
    pub fn page_size(&self, page: usize) -> (f32, f32) {
        let size = self.pages[page].size();
        (size.width(), size.height())
    }

    /// Where the head of the engraved note `id` (a timeline's [`Timeline::element`]) is.
    ///
    /// [`Timeline::element`]: crate::timeline::Timeline::element
    pub fn head(&self, id: &str) -> Option<Placed> {
        self.heads.get(id).copied()
    }

    /// The length of the piece, in ms.
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// The measures in the order they are played.
    pub fn measures(&self) -> &[Measure] {
        &self.measures
    }

    /// What is shown at `ms`: the measure, the cursor, the notes that sound.
    pub fn at(&self, ms: f64) -> Moment<'_> {
        let Some(step) = self
            .steps
            .partition_point(|step| step.at <= ms)
            .checked_sub(1)
            .map(|index| &self.steps[index])
        else {
            return Moment::default();
        };
        Moment {
            measure: step.measure.map(|index| &self.measures[index]),
            cursor: self.cursor(ms),
            sounding: &step.sounding,
        }
    }

    /// The cursor at `ms`, across its measure's system, gliding from one mark to the next; to
    /// the end of the measure when the next mark is on another system.
    fn cursor(&self, ms: f64) -> Option<Cursor> {
        let k = self
            .marks
            .partition_point(|mark| mark.at <= ms)
            .checked_sub(1)?;
        let from = self.marks[k];
        let measure = &self.measures[from.measure];
        let (to_x, to_at) = match self.marks.get(k + 1) {
            Some(next) => {
                let next_measure = &self.measures[next.measure];
                let same_system = next_measure.page == measure.page
                    && next_measure.area.top < measure.area.bottom
                    && next_measure.area.bottom > measure.area.top;
                match same_system && next.x >= from.x {
                    true => (next.x, next.at),
                    false => (measure.area.right, next.at),
                }
            }
            None => (measure.area.right, self.duration.max(from.at + 1.)),
        };
        let t = ((ms - from.at) / (to_at - from.at)).clamp(0., 1.) as f32;
        Some(Cursor {
            page: measure.page,
            x: from.x + (to_x - from.x) * t,
            top: measure.area.top,
            bottom: measure.area.bottom,
        })
    }
}

/// The measure `id` starting at `at`: its own width, its system's height.
fn measure(
    id: &str,
    at: f64,
    number: usize,
    name: String,
    parts: &Parts,
    placed: &HashMap<String, Placed>,
) -> Option<Measure> {
    let own = placed.get(id)?;
    let system = parts
        .systems
        .get(id)
        .and_then(|system| placed.get(system))
        .filter(|system| system.page == own.page)
        .map_or(own.area, |system| system.area);
    Some(Measure {
        at,
        number,
        name,
        page: own.page,
        area: Area {
            left: own.area.left,
            right: own.area.right,
            top: system.top,
            bottom: system.bottom,
        },
    })
}

/// What Verovio's classes say and the trees no longer do: the system each measure is in, and
/// the head of each note (a note's own box takes in its stem and flag). Verovio gives a
/// notehead no id: it is given one, `<note>-head`, for the tree to keep.
#[derive(Default)]
struct Parts {
    /// Measure id to system id.
    systems: HashMap<String, String>,
    /// Note id to notehead id.
    heads: HashMap<String, String>,
}

impl Parts {
    /// Reads a page, and gives it back with an id on each notehead.
    fn read(&mut self, svg: &str) -> Result<String> {
        let options = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..roxmltree::ParsingOptions::default()
        };
        let document = roxmltree::Document::parse_with_options(svg, options)
            .map_err(|error| format!("a page cannot be read: {error}"))?;
        fn is(node: roxmltree::Node, class: &str) -> bool {
            node.tag_name().name() == "g"
                && node
                    .attribute("class")
                    .is_some_and(|classes| classes.split_whitespace().any(|c| c == class))
        }
        // Where an id goes, after `<g`, in the order of the text.
        let mut ids: Vec<(usize, String)> = Vec::new();
        for node in document.descendants() {
            let Some(id) = node.attribute("id") else {
                continue;
            };
            if is(node, "measure") {
                let system = node
                    .ancestors()
                    .find(|above| is(*above, "system"))
                    .and_then(|system| system.attribute("id"));
                if let Some(system) = system {
                    self.systems.insert(id.to_owned(), system.to_owned());
                }
            } else if is(node, "note") {
                let Some(head) = node.descendants().find(|below| is(*below, "notehead")) else {
                    continue;
                };
                let head_id = match head.attribute("id") {
                    Some(head_id) => head_id.to_owned(),
                    None if svg[head.range().start..].starts_with("<g") => {
                        let head_id = format!("{id}-head");
                        ids.push((head.range().start + "<g".len(), head_id.clone()));
                        head_id
                    }
                    None => continue,
                };
                self.heads.insert(id.to_owned(), head_id);
            }
        }
        let mut page = String::with_capacity(svg.len() + ids.len() * 24);
        let mut from = 0;
        for (at, id) in ids {
            page.push_str(&svg[from..at]);
            page.push_str(&format!(" id=\"{id}\""));
            from = at;
        }
        page.push_str(&svg[from..]);
        Ok(page)
    }

    /// The ids to place for `events`: their measures and systems, their notes and noteheads.
    fn wanted(&self, events: &[Event]) -> HashSet<String> {
        let mut wanted = HashSet::new();
        for event in events {
            for id in event.on.iter().chain(&event.measure_on) {
                wanted.extend(self.systems.get(id).cloned());
                wanted.extend(self.heads.get(id).cloned());
                wanted.insert(id.clone());
            }
        }
        wanted
    }
}

/// The system's fonts, read once: Verovio's titles ask for "Times, serif".
fn fonts() -> Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut fonts = fontdb::Database::new();
            fonts.load_system_fonts();
            // fontdb's own serif is "Times New Roman"; without it, it may take a symbol font.
            let serif = [
                "Times New Roman",
                "Nimbus Roman",
                "Liberation Serif",
                "DejaVu Serif",
            ]
            .into_iter()
            .find(|family| {
                fonts
                    .faces()
                    .any(|face| face.families.iter().any(|(name, _)| name == *family))
            });
            if let Some(serif) = serif {
                fonts.set_serif_family(serif);
            }
            Arc::new(fonts)
        })
        .clone()
}

/// Places every node of `group` whose id is `wanted`, `parent` the transform above it.
fn collect(
    group: &usvg::Group,
    parent: usvg::Transform,
    page: usize,
    wanted: &HashSet<String>,
    out: &mut HashMap<String, Placed>,
) {
    let transform = parent.pre_concat(group.transform());
    for node in group.children() {
        if wanted.contains(node.id())
            && let Some(area) = ink(node, transform)
        {
            out.insert(node.id().to_owned(), Placed { page, area });
        }
        if let usvg::Node::Group(child) = node {
            collect(child, transform, page, wanted, out);
        }
    }
}

/// The box around what `node` draws on the page, `parent` the transform above it: each group's
/// own transform composed the way resvg draws, and empty groups taking no room.
fn ink(node: &usvg::Node, parent: usvg::Transform) -> Option<Area> {
    let usvg::Node::Group(group) = node else {
        return node.bounding_box().transform(parent).map(Area::of);
    };
    let transform = parent.pre_concat(group.transform());
    group
        .children()
        .iter()
        .filter_map(|child| ink(child, transform))
        .reduce(Area::union)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::engraver;

    /// A page shaped as Verovio draws one: a nested, scaled `<svg>`, a page margin moved by a
    /// transform, glyphs placed by `<use>` with an offset, and an empty group.
    const PAGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="200px" height="100px" version="1.1">
<defs><path id="head" d="M0 0 L10 0 L10 8 L0 8 Z"/></defs>
<svg class="definition-scale" viewBox="0 0 2000 1000">
<g class="page-margin" transform="translate(100, 50)">
<g id="sys1" class="system">
  <path d="M0 100 L1800 100" stroke="black" stroke-width="2"/>
  <path d="M0 300 L1800 300" stroke="black" stroke-width="2"/>
  <g id="m1" class="measure">
    <path d="M0 100 L0 300" stroke="black" stroke-width="2"/>
    <g id="n1" class="note">
      <g id="h1" class="notehead"><use xlink:href="#head" x="200" y="150" transform="translate(200, 150) scale(2, 2) translate(-200, -150)"/></g>
      <g id="s1" class="stem"><path d="M218 50 L218 166" stroke="black" stroke-width="2"/></g>
    </g>
    <g id="empty" class="dir"></g>
  </g>
  <g id="m2" class="measure">
    <path d="M900 100 L900 300" stroke="black" stroke-width="2"/>
    <g id="n2" class="note">
      <g class="notehead"><use xlink:href="#head" x="1100" y="200"/></g>
      <g id="s2" class="stem"><path d="M1108 100 L1108 208" stroke="black" stroke-width="2"/></g>
    </g>
  </g>
</g>
</g>
</svg>
</svg>"##;

    fn event(at: f64, on: &[&str], off: &[&str], measure: Option<&str>) -> Event {
        Event {
            tstamp: at,
            on: on.iter().map(|id| id.to_string()).collect(),
            off: off.iter().map(|id| id.to_string()).collect(),
            measure_on: measure.map(str::to_owned),
        }
    }

    fn score() -> Score {
        let events = vec![
            event(0., &["n1"], &[], Some("m1")),
            event(1000., &["n2"], &["n1"], Some("m2")),
            // The repeat plays m1 again.
            event(2000., &["n1"], &["n2"], Some("m1")),
            event(3000., &[], &["n1"], None),
        ];
        Score::place(Engraved {
            pages: vec![PAGE.to_owned()],
            events,
            numbers: HashMap::from([("m1".to_owned(), "1".to_owned())]),
            midi: Vec::new(),
            written: HashMap::new(),
        })
        .unwrap()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn a_notehead_is_placed_by_its_transforms_composed() {
        let score = score();
        let head = score.at(0.).sounding[0];
        // Units of the outer page: the viewBox halves everything, the margin moves it by
        // (100, 50), the `<use>` scales the glyph by 2 around (200, 150).
        let expected = Area {
            left: (100. + 200.) / 10.,
            top: (50. + 150.) / 10.,
            right: (100. + 220.) / 10.,
            bottom: (50. + 166.) / 10.,
        };
        let area = head.area;
        assert!(
            close(area.left, expected.left)
                && close(area.top, expected.top)
                && close(area.right, expected.right)
                && close(area.bottom, expected.bottom),
            "{area:?} is not {expected:?}"
        );
    }

    #[test]
    fn a_measure_is_as_tall_as_its_system_and_an_empty_group_takes_no_room() {
        let score = score();
        let measures = score.measures();
        assert_eq!(
            measures.len(),
            3,
            "the repeat plays the first measure again"
        );
        let first = measures[0].area;
        // m1 draws from its barline (x 0) to its notehead (x 220); its empty group does not
        // pull it to 0, 0. Boxes are those of the shapes, strokes left out.
        assert!(close(first.left, (100. + 0.) / 10.), "{first:?}");
        assert!(close(first.right, (100. + 220.) / 10.), "{first:?}");
        // Its height is its system's: from the stem's top (y 50) to the lowest line (y 300).
        assert!(close(first.top, (50. + 50.) / 10.), "{first:?}");
        assert!(close(first.bottom, (50. + 300.) / 10.), "{first:?}");
        assert_eq!(measures[2].area, first);
        assert_eq!(
            measures.iter().map(|m| m.number).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // Named as printed; a measure with no number of its own by its order of play.
        assert_eq!(
            measures.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["1", "2", "1"]
        );
    }

    #[test]
    fn the_cursor_glides_from_note_to_note_in_its_measure() {
        let score = score();
        let start = score.at(0.).cursor.unwrap();
        assert!(close(start.x, 30.), "{start:?}");
        let half = score.at(500.).cursor.unwrap();
        // Halfway to the note of the next measure, on the same system.
        assert!(close(half.x, (30. + 120.) / 2.), "{half:?}");
        let second = score.at(1000.);
        assert_eq!(second.measure.unwrap().number, 2);
        assert!(close(second.cursor.unwrap().x, 120.));
        // Back at the repeat: the cursor is in the first measure again.
        let again = score.at(2500.);
        assert_eq!(again.measure.unwrap().number, 3);
        let x = again.cursor.unwrap().x;
        assert!(x >= 30. && x <= score.measures()[2].area.right, "{x}");
        assert!(score.at(-1.).cursor.is_none());
    }

    /// Verovio itself, on the fixture: 108 measures over three pages, the first eight repeated.
    #[test]
    fn a_piece_is_engraved_and_its_cursor_is_in_the_measure_played() {
        let cache = tempfile::tempdir().unwrap();
        let engraver = engraver::Engraver::start(Some(cache.path().join("gamme")));
        let piece =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exercise-in-c.musicxml");
        let score = Score::place(engraver.engrave(&piece).unwrap()).unwrap();
        assert_eq!(score.pages().len(), 3);

        let measures = score.measures();
        assert_eq!(measures.len(), 116, "108 measures, 8 of them played twice");
        let names: Vec<&str> = measures.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names[..10],
            ["1", "2", "3", "4", "5", "6", "7", "8", "1", "2"]
        );
        assert_eq!(names[115], "108");
        assert_eq!(
            measures[8].area, measures[0].area,
            "the repeat is the same measure"
        );
        let pages: HashSet<usize> = measures.iter().map(|m| m.page).collect();
        assert_eq!(pages.len(), 3, "measures on every page");

        for (index, measure) in measures.iter().enumerate() {
            let end = measures
                .get(index + 1)
                .map_or(score.duration(), |next| next.at);
            let moment = score.at((measure.at + end) / 2.);
            let name = &measure.name;
            assert_eq!(moment.measure, Some(measure), "measure {name}");
            let cursor = moment.cursor.unwrap();
            assert_eq!(cursor.page, measure.page, "measure {name}");
            let area = measure.area;
            assert!(
                (area.left..=area.right).contains(&cursor.x),
                "measure {name}: the cursor at {cursor:?} is out of {area:?}"
            );
            assert!(!moment.sounding.is_empty(), "measure {name}");
            for head in moment.sounding {
                let inside = head.page == measure.page
                    && head.area.left >= area.left
                    && head.area.right <= area.right
                    && head.area.top >= area.top
                    && head.area.bottom <= area.bottom;
                assert!(
                    inside,
                    "measure {name}: a head at {head:?} is out of {area:?}"
                );
                // A head is wider than tall; with its stem, a note is not.
                let a = head.area;
                assert!(a.height() < a.width(), "a head, not its stem: {head:?}");
            }
        }

        // Verovio's resources went into the cache, once, and nothing else did.
        let entries: Vec<_> = std::fs::read_dir(cache.path().join("gamme"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            matches!(&entries[..], [one] if one.starts_with("verovio-")),
            "{entries:?}"
        );
    }

    #[test]
    fn the_notes_that_sound_are_those_started_and_not_stopped() {
        let score = score();
        assert_eq!(score.at(999.).sounding.len(), 1);
        let second = score.at(1000.).sounding;
        assert_eq!(second.len(), 1);
        // The head Verovio gave no id, without its stem.
        let head = second[0].area;
        assert!(close(head.left, 120.) && close(head.top, 25.), "{head:?}");
        assert!(score.at(3000.).sounding.is_empty());
        assert_eq!(score.duration(), 3000.);
    }

    #[test]
    fn a_note_played_is_found_by_its_id_at_its_head() {
        let score = score();
        assert_eq!(score.head("n1"), Some(score.at(0.).sounding[0]));
        assert_eq!(score.head("n2"), Some(score.at(1000.).sounding[0]));
        assert_eq!(score.head("m1"), None, "a measure is no note");
    }
}
