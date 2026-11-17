//! The notes of a piece in time, per hand, each with the measure it starts in and, for an engraved
//! piece, the engraved note it is.
//!
//! Times are milliseconds of the piece **at its own tempo** (speed 100 %): the clock (`clock.rs`)
//! scales real time into them, so nothing here knows the speed.
//!
//! - An engraved piece's timeline ([`Timeline::from_engraved`]) is read from Verovio's MIDI output
//!   (one track per staff, ties joined, repeats played through), and each of its notes is found
//!   again among the notes the timemap starts at the same moment on the same staff: the timemap
//!   names them by their engraved ids (`engraver.rs` maps a repeat's copies back), so a note
//!   played twice by a repeat is the same engraved note twice. Verovio's own
//!   `midi_values_for_element` would say it directly, but renders the whole timemap at each call.
//! - A MIDI file's ([`Timeline::from_smf`]) has its tracks, and the channels of a track, as parts.
//!
//! Hands are given by parts: one staff, one hand, the upper staves the right; for a MIDI file a
//! guess the player can correct ([`Timeline::with_hands`]), kept with the piece in the library.

use std::collections::HashMap;

use midly::{Format, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use serde::{Deserialize, Serialize};

use crate::engraver::Event;

/// The key a single part is split at when it is played by both hands: the middle C goes to the
/// right hand.
const MIDDLE_C: u8 = 60;

/// How far a note of Verovio's MIDI and the timemap's moment it starts at may be apart: both come
/// from the same score times, rounded differently (whole ms, whole MIDI ticks).
const SAME_MOMENT_MS: f64 = 30.;

/// Which hand plays a note: the upper staff is the right hand, the lower one the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hand {
    Right,
    Left,
}

/// Which hand plays a part: one hand, or both, split at the middle C.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartHand {
    Right,
    Left,
    Split,
}

impl PartHand {
    fn hand(self, key: u8) -> Hand {
        match self {
            PartHand::Right => Hand::Right,
            PartHand::Left => Hand::Left,
            PartHand::Split if key >= MIDDLE_C => Hand::Right,
            PartHand::Split => Hand::Left,
        }
    }
}

/// What the hands are given by: a staff of an engraved piece, a track (or one channel of a track)
/// of a MIDI file.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    /// "Staff 2", or a track's name ("Track 3" when it has none).
    pub name: String,
    pub hand: PartHand,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// MIDI key, 0–127 (60 is the middle C).
    pub key: u8,
    pub velocity: u8,
    pub on_ms: f64,
    pub off_ms: f64,
    pub hand: Hand,
    /// Its part, in [`Timeline::parts`].
    pub part: u16,
    /// The measure it starts in, in the order of play (see [`Timeline::measure_at`]).
    pub measure: u32,
    /// The engraved note it is ([`Timeline::element`]); none for a MIDI file.
    pub element: Option<u32>,
}

/// What an engraved note is written as: its staff (`@n`, from 1) and its pitch as written
/// without its accidental (its `@pname` and `@oct`), which the key played is near.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    pub staff: u16,
    pub key: Option<u8>,
}

impl Written {
    /// The key of `pname` (`c` to `b`) in `oct` (4 holds the middle C), without accidental.
    pub fn key(pname: &str, oct: &str) -> Option<u8> {
        let step = match pname {
            "c" => 0,
            "d" => 2,
            "e" => 4,
            "f" => 5,
            "g" => 7,
            "a" => 9,
            "b" => 11,
            _ => return None,
        };
        let oct: i32 = oct.parse().ok()?;
        u8::try_from(12 * (oct + 1) + step)
            .ok()
            .filter(|&key| key < 128)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    /// Sorted by `on_ms`.
    notes: Vec<Note>,
    /// Start of each measure, in order; never empty, the first is 0.
    measures: Vec<f64>,
    duration_ms: f64,
    parts: Vec<Part>,
    /// The engraved notes' ids, by [`Note::element`].
    elements: Vec<String>,
}

impl Timeline {
    /// `notes` in any order, `measures` the start of each measure (a measure starting at 0 is
    /// added when none does), `duration_ms` at least as long as the last note. Each note's
    /// measure is found here.
    pub fn new(notes: Vec<Note>, measures: Vec<f64>, duration_ms: f64) -> Self {
        Self::with_parts(notes, measures, duration_ms, Vec::new(), Vec::new())
    }

