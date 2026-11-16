//! The notes of a piece in time, and where its measures start.
//!
//! Times are milliseconds of the piece **at its own tempo** (speed 100 %): the clock (`clock.rs`)
//! scales real time into them, so nothing here knows the speed.
//!
//! For now a timeline is read from a Standard MIDI File ([`Timeline::from_smf`]), the format
//! Verovio's MIDI output has too: one track per staff, the tempo and the time signatures of the
//! score. Pieces from the library (MusicXML) will arrive through Verovio's MIDI, so the engraved
//! score and this timeline come from the same source; [`Timeline::new`] takes the notes and the
//! measure starts already computed, should another source fit better.

use midly::{Format, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

/// Which hand plays a note: the upper staff is the right hand, the lower one the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hand {
    Right,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// MIDI key, 0–127 (60 is the middle C).
    pub key: u8,
    pub velocity: u8,
    pub on_ms: f64,
    pub off_ms: f64,
    pub hand: Hand,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    /// Sorted by `on_ms`.
    notes: Vec<Note>,
    /// Start of each measure, in order; never empty, the first is 0.
    measures: Vec<f64>,
    duration_ms: f64,
}

impl Timeline {
    /// `notes` in any order, `measures` the start of each measure (a measure starting at 0 is
    /// added when none does), `duration_ms` at least as long as the last note.
    pub fn new(mut notes: Vec<Note>, mut measures: Vec<f64>, duration_ms: f64) -> Self {
        notes.sort_by(|a, b| a.on_ms.total_cmp(&b.on_ms));
        measures.sort_by(f64::total_cmp);
        if measures.first().is_none_or(|&first| first > 0.) {
            measures.insert(0, 0.);
        }
        let last = notes.iter().map(|note| note.off_ms).fold(0., f64::max);
        Self {
            notes,
            measures,
            duration_ms: duration_ms.max(last),
        }
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

    /// Reads a Standard MIDI File.
    ///
    /// - Tempo changes and time signatures of every track count (the measures follow them).
    /// - The tracks that hold notes are the staves: the first is the right hand, the next ones
    ///   the left; a file with one such track (format 0, a piano on one track) is split at the
    ///   middle C. Verovio's MIDI has one track per staff, upper first.
    /// - The pedal is ignored.
    pub fn from_smf(bytes: &[u8]) -> Result<Self, String> {
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
        let mut raw: Vec<Raw> = Vec::new();
        let mut note_tracks = 0;
        let mut end_tick = 0;
        for track in tracks {
            let (mut tick, mut has_notes) = (0u64, false);
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
                            let (_, key, start, velocity) = open.remove(at);
                            raw.push(Raw {
                                track: note_tracks,
                                key,
                                velocity,
                                start,
                                end: tick,
                            });
                            has_notes = true;
                        }
                    }
                    _ => {}
                }
            }
            // A note never released ends with its track.
            for (_, key, start, velocity) in open {
                raw.push(Raw {
                    track: note_tracks,
                    key,
                    velocity,
                    start,
                    end: tick,
                });
                has_notes = true;
            }
            end_tick = end_tick.max(tick);
            if has_notes {
                note_tracks += 1;
            }
        }
        raw.retain(|note| note.end > note.start);

        let map = TempoMap::new(ticks_per_beat, tempi);
        let notes = raw
            .iter()
            .map(|note| Note {
                key: note.key,
                velocity: note.velocity,
                on_ms: map.ms(note.start),
                off_ms: map.ms(note.end),
                hand: if note_tracks == 1 {
                    if note.key >= 60 {
                        Hand::Right
                    } else {
                        Hand::Left
                    }
                } else if note.track == 0 {
                    Hand::Right
                } else {
                    Hand::Left
                },
            })
            .collect();

        // The piece ends with its last note, not with a track's trailing silence.
        let last_note_tick = raw.iter().map(|note| note.end).max().unwrap_or(0);
        let measures = measure_ticks(ticks_per_beat, signatures, last_note_tick.max(1))?
            .into_iter()
            .map(|tick| map.ms(tick))
            .collect();
        Ok(Self::new(notes, measures, map.ms(end_tick)))
    }
}

struct Raw {
    track: usize,
    key: u8,
    velocity: u8,
    start: u64,
    end: u64,
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
pub(crate) mod tests {
    use midly::num::{u4, u7, u15, u24, u28};
    use midly::{Header, TrackEvent};

    use super::*;

    pub const PPQ: u16 = 480;

    pub fn meta(delta: u32, message: MetaMessage<'static>) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Meta(message),
        }
    }

    pub fn note(delta: u32, key: u8, on: bool) -> TrackEvent<'static> {
        let (key, vel) = (u7::new(key), u7::new(if on { 80 } else { 0 }));
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Midi {
                channel: u4::new(0),
                message: if on {
                    MidiMessage::NoteOn { key, vel }
                } else {
                    MidiMessage::NoteOff { key, vel }
                },
            },
        }
    }

    pub fn smf(tracks: Vec<Vec<TrackEvent<'static>>>) -> Vec<u8> {
        let mut smf = Smf::new(Header::new(
            Format::Parallel,
            Timing::Metrical(u15::new(PPQ)),
        ));
        smf.tracks = tracks;
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).expect("a valid file");
        bytes
    }

    pub fn tempo(micros: u32) -> MetaMessage<'static> {
        MetaMessage::Tempo(u24::new(micros))
    }

    /// Two measures of 4/4 at 120 beats a minute (2 s each): a quarter-note C on each beat of the
    /// first, and a whole-note A in the second, then a left-hand track with a whole-note C.
    pub fn two_measures() -> Vec<u8> {
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
    fn the_first_track_is_the_right_hand_and_the_next_the_left() {
        let timeline = Timeline::from_smf(&two_measures()).unwrap();
        let left: Vec<_> = timeline
            .notes()
            .iter()
            .filter(|n| n.hand == Hand::Left)
            .collect();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].key, 48);
    }

    #[test]
    fn a_single_track_is_split_at_the_middle_c() {
        let bytes = smf(vec![vec![
            note(0, 72, true),
            note(100, 72, false),
            note(0, 40, true),
            note(100, 40, false),
        ]]);
        let timeline = Timeline::from_smf(&bytes).unwrap();
        let hands: Vec<_> = timeline.notes().iter().map(|n| (n.key, n.hand)).collect();
        assert_eq!(hands, [(72, Hand::Right), (40, Hand::Left)]);
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
}