    fn with_parts(
        mut notes: Vec<Note>,
        mut measures: Vec<f64>,
        duration_ms: f64,
        parts: Vec<Part>,
        elements: Vec<String>,
    ) -> Self {
        notes.sort_by(|a, b| a.on_ms.total_cmp(&b.on_ms).then(a.key.cmp(&b.key)));
        measures.sort_by(f64::total_cmp);
        if measures.first().is_none_or(|&first| first > 0.) {
            measures.insert(0, 0.);
        }
        let last = notes.iter().map(|note| note.off_ms).fold(0., f64::max);
        let mut timeline = Self {
            notes,
            measures,
            duration_ms: duration_ms.max(last),
            parts,
            elements,
        };
        for index in 0..timeline.notes.len() {
            let measure = timeline.measure_at(timeline.notes[index].on_ms);
            timeline.notes[index].measure = u32::try_from(measure).unwrap_or(u32::MAX);
        }
        timeline
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    pub fn duration_ms(&self) -> f64 {
        self.duration_ms
    }

    pub fn measure_count(&self) -> usize {
        self.measures.len()
    }

    /// Where measure `index` (0 is the first) starts.
    pub fn measure_start_ms(&self, index: usize) -> Option<f64> {
        self.measures.get(index).copied()
    }

    /// The measure `ms` falls in: the last one starting at or before it.
    pub fn measure_at(&self, ms: f64) -> usize {
        self.measures
            .partition_point(|&start| start <= ms)
            .saturating_sub(1)
    }

    /// The parts the hands are given by, in the order of the score or the file.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The id of the engraved note `note` is, for an engraved piece.
    pub fn element(&self, note: &Note) -> Option<&str> {
        self.elements
            .get(note.element? as usize)
            .map(String::as_str)
    }

    /// Each part's hand, in the order of [`Timeline::parts`].
    pub fn hands(&self) -> Vec<PartHand> {
        self.parts.iter().map(|part| part.hand).collect()
    }

    /// The same piece with its parts given to `hands` (one per part, in order); a list that does
    /// not fit the parts (a file changed since it was made) changes nothing.
    pub fn with_hands(&self, hands: &[PartHand]) -> Self {
        let mut timeline = self.clone();
        if hands.len() != timeline.parts.len() {
            return timeline;
        }
        for (part, &hand) in timeline.parts.iter_mut().zip(hands) {
            part.hand = hand;
        }
        for note in &mut timeline.notes {
            if let Some(part) = timeline.parts.get(usize::from(note.part)) {
                note.hand = part.hand.hand(note.key);
            }
        }
        timeline
    }

    /// Reads a Standard MIDI File, a piece of its own.
    ///
    /// - Tempo changes and time signatures of every track count (the measures follow them).
    /// - Each track holding notes is a part, each of its channels one when it has several (a
    ///   piano on one track, its hands on two channels). Their hands are guessed: a track named
    ///   for a hand is that hand; the others are split where their average keys are furthest
    ///   apart, the higher ones the right hand. A file with one part is split at the middle C.
    /// - The pedal and the drums (channel 10) are ignored.
    pub fn from_smf(bytes: &[u8]) -> Result<Self, String> {
        let read = Read::parse(bytes)?;
        let mut keys: Vec<(usize, u8)> = read.notes.iter().map(|n| (n.track, n.channel)).collect();
        keys.sort_unstable();
        keys.dedup();
        let mut tracks: Vec<usize> = keys.iter().map(|&(track, _)| track).collect();
        tracks.dedup();
        let names: Vec<Option<String>> = keys
            .iter()
            .map(|&(track, channel)| {
                let several = keys.iter().filter(|key| key.0 == track).count() > 1;
                let name = read.names.get(&track).cloned();
                match (name, several) {
                    (Some(name), false) => Some(name),
                    (Some(name), true) => Some(format!("{name}, channel {}", channel + 1)),
                    (None, _) => None,
                }
            })
            .collect();
        let part_of = |note: &Raw| {
            keys.binary_search(&(note.track, note.channel))
                .expect("every note's part is listed") as u16
        };
        let means: Vec<f64> = (0..keys.len())
            .map(|part| {
                let (sum, count) = read
                    .notes
                    .iter()
                    .filter(|note| usize::from(part_of(note)) == part)
                    .fold((0., 0.), |(sum, count), note| {
                        (sum + f64::from(note.key), count + 1.)
                    });
                sum / count
            })
            .collect();
        let hands = guess_hands(&names, &means);
        let parts: Vec<Part> = keys
            .iter()
            .zip(names)
            .zip(&hands)
            .map(|((&(track, channel), name), &hand)| Part {
                name: name.unwrap_or_else(|| {
                    let ordinal = tracks.iter().position(|&t| t == track).unwrap_or(0) + 1;
                    let several = keys.iter().filter(|key| key.0 == track).count() > 1;
                    match several {
                        true => format!("Track {ordinal}, channel {}", channel + 1),
                        false => format!("Track {ordinal}"),
                    }
                }),
                hand,
            })
            .collect();

        let notes = read
            .notes
            .iter()
            .map(|note| {
                let part = part_of(note);
                Note {
                    key: note.key,
                    velocity: note.velocity,
                    on_ms: read.tempo.ms(note.start),
                    off_ms: read.tempo.ms(note.end),
                    hand: hands[usize::from(part)].hand(note.key),
                    part,
                    measure: 0,
                    element: None,
                }
            })
            .collect();

        // The piece ends with its last note, not with a track's trailing silence.
        let last_note_tick = read.notes.iter().map(|note| note.end).max().unwrap_or(0);
        let measures = measure_ticks(
            read.ticks_per_beat,
            read.signatures.clone(),
            last_note_tick.max(1),
        )?
        .into_iter()
        .map(|tick| read.tempo.ms(tick))
        .collect();
        Ok(Self::with_parts(
            notes,
            measures,
            read.tempo.ms(read.end_tick),
            parts,
            Vec::new(),
        ))
    }

    /// The timeline of an engraved piece: `midi` is Verovio's MIDI output for it, `events` its
    /// timemap (ids mapped back to the engraved notes), `written` what each engraved note is
    /// written as. The measures are the timemap's, in the order of play.
    pub fn from_engraved(
        midi: &[u8],
        events: &[Event],
        written: &HashMap<String, Written>,
    ) -> Result<Self, String> {
        let read = Read::parse(midi).map_err(|error| format!("Verovio's MIDI: {error}"))?;
        // Verovio writes staff n on track n.
        let mut staves: Vec<usize> = read.notes.iter().map(|note| note.track).collect();
        staves.sort_unstable();
        staves.dedup();
        let mean = {
            let keys: Vec<f64> = read.notes.iter().map(|n| f64::from(n.key)).collect();
            keys.iter().sum::<f64>() / keys.len().max(1) as f64
        };
        let parts: Vec<Part> = staves
            .iter()
            .enumerate()
            .map(|(index, staff)| Part {
                name: format!("Staff {staff}"),
                hand: staff_hand(index, staves.len(), mean),
            })
            .collect();

        // The engraved notes each moment of the timemap starts.
        let mut elements: Vec<String> = Vec::new();
        let mut element_of: HashMap<&str, u32> = HashMap::new();
        let mut onsets: Vec<Onset> = Vec::new();
        for event in events {
            for id in &event.on {
                let Some(found) = written.get(id) else {
                    continue;
                };
                let element = *element_of.entry(id).or_insert_with(|| {
                    elements.push(id.clone());
                    (elements.len() - 1) as u32
                });
                onsets.push(Onset {
                    at: event.tstamp,
                    staff: usize::from(found.staff),
                    key: found.key,
                    element,
                });
            }
        }
        onsets.sort_by(|a, b| a.at.total_cmp(&b.at));

        // Every pairing of a MIDI note with an engraved note of its staff starting with it, the
        // nearest in pitch first: a note tied from before starts in the timemap but not in the
        // MIDI, and must not take a chord's other note.
        let times: Vec<(f64, f64)> = read
            .notes
            .iter()
            .map(|note| (read.tempo.ms(note.start), read.tempo.ms(note.end)))
            .collect();
        let mut pairs: Vec<(u32, u64, usize, usize)> = Vec::new();
        for (index, (note, &(on, _))) in read.notes.iter().zip(&times).enumerate() {
            let from = onsets.partition_point(|onset| onset.at < on - SAME_MOMENT_MS);
            for (at, onset) in onsets.iter().enumerate().skip(from) {
                if onset.at > on + SAME_MOMENT_MS {
                    break;
                }
                if onset.staff != note.track {
                    continue;
                }
                let pitch = onset
                    .key
                    .map_or(u32::from(u8::MAX), |key| u32::from(key.abs_diff(note.key)));
                let apart = ((onset.at - on).abs() * 1e3) as u64;
                pairs.push((pitch, apart, index, at));
            }
        }
        pairs.sort_unstable();
        let mut element_of_note: Vec<Option<u32>> = vec![None; read.notes.len()];
        let mut taken = vec![false; onsets.len()];
        for (_, _, note, onset) in pairs {
            if element_of_note[note].is_none() && !taken[onset] {
                element_of_note[note] = Some(onsets[onset].element);
                taken[onset] = true;
            }
        }

        let notes = read
            .notes
            .iter()
            .zip(&times)
            .zip(element_of_note)
            .map(|((note, &(on_ms, off_ms)), element)| {
                let part = staves
                    .binary_search(&note.track)
                    .expect("every note's staff is listed");
                Note {
                    key: note.key,
                    velocity: note.velocity,
                    on_ms,
                    off_ms,
                    hand: parts[part].hand.hand(note.key),
                    part: part as u16,
                    measure: 0,
                    element,
                }
            })
            .collect();
        let measures = events
            .iter()
            .filter(|event| event.measure_on.is_some())
            .map(|event| event.tstamp)
            .collect();
        let duration = events.last().map_or(0., |event| event.tstamp);
        Ok(Self::with_parts(notes, measures, duration, parts, elements))
    }
}

/// An engraved note starting at a moment of the timemap.
struct Onset {
    at: f64,
    staff: usize,
    key: Option<u8>,
    element: u32,
}

/// The hand of the staff at `index` of `count`: the upper half of the staves is the right hand
/// (the middle one too, when there is one); a lone staff is the hand its notes lie under.
fn staff_hand(index: usize, count: usize, mean_key: f64) -> PartHand {
    match count {
        1 if mean_key >= f64::from(MIDDLE_C) => PartHand::Right,
        1 => PartHand::Left,
        _ if index < count.div_ceil(2) => PartHand::Right,
        _ => PartHand::Left,
    }
}

/// The hand a part's name says, if it says one ("Piano RH", "Left hand", "Bass").
fn named_hand(name: &str) -> Option<PartHand> {
    let lower = name.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    // "R.H." is two words.
    let pairs: Vec<String> = words.windows(2).map(|pair| pair.concat()).collect();
    let says = |names: &[&str]| {
        words.iter().any(|word| names.contains(word))
            || pairs.iter().any(|pair| names.contains(&pair.as_str()))
    };
    if says(&["right", "rh", "treble"]) {
        Some(PartHand::Right)
    } else if says(&["left", "lh", "bass"]) {
        Some(PartHand::Left)
    } else {
        None
    }
}

/// The hands of a MIDI file's parts, from their names when they say one, from their average
/// keys otherwise (see [`Timeline::from_smf`]).
fn guess_hands(names: &[Option<String>], means: &[f64]) -> Vec<PartHand> {
    if means.len() == 1 {
        return vec![
            names[0]
                .as_deref()
                .and_then(named_hand)
                .unwrap_or(PartHand::Split),
        ];
    }
    let mut order: Vec<usize> = (0..means.len()).collect();
    order.sort_by(|&a, &b| means[a].total_cmp(&means[b]));
    // The rank in `order` the right hand starts at: after the widest gap.
    let split = (1..order.len())
        .max_by(|&a, &b| {
            let gap = |i: usize| means[order[i]] - means[order[i - 1]];
            gap(a).total_cmp(&gap(b))
        })
        .unwrap_or(0);
    (0..means.len())
        .map(|part| {
            let by_pitch = match order.iter().position(|&p| p == part) {
                Some(rank) if rank >= split => PartHand::Right,
                _ => PartHand::Left,
            };
            names[part]
                .as_deref()
                .and_then(named_hand)
                .unwrap_or(by_pitch)
        })
        .collect()
}

/// A Standard MIDI File's notes, by track and channel, with what times them.
struct Read {
    notes: Vec<Raw>,
    tempo: TempoMap,
    ticks_per_beat: u64,
    signatures: Vec<(u64, u32, u32)>,
    end_tick: u64,
    /// Each track's name, by its index in the file.
    names: HashMap<usize, String>,
}

struct Raw {
    /// Its index in the file.
    track: usize,
    channel: u8,
    key: u8,
    velocity: u8,
    start: u64,
    end: u64,
}

impl Read {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let smf = Smf::parse(bytes).map_err(|e| format!("not a MIDI file: {e}"))?;
        let Timing::Metrical(ticks_per_beat) = smf.header.timing else {
            return Err("MIDI files timed in SMPTE frames are not supported".into());
        };
        let ticks_per_beat = u64::from(ticks_per_beat.as_int());
        if ticks_per_beat == 0 {
            return Err("MIDI file with no ticks per beat".into());
        }

        // Format 2 holds independent songs: only the first is read.
        let tracks = match smf.header.format {
            Format::Sequential => &smf.tracks[..smf.tracks.len().min(1)],
            _ => &smf.tracks[..],
        };

        let mut tempi: Vec<(u64, u32)> = Vec::new();
        let mut signatures: Vec<(u64, u32, u32)> = Vec::new();
        let mut notes: Vec<Raw> = Vec::new();
        let mut names = HashMap::new();
        let mut end_tick = 0;
        for (index, track) in tracks.iter().enumerate() {
            let mut tick = 0u64;
            let mut open: Vec<(u8, u8, u64, u8)> = Vec::new(); // (channel, key, tick, velocity)
            for event in track {
                tick += u64::from(event.delta.as_int());
                match event.kind {
                    TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                        tempi.push((tick, micros.as_int().max(1)));
                    }
                    // A power above 15 would overflow, and no music needs it.
                    TrackEventKind::Meta(MetaMessage::TimeSignature(num, power, _, _))
                        if num > 0 && power <= 15 =>
                    {
                        signatures.push((tick, u32::from(num), 1 << power));
                    }
                    TrackEventKind::Meta(MetaMessage::TrackName(name)) => {
                        let name = String::from_utf8_lossy(name).trim().to_owned();
                        if !name.is_empty() {
                            names.entry(index).or_insert(name);
                        }
                    }
                    TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::NoteOn { key, vel },
                    } if vel.as_int() > 0 && channel.as_int() != 9 => {
                        open.push((channel.as_int(), key.as_int(), tick, vel.as_int()));
                    }
                    TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::NoteOff { key, .. } | MidiMessage::NoteOn { key, .. },
                    } => {
                        // The oldest note still sounding on this key ends.
                        let found = open
                            .iter()
                            .position(|o| o.0 == channel.as_int() && o.1 == key.as_int());
                        if let Some(at) = found {
                            let (channel, key, start, velocity) = open.remove(at);
                            notes.push(Raw {
                                track: index,
                                channel,
                                key,
                                velocity,
                                start,
                                end: tick,
                            });
                        }
                    }
                    _ => {}
                }
            }
            // A note never released ends with its track.
            for (channel, key, start, velocity) in open {
                notes.push(Raw {
                    track: index,
                    channel,
                    key,
                    velocity,
                    start,
                    end: tick,
                });
            }
            end_tick = end_tick.max(tick);
        }
        notes.retain(|note| note.end > note.start);
        Ok(Self {
            notes,
            tempo: TempoMap::new(ticks_per_beat, tempi),
            ticks_per_beat,
            signatures,
            end_tick,
            names,
        })
    }
}

/// Ticks to milliseconds, through the tempo changes.
struct TempoMap {
    ticks_per_beat: u64,
    /// `(tick, ms at that tick, microseconds per beat from it)`, by tick.
    segments: Vec<(u64, f64, u32)>,
}

impl TempoMap {
    fn new(ticks_per_beat: u64, mut tempi: Vec<(u64, u32)>) -> Self {
        tempi.sort_by_key(|&(tick, _)| tick);
        let mut segments = vec![(0, 0., 500_000)]; // 120 beats a minute until told otherwise
        for (tick, micros) in tempi {
            let &(from, ms, current) = segments.last().expect("one segment at least");
            let at = ms + (tick - from) as f64 * f64::from(current) / ticks_per_beat as f64 / 1e3;
            if tick == from {
                segments.pop();
            }
            segments.push((tick, at, micros));
        }
        Self {
            ticks_per_beat,
            segments,
        }
    }

    fn ms(&self, tick: u64) -> f64 {
        let at = self.segments.partition_point(|s| s.0 <= tick) - 1;
        let (from, ms, micros) = self.segments[at];
        ms + (tick - from) as f64 * f64::from(micros) / self.ticks_per_beat as f64 / 1e3
    }
}

/// More measures than any piece has: a file that asks for more (a tiny measure over a long
/// silence) is refused rather than filling the memory.
const MAX_MEASURES: usize = 100_000;

/// The tick each measure starts at, up to the one holding `end`. A time signature takes effect at
/// the barline that follows it, as scores are written.
fn measure_ticks(
    ticks_per_beat: u64,
    mut signatures: Vec<(u64, u32, u32)>,
    end: u64,
) -> Result<Vec<u64>, String> {
    signatures.sort_by_key(|&(tick, ..)| tick);
    let (mut num, mut den) = (4, 4);
    let mut next_signature = 0;
    let mut starts = vec![0];
    let mut tick = 0;
    loop {
        while let Some(&(at, n, d)) = signatures.get(next_signature)
            && at <= tick
        {
            (num, den) = (n, d);
            next_signature += 1;
        }
        tick += (ticks_per_beat * 4 * u64::from(num) / u64::from(den)).max(1);
        if tick >= end {
            return Ok(starts);
        }
        if starts.len() >= MAX_MEASURES {
            return Err(format!("more than {MAX_MEASURES} measures"));
        }
        starts.push(tick);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use midly::num::{u4, u7, u15, u24, u28};
    use midly::{Header, TrackEvent};

    use super::*;

    const PPQ: u16 = 480;

    fn meta(delta: u32, message: MetaMessage<'static>) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Meta(message),
        }
    }

    fn note(delta: u32, key: u8, on: bool) -> TrackEvent<'static> {
        note_on_channel(delta, key, on, 0)
    }

    fn note_on_channel(delta: u32, key: u8, on: bool, channel: u8) -> TrackEvent<'static> {
        let (key, vel) = (u7::new(key), u7::new(if on { 80 } else { 0 }));
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Midi {
                channel: u4::new(channel),
                message: if on {
                    MidiMessage::NoteOn { key, vel }
                } else {
                    MidiMessage::NoteOff { key, vel }
                },
            },
        }
    }

    fn named(name: &'static str) -> TrackEvent<'static> {
        meta(0, MetaMessage::TrackName(name.as_bytes()))
    }

    fn smf(tracks: Vec<Vec<TrackEvent<'static>>>) -> Vec<u8> {
        let mut smf = Smf::new(Header::new(
            Format::Parallel,
            Timing::Metrical(u15::new(PPQ)),
        ));
        smf.tracks = tracks;
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).expect("a valid file");
        bytes
    }

    fn tempo(micros: u32) -> MetaMessage<'static> {
        MetaMessage::Tempo(u24::new(micros))
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Two measures of 4/4 at 120 beats a minute (2 s each): a quarter-note C on each beat of the
    /// first, and a whole-note A in the second, then a left-hand track with a whole-note C.
    fn two_measures() -> Vec<u8> {
        let beat = u32::from(PPQ);
        let mut right = vec![
            meta(0, tempo(500_000)),
            meta(0, MetaMessage::TimeSignature(4, 2, 24, 8)),
        ];
        for _ in 0..4 {
            right.push(note(0, 72, true));
            right.push(note(beat, 72, false));
        }
        right.push(note(0, 81, true));
        right.push(note(beat * 4, 81, false));
        let left = vec![note(0, 48, true), note(beat * 8, 48, false)];
        smf(vec![right, left])
    }

    fn hands_of(timeline: &Timeline) -> Vec<(u8, Hand)> {
        timeline.notes().iter().map(|n| (n.key, n.hand)).collect()
    }

    #[test]
    fn notes_get_their_time_at_the_pieces_own_tempo() {
        let timeline = Timeline::from_smf(&two_measures()).unwrap();
        let right: Vec<_> = timeline
            .notes()
            .iter()
            .filter(|n| n.hand == Hand::Right)
            .collect();
        assert_eq!(right.len(), 5);
        assert_eq!(right[0].on_ms, 0.);
        assert_eq!(right[0].off_ms, 500.);
        assert_eq!(right[3].on_ms, 1500.);
        assert_eq!(
            (right[4].key, right[4].on_ms, right[4].off_ms),
            (81, 2000., 4000.)
        );
        assert_eq!(timeline.duration_ms(), 4000.);
    }

    #[test]
    fn each_note_knows_its_measure() {
        let timeline = Timeline::from_smf(&two_measures()).unwrap();
        let measures: Vec<_> = timeline
            .notes()
            .iter()
            .map(|n| (n.key, n.measure))
            .collect();
        assert_eq!(
            measures,
            [(48, 0), (72, 0), (72, 0), (72, 0), (72, 0), (81, 1)]
        );
    }

    #[test]
    fn of_two_tracks_the_higher_is_the_right_hand_whatever_their_order() {
        let timeline = Timeline::from_smf(&two_measures()).unwrap();
        assert_eq!(
            timeline.parts(),
            [
                Part {
                    name: "Track 1".into(),
                    hand: PartHand::Right
                },
                Part {
                    name: "Track 2".into(),
                    hand: PartHand::Left
                }
            ]
        );
        // The left hand first in the file.
        let beat = u32::from(PPQ);
        let bytes = smf(vec![
            vec![note(0, 40, true), note(beat, 40, false)],
            vec![note(0, 76, true), note(beat, 76, false)],
        ]);
        let timeline = Timeline::from_smf(&bytes).unwrap();
        assert_eq!(hands_of(&timeline), [(40, Hand::Left), (76, Hand::Right)]);
    }

    /// `three-voices.mid`: a conductor track, then a bass, a melody and an inner voice, none
    /// named: the bass is the left hand, the two others the right.
    #[test]
    fn three_tracks_are_split_where_their_keys_are_furthest_apart() {
        let bytes = std::fs::read(fixture("three-voices.mid")).unwrap();
        let timeline = Timeline::from_smf(&bytes).unwrap();
        let parts: Vec<_> = timeline
            .parts()
            .iter()
            .map(|p| (p.name.as_str(), p.hand))
            .collect();
        assert_eq!(
            parts,
            [
                ("Track 1", PartHand::Left),
                ("Track 2", PartHand::Right),
                ("Track 3", PartHand::Right)
            ]
        );
        for note in timeline.notes() {
            let wanted = if note.part == 0 {
                Hand::Left
            } else {
                Hand::Right
            };
            assert_eq!(note.hand, wanted, "{note:?}");
        }
        assert_eq!(timeline.measure_count(), 2);
    }

    #[test]
    fn a_track_named_for_a_hand_is_that_hand() {
        let beat = u32::from(PPQ);
        // Named against their keys: the names win.
        let bytes = smf(vec![
            vec![
                named("Piano L.H."),
                note(0, 76, true),
                note(beat, 76, false),
            ],
            vec![
                named("Right hand"),
                note(0, 40, true),
                note(beat, 40, false),
            ],
        ]);
        let timeline = Timeline::from_smf(&bytes).unwrap();
        assert_eq!(timeline.hands(), [PartHand::Left, PartHand::Right]);
        assert_eq!(timeline.parts()[1].name, "Right hand");
        assert_eq!(named_hand("Copyright 2024"), None);
        assert_eq!(named_hand("Bass"), Some(PartHand::Left));
        assert_eq!(named_hand("RH"), Some(PartHand::Right));
    }

    #[test]
    fn the_channels_of_one_track_are_parts_of_their_own() {
        let beat = u32::from(PPQ);
        let bytes = smf(vec![vec![
            note_on_channel(0, 74, true, 0),
            note_on_channel(0, 50, true, 1),
            note_on_channel(beat, 74, false, 0),
            note_on_channel(0, 50, false, 1),
        ]]);
        let timeline = Timeline::from_smf(&bytes).unwrap();
        let names: Vec<_> = timeline.parts().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Track 1, channel 1", "Track 1, channel 2"]);
        assert_eq!(hands_of(&timeline), [(50, Hand::Left), (74, Hand::Right)]);
    }

    #[test]
    fn a_single_part_is_split_at_the_middle_c() {
        let bytes = smf(vec![vec![
            note(0, 72, true),
            note(100, 72, false),
            note(0, 40, true),
            note(100, 40, false),
        ]]);
        let timeline = Timeline::from_smf(&bytes).unwrap();
        assert_eq!(timeline.hands(), [PartHand::Split]);
        assert_eq!(hands_of(&timeline), [(72, Hand::Right), (40, Hand::Left)]);
    }

    #[test]
    fn the_player_gives_the_parts_other_hands() {
        let timeline = Timeline::from_smf(&two_measures()).unwrap();
        let swapped = timeline.with_hands(&[PartHand::Left, PartHand::Split]);
        assert_eq!(swapped.hands(), [PartHand::Left, PartHand::Split]);
        for note in swapped.notes() {
            let wanted = match (note.part, note.key) {
                (0, _) => Hand::Left,
                (_, key) if key >= 60 => Hand::Right,
                _ => Hand::Left,
            };
            assert_eq!(note.hand, wanted, "{note:?}");
        }
        // A list that does not fit the parts changes nothing.
        assert_eq!(timeline.with_hands(&[PartHand::Left]), timeline);
    }

    #[test]
    fn measures_follow_the_time_signature() {
        let timeline = Timeline::from_smf(&two_measures()).unwrap();
        assert_eq!(timeline.measure_count(), 2);
        assert_eq!(timeline.measure_start_ms(0), Some(0.));
        assert_eq!(timeline.measure_start_ms(1), Some(2000.));
        assert_eq!(timeline.measure_start_ms(2), None);

        // 3/4 then 6/8 at 120 beats a minute: every measure lasts 1.5 s, and the 6/8 note
        // (six quarters long) covers two of them.
        let beat = u32::from(PPQ);
        let bytes = smf(vec![vec![
            meta(0, MetaMessage::TimeSignature(3, 2, 24, 8)),
            note(0, 60, true),
            note(beat * 3, 60, false),
            meta(0, MetaMessage::TimeSignature(6, 3, 24, 8)),
            note(0, 62, true),
            note(beat * 6, 62, false),
        ]]);
        let timeline = Timeline::from_smf(&bytes).unwrap();
        let starts: Vec<_> = (0..timeline.measure_count())
            .map(|i| timeline.measure_start_ms(i).unwrap())
            .collect();
        assert_eq!(starts, [0., 1500., 3000.]);
    }

    #[test]
    fn a_tempo_change_moves_what_follows_it() {
        let beat = u32::from(PPQ);
        let bytes = smf(vec![vec![
            meta(0, tempo(500_000)),
            note(0, 60, true),
            note(beat * 4, 60, false),
            meta(0, tempo(1_000_000)),
            note(0, 62, true),
            note(beat * 4, 62, false),
        ]]);
        let timeline = Timeline::from_smf(&bytes).unwrap();
        let second = timeline.notes()[1];
        assert_eq!((second.on_ms, second.off_ms), (2000., 6000.));
        assert_eq!(timeline.measure_start_ms(1), Some(2000.));
    }

    #[test]
    fn measure_at_finds_the_measure_holding_a_time() {
        let timeline = Timeline::new(vec![], vec![0., 2000., 4000.], 6000.);
        assert_eq!(timeline.measure_at(0.), 0);
        assert_eq!(timeline.measure_at(1999.9), 0);
        assert_eq!(timeline.measure_at(2000.), 1);
        assert_eq!(timeline.measure_at(1e9), 2);
        assert_eq!(timeline.measure_at(-5.), 0);
    }

    #[test]
    fn a_file_with_a_hostile_number_of_measures_is_refused() {
        // 255/32768 time: a measure of 14 ticks, over two million ticks of one note.
        let bytes = smf(vec![vec![
            meta(0, MetaMessage::TimeSignature(255, 15, 24, 8)),
            note(0, 60, true),
            note(2_000_000, 60, false),
        ]]);
        let error = Timeline::from_smf(&bytes).unwrap_err();
        assert!(error.contains("measures"), "{error}");
    }

    #[test]
    fn what_is_not_a_midi_file_is_refused() {
        assert!(Timeline::from_smf(b"not midi").is_err());
    }

    #[test]
    fn a_written_pitch_is_a_key() {
        assert_eq!(Written::key("c", "4"), Some(60));
        assert_eq!(Written::key("b", "3"), Some(59));
        assert_eq!(Written::key("a", "0"), Some(21));
        assert_eq!(Written::key("h", "4"), None);
        assert_eq!(Written::key("c", "loud"), None);
        assert_eq!(Written::key("g", "10"), None);
    }

    fn event(at: f64, on: &[&str], measure: Option<&str>) -> Event {
        Event {
            tstamp: at,
            on: on.iter().map(|id| id.to_string()).collect(),
            off: Vec::new(),
            measure_on: measure.map(str::to_owned),
        }
    }

    /// A score as Verovio would give it: a measure played twice, its notes named by their
    /// engraved ids both times; a note tied from before that starts in the timemap but not in
    /// the MIDI; a sharp written F.
    #[test]
    fn each_note_of_the_midi_finds_its_engraved_note() {
        let beat = u32::from(PPQ);
        let conductor = vec![meta(0, tempo(500_000))];
        // Staff 1: the chord C5 E5, then F#5 in the repeat; staff 2: a C3 under both.
        let upper = vec![
            note(0, 72, true),
            note(0, 76, true),
            note(beat, 72, false),
            note(0, 76, false),
            note(beat, 72, true),
            note(0, 78, true),
            note(beat, 72, false),
            note(0, 78, false),
        ];
        let lower = vec![note(0, 48, true), note(beat * 4, 48, false)];
        let midi = smf(vec![conductor, upper, lower]);
        let events = vec![
            event(0., &["c5", "e5", "tied-d5", "c3"], Some("m1")),
            event(1000., &["c5", "f5"], Some("m1")),
            event(2000., &[], None),
        ];
        let written = HashMap::from([
            ("c5".to_owned(), (1, 72)),
            ("e5".to_owned(), (1, 76)),
            ("tied-d5".to_owned(), (1, 74)),
            ("f5".to_owned(), (1, 77)),
            ("c3".to_owned(), (2, 48)),
        ])
        .into_iter()
        .map(|(id, (staff, key))| {
            (
                id,
                Written {
                    staff,
                    key: Some(key),
                },
            )
        })
        .collect();
        let timeline = Timeline::from_engraved(&midi, &events, &written).unwrap();
        let found: Vec<_> = timeline
            .notes()
            .iter()
            .map(|n| (n.key, n.on_ms, timeline.element(n), n.hand, n.measure))
            .collect();
        assert_eq!(
            found,
            [
                (48, 0., Some("c3"), Hand::Left, 0),
                (72, 0., Some("c5"), Hand::Right, 0),
                (76, 0., Some("e5"), Hand::Right, 0),
                (72, 1000., Some("c5"), Hand::Right, 1),
                (78, 1000., Some("f5"), Hand::Right, 1),
            ]
        );
        let names: Vec<_> = timeline.parts().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Staff 1", "Staff 2"]);
        assert_eq!(timeline.measure_count(), 2);
        assert_eq!(timeline.duration_ms(), 2000.);
    }

    #[test]
    fn the_staves_give_the_hands() {
        assert_eq!(staff_hand(0, 2, 60.), PartHand::Right);
        assert_eq!(staff_hand(1, 2, 60.), PartHand::Left);
        // A voice above the piano: the voice and the upper piano staff are the right hand.
        assert_eq!(staff_hand(1, 3, 60.), PartHand::Right);
        assert_eq!(staff_hand(2, 3, 60.), PartHand::Left);
        assert_eq!(staff_hand(0, 1, 50.), PartHand::Left);
        assert_eq!(staff_hand(0, 1, 67.), PartHand::Right);
    }

    /// Verovio on the score's fixture: 108 measures over three pages, the first eight repeated.
    #[test]
    fn every_note_of_a_long_piece_is_found_engraved() {
        let cache = tempfile::tempdir().unwrap();
        let engraver = crate::engraver::Engraver::start(Some(cache.path().join("gamme")));
        let engraved = engraver
            .engrave(&fixture("exercise-in-c.musicxml"))
            .unwrap();
        let timeline =
            Timeline::from_engraved(&engraved.midi, &engraved.events, &engraved.written).unwrap();
        assert_eq!(timeline.measure_count(), 116);
        assert!(timeline.notes().len() > 500, "{}", timeline.notes().len());
        let lost: Vec<_> = timeline
            .notes()
            .iter()
            .filter(|n| n.element.is_none())
            .collect();
        assert!(lost.is_empty(), "{lost:?}");
        assert_eq!(timeline.hands(), [PartHand::Right, PartHand::Left]);
    }

    /// Verovio itself, on `round-and-back.musicxml`: measures 1 and 2 repeated, then 3, at 120
    /// quarters a minute; a chord and a sharp in the right hand, a tie in the left.
    #[test]
    fn a_repeat_plays_the_same_engraved_notes_again() {
        let cache = tempfile::tempdir().unwrap();
        let engraver = crate::engraver::Engraver::start(Some(cache.path().join("gamme")));
        let engraved = engraver
            .engrave(&fixture("round-and-back.musicxml"))
            .unwrap();
        let timeline =
            Timeline::from_engraved(&engraved.midi, &engraved.events, &engraved.written).unwrap();

        let starts: Vec<f64> = (0..timeline.measure_count())
            .map(|i| timeline.measure_start_ms(i).unwrap())
            .collect();
        assert_eq!(starts, [0., 2000., 4000., 6000., 8000.]);
        assert_eq!(timeline.duration_ms(), 10_000.);
        // Measure 1 has six notes, measure 2 four, both played twice; measure 3 has two.
        assert_eq!(
            timeline.notes().len(),
            2 * (6 + 4) + 2,
            "{:?}",
            timeline.notes()
        );
        assert!(timeline.notes().iter().all(|n| n.element.is_some()));

        // What a measure plays: its keys, when in it, and which engraved notes.
        let played = |measure: u32| {
            let start = timeline.measure_start_ms(measure as usize).unwrap();
            let mut notes: Vec<(u8, i64, String)> = timeline
                .notes()
                .iter()
                .filter(|n| n.measure == measure)
                .map(|n| {
                    let at = (n.on_ms - start).round() as i64;
                    (n.key, at, timeline.element(n).unwrap().to_owned())
                })
                .collect();
            notes.sort();
            notes
        };
        assert_eq!(played(0), played(2), "the repeat of measure 1");
        assert_eq!(played(1), played(3), "the repeat of measure 2");
        assert_ne!(played(0), played(1));
        let keys = |measure| played(measure).iter().map(|n| n.0).collect::<Vec<_>>();
        // C3 tied: one note; the chord; the sharp.
        assert_eq!(keys(0), [43, 48, 72, 74, 76, 79]);
        assert_eq!(keys(1), [43, 72, 78, 79]);
        assert_eq!(keys(4), [48, 72]);
        let tied = timeline
            .notes()
            .iter()
            .find(|n| n.key == 48 && n.measure == 0)
            .unwrap();
        assert!(
            (tied.off_ms - tied.on_ms - 1500.).abs() < 10.,
            "the tie is one note: {tied:?}"
        );
        // Each engraved note of a measure is a note of its own.
        let elements: std::collections::HashSet<_> = played(0).into_iter().map(|n| n.2).collect();
        assert_eq!(elements.len(), 6);

        // One staff, one hand.
        let names: Vec<_> = timeline.parts().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Staff 1", "Staff 2"]);
        for note in timeline.notes() {
            let wanted = if note.key >= 72 {
                Hand::Right
            } else {
                Hand::Left
            };
            assert_eq!(note.hand, wanted, "{note:?}");
        }
    }
}
