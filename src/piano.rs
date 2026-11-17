//! The built-in piano: a SoundFont synthesizer on the default audio output, playing the notes of
//! a piece as its clock (`clock.rs`) reaches them.
//!
//! The audio callback never blocks, locks or allocates: the notes were turned into a sorted list
//! of events before it started, the clock and the hands are atomics, and the synthesizer's buffers
//! exist since it was made. What the callback does to the clock is the only thing the window
//! sees of it.
//!
//! [`Renderer`] is the whole of the callback's logic, over a [`Sink`] instead of a synthesizer
//! and a device, so that it is tested without either. [`start`] puts it on the output.
//!
//! The keys the player strikes on a MIDI keyboard (`midi_in.rs`) reach the callback through
//! [`LiveKeys`], a ring of atomics made once: the callback plays them at the start of its next
//! buffer, so the output is opened with a small one (`LIVE_FRAMES`). They are counted apart from
//! the piece's: the piece ending a note, a pause or a seek do not cut a key the player holds, and
//! the player letting a key go does not cut the piece's note on it.

use std::sync::atomic::Ordering::{Acquire, Release};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::clock::Clock;
use crate::timeline::{Hand, Timeline};

/// The largest chunk the callback renders at once; a longer buffer is rendered in several.
const MAX_FRAMES: usize = 4096;
/// The channel of a MIDI synthesizer the piano is on.
const CHANNEL: i32 = 0;
/// The buffer the output is asked for, in frames (5.3 ms at 48 kHz): a key struck sounds at the
/// next buffer. Smaller would gain little and risk gaps on a busy machine; the device's own
/// range has the last word.
const LIVE_FRAMES: u32 = 256;
/// How many keys struck and let go can wait for the callback: hundreds of buffers' worth.
const LIVE_SLOTS: usize = 256;

/// The keys the player strikes, on their way to the audio callback: a ring of atomics, written
/// on MIDI's threads and read by the callback, which neither locks nor waits. Clones share it.
#[derive(Clone)]
pub struct LiveKeys(Arc<Ring>);

struct Ring {
    /// Each a key and its velocity (0: let go), `key << 8 | velocity`.
    slots: [AtomicU16; LIVE_SLOTS],
    /// How many were ever written, and read: a slot is free once read.
    written: AtomicUsize,
    read: AtomicUsize,
    /// The writers take turns (several keyboards); the reader never takes it.
    writing: Mutex<()>,
}

impl Default for LiveKeys {
    fn default() -> Self {
        Self(Arc::new(Ring {
            slots: std::array::from_fn(|_| AtomicU16::new(0)),
            written: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            writing: Mutex::new(()),
        }))
    }
}

impl LiveKeys {
    /// A key struck (`velocity` above 0) or let go. Not for the audio callback. A ring that is
    /// full (no output reads it) drops it: what is not heard now is not to be heard later.
    pub fn push(&self, key: u8, velocity: u8) -> bool {
        let ring = &*self.0;
        let _turn = ring.writing.lock().expect("the live keys' lock");
        let written = ring.written.load(Relaxed);
        if written.wrapping_sub(ring.read.load(Acquire)) >= LIVE_SLOTS {
            return false;
        }
        let packed = u16::from(key & 127) << 8 | u16::from(velocity & 127);
        ring.slots[written % LIVE_SLOTS].store(packed, Relaxed);
        ring.written.store(written.wrapping_add(1), Release);
        true
    }

    /// The next key waiting, for the callback: no lock, no allocation.
    fn pop(&self) -> Option<(u8, u8)> {
        let ring = &*self.0;
        let read = ring.read.load(Relaxed);
        if read == ring.written.load(Acquire) {
            return None;
        }
        let packed = ring.slots[read % LIVE_SLOTS].load(Relaxed);
        ring.read.store(read.wrapping_add(1), Release);
        Some(((packed >> 8) as u8, (packed & 0xFF) as u8))
    }

    /// Forgets what waits: struck before the output was there, it is not played late.
    fn discard(&self) {
        let ring = &*self.0;
        ring.read.store(ring.written.load(Acquire), Release);
    }
}

/// Which hands the computer plays: the other ones are the player's.
#[derive(Clone)]
pub struct Hands(Arc<AtomicU8>);

impl Hands {
    const RIGHT: u8 = 1;
    const LEFT: u8 = 2;

    /// The computer plays both hands (listening to the piece).
    pub fn both() -> Self {
        Self(Arc::new(AtomicU8::new(Self::RIGHT | Self::LEFT)))
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the choice of hands comes with the hands setting (#15)"
        )
    )]
    pub fn set(&self, right: bool, left: bool) {
        let bits = (u8::from(right) * Self::RIGHT) | (u8::from(left) * Self::LEFT);
        self.0.store(bits, Relaxed);
    }

    fn plays(&self, hand: Hand) -> bool {
        let bit = match hand {
            Hand::Right => Self::RIGHT,
            Hand::Left => Self::LEFT,
        };
        self.0.load(Relaxed) & bit != 0
    }
}

/// What the renderer plays on: a synthesizer.
pub trait Sink {
    fn note_on(&mut self, key: u8, velocity: u8);
    fn note_off(&mut self, key: u8);
    /// Lets every sounding note go, with its release.
    fn release_all(&mut self);
    /// Fills both channels; they have the same length.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
}

impl Sink for Synthesizer {
    fn note_on(&mut self, key: u8, velocity: u8) {
        Synthesizer::note_on(self, CHANNEL, i32::from(key), i32::from(velocity));
    }

    fn note_off(&mut self, key: u8) {
        Synthesizer::note_off(self, CHANNEL, i32::from(key));
    }

    fn release_all(&mut self) {
        self.note_off_all(false);
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        Synthesizer::render(self, left, right);
    }
}

/// One thing to do at a time of the piece: strike a key (`velocity` above 0) or release it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    at_us: u64,
    /// The note it belongs to: its place in the timeline.
    note: u32,
    key: u8,
    velocity: u8,
    hand: Hand,
}

/// The notes of `timeline` as events by time; at one time the releases come first, so that a key
/// struck again is released before it is.
pub fn events(timeline: &Timeline) -> Arc<[Event]> {
    let micros = |ms: f64| (ms * 1e3).max(0.).round() as u64;
    let mut events: Vec<Event> = timeline
        .notes()
        .iter()
        .enumerate()
        .flat_map(|(id, note)| {
            [
                Event {
                    at_us: micros(note.on_ms),
                    note: id as u32,
                    key: note.key,
                    velocity: note.velocity.max(1),
                    hand: note.hand,
                },
                Event {
                    at_us: micros(note.off_ms),
                    note: id as u32,
                    key: note.key,
                    velocity: 0,
                    hand: note.hand,
                },
            ]
        })
        .collect();
    events.sort_by_key(|event| (event.at_us, event.velocity != 0));
    events.into()
}

/// The audio callback's logic: renders buffers, strikes the notes the clock reaches inside each
/// one at its exact frame, and moves the clock on by what it rendered.
pub struct Renderer<S: Sink> {
    sink: S,
    events: Arc<[Event]>,
    clock: Clock,
    hands: Hands,
    sample_rate: f64,
    /// The first event not played yet.
    next: usize,
    /// The seeks the renderer has followed.
    seeks: u32,
    was_playing: bool,
    /// What is left, in microseconds, of the buffers rendered so far once their whole
    /// microseconds went to the clock: without it a speed that does not divide the rate drifts.
    carry: f64,
    /// How many times each key was struck and not released yet: a release cuts the sound only
    /// when the last note on its key ends, and a note the computer did not play has none.
    held: [u8; 128],
    /// Which notes were struck by the computer and not ended yet: the end of one the computer
    /// did not strike (a hand that is the player's) is none of its business. Made here, once.
    struck: Vec<bool>,
    /// The keys the player strikes, when this output sounds them, and the ones held now.
    live: Option<LiveKeys>,
    live_held: [bool; 128],
}

impl<S: Sink> Renderer<S> {
    pub fn new(
        sink: S,
        events: Arc<[Event]>,
        clock: Clock,
        hands: Hands,
        sample_rate: u32,
    ) -> Self {
        let seeks = clock.snapshot().seeks;
        let notes = events
            .iter()
            .map(|e| e.note as usize + 1)
            .max()
            .unwrap_or(0);
        Self {
            sink,
            events,
            clock,
            hands,
            sample_rate: f64::from(sample_rate),
            next: 0,
            seeks,
            was_playing: false,
            carry: 0.,
            held: [0; 128],
            struck: vec![false; notes],
            live: None,
            live_held: [false; 128],
        }
        .following()
    }

    /// Sounds the keys the player strikes too, from now on.
    pub fn with_live(mut self, live: LiveKeys) -> Self {
        live.discard();
        self.live = Some(live);
        self
    }

    /// Plays the keys struck and let go since the last buffer, at its start.
    fn play_live(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        while let Some((key, velocity)) = live.pop() {
            let at = usize::from(key);
            if velocity > 0 {
                self.live_held[at] = true;
                self.sink.note_on(key, velocity);
            } else {
                self.live_held[at] = false;
                // The piece holds it too: its note ends with the piece's.
                if self.held[at] == 0 {
                    self.sink.note_off(key);
                }
            }
        }
    }

    /// Puts `next` where the clock is.
    fn following(mut self) -> Self {
        let position = self.clock.snapshot().position_us;
        self.next = self.events.partition_point(|event| event.at_us < position);
        self
    }

    /// Lets the piece's notes go, and none of the keys the player holds.
    fn release_all(&mut self) {
        if self.live_held.contains(&true) {
            for key in 0..128u8 {
                let at = usize::from(key);
                if self.held[at] > 0 && !self.live_held[at] {
                    self.sink.note_off(key);
                }
            }
        } else {
            self.sink.release_all();
        }
        self.held = [0; 128];
        self.struck.fill(false);
    }

    /// Fills `left` and `right` (the same length) with the next frames of sound.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len());
        // From the first buffer on, the clock is this output's: the wall clock stops moving it.
        self.clock.claim();
        // Playing or not, the keys struck sound.
        self.play_live();
        let snapshot = self.clock.snapshot();
        if snapshot.seeks != self.seeks {
            // The position jumped: what sounded belongs to the old one.
            self.seeks = snapshot.seeks;
            self.carry = 0.;
            self.release_all();
            self.next = self
                .events
                .partition_point(|event| event.at_us < snapshot.position_us);
        }
        if self.was_playing && !snapshot.playing {
            self.release_all();
        }
        self.was_playing = snapshot.playing;

        let frames = left.len();
        if !snapshot.playing {
            self.sink.render(left, right);
            return;
        }

        let us_per_frame = 1e6 / self.sample_rate * snapshot.speed.factor();
        let start = snapshot.position_us as f64 + self.carry;
        let end = start + frames as f64 * us_per_frame;
        let mut rendered = 0;
        while let Some(event) = self.events.get(self.next)
            && (event.at_us as f64) < end
        {
            let frame =
                (((event.at_us as f64 - start) / us_per_frame).max(0.) as usize).min(frames);
            if frame > rendered {
                self.sink
                    .render(&mut left[rendered..frame], &mut right[rendered..frame]);
                rendered = frame;
            }
            let held = &mut self.held[usize::from(event.key & 127)];
            let struck = &mut self.struck[event.note as usize];
            if event.velocity == 0 {
                if std::mem::take(struck) {
                    *held = held.saturating_sub(1);
                    // A key the player holds is his to let go.
                    if *held == 0 && !self.live_held[usize::from(event.key & 127)] {
                        self.sink.note_off(event.key);
                    }
                }
            } else if self.hands.plays(event.hand) {
                *struck = true;
                *held = held.saturating_add(1);
                self.sink.note_on(event.key, event.velocity);
            }
            self.next += 1;
        }
        self.sink
            .render(&mut left[rendered..], &mut right[rendered..]);

        let total = self.carry + frames as f64 * us_per_frame;
        let whole = total.floor();
        self.carry = total - whole;
        self.clock.advance(&snapshot, whole as u64);
    }
}

/// The open output, which sounds while it lives.
pub struct Output {
    _stream: cpal::Stream,
}

/// The buffer asked of a device: [`LIVE_FRAMES`] within what it takes, its own when it does not
/// say.
fn buffer_size(supported: &cpal::SupportedBufferSize) -> cpal::BufferSize {
    match supported {
        cpal::SupportedBufferSize::Range { min, max } if min <= max => {
            cpal::BufferSize::Fixed(LIVE_FRAMES.clamp(*min, *max))
        }
        _ => cpal::BufferSize::Default,
    }
}

/// Opens the default output and plays the piece on `font` as `clock` moves, and the keys the
/// player strikes (`live`) as they come. Slow (it loads the synthesizer): not on the main thread.
pub fn start(
    font: &Arc<SoundFont>,
    timeline: &Timeline,
    clock: &Clock,
    hands: &Hands,
    lost: &Arc<AtomicBool>,
    live: &LiveKeys,
) -> Result<Output, String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output")?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("the audio output has no usable setup: {e}"))?;
    let sample_rate = supported.sample_rate();
    let channels = usize::from(supported.channels());
    let format = supported.sample_format();
    if !matches!(
        format,
        cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
    ) {
        return Err(format!(
            "the audio output's sample format ({format}) is not supported"
        ));
    }
    let open = |buffer_size| -> Result<cpal::Stream, String> {
        let config = cpal::StreamConfig {
            channels: supported.channels(),
            sample_rate,
            buffer_size,
        };
        let synth = Synthesizer::new(font, &SynthesizerSettings::new(sample_rate as i32))
            .map_err(|e| format!("cannot start the synthesizer: {e}"))?;
        let renderer = Renderer::new(
            synth,
            events(timeline),
            clock.clone(),
            hands.clone(),
            sample_rate,
        )
        .with_live(live.clone());
        match format {
            cpal::SampleFormat::I16 => build::<i16>(&device, config, channels, renderer, lost),
            cpal::SampleFormat::U16 => build::<u16>(&device, config, channels, renderer, lost),
            _ => build::<f32>(&device, config, channels, renderer, lost),
        }
        .map_err(|e| format!("cannot open the audio output: {e}"))
    };
    // A small buffer, for the keys struck to sound at once; the device's own if it refuses it.
    let small = buffer_size(supported.buffer_size());
    let stream = match open(small) {
        Ok(stream) => stream,
        Err(_) if small != cpal::BufferSize::Default => open(cpal::BufferSize::Default)?,
        Err(error) => return Err(error),
    };
    stream
        .play()
        .map_err(|e| format!("cannot start the audio output: {e}"))?;
    Ok(Output { _stream: stream })
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut renderer: Renderer<Synthesizer>,
    lost: &Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    // The callback owns its buffers: nothing is made in it.
    let mut left = vec![0f32; MAX_FRAMES];
    let mut right = vec![0f32; MAX_FRAMES];
    let lost = lost.clone();
    device.build_output_stream::<T, _, _>(
        config,
        move |data, _| {
            for chunk in data.chunks_mut(MAX_FRAMES * channels) {
                let frames = chunk.len() / channels;
                renderer.render(&mut left[..frames], &mut right[..frames]);
                for (frame, (l, r)) in chunk
                    .chunks_exact_mut(channels)
                    .zip(left.iter().zip(right.iter()))
                {
                    for (channel, sample) in frame.iter_mut().enumerate() {
                        *sample = T::from_sample(match channel {
                            0 => *l,
                            1 => *r,
                            _ => 0.,
                        });
                    }
                }
            }
        },
        // The device went (unplugged, a sound server restarted): the player says so and stops
        // the clock, rather than let the piece run on in silence.
        move |error| {
            eprintln!("gamme: audio: {error}");
            lost.store(true, Relaxed);
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Speed;
    use crate::timeline::Note;

    #[derive(Debug, PartialEq)]
    enum Call {
        On(u8, usize),
        Off(u8, usize),
        Release(usize),
    }

    /// Records what is played and at which frame of the whole run.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<Call>,
        frames: usize,
    }

    impl Sink for Recorder {
        fn note_on(&mut self, key: u8, _: u8) {
            self.calls.push(Call::On(key, self.frames));
        }
        fn note_off(&mut self, key: u8) {
            self.calls.push(Call::Off(key, self.frames));
        }
        fn release_all(&mut self) {
            self.calls.push(Call::Release(self.frames));
        }
        fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
            assert_eq!(left.len(), right.len());
            self.frames += left.len();
        }
    }

    const RATE: u32 = 1000;

    fn note(key: u8, on_ms: f64, off_ms: f64, hand: Hand) -> Note {
        Note {
            key,
            velocity: 90,
            on_ms,
            off_ms,
            hand,
            part: 0,
            measure: 0,
            element: None,
        }
    }

    fn rig(notes: Vec<Note>) -> (Renderer<Recorder>, Clock, Hands) {
        rig_at(notes, RATE)
    }

    fn rig_at(notes: Vec<Note>, rate: u32) -> (Renderer<Recorder>, Clock, Hands) {
        let timeline = Timeline::new(notes, vec![0., 1000., 2000.], 3000.);
        let clock = Clock::new(timeline.duration_ms());
        let hands = Hands::both();
        let renderer = Renderer::new(
            Recorder::default(),
            events(&timeline),
            clock.clone(),
            hands.clone(),
            rate,
        );
        (renderer, clock, hands)
    }

    /// Renders `buffers` buffers of `frames` frames.
    fn run(renderer: &mut Renderer<Recorder>, buffers: usize, frames: usize) {
        let (mut l, mut r) = (vec![0.; frames], vec![0.; frames]);
        for _ in 0..buffers {
            renderer.render(&mut l, &mut r);
        }
    }

    /// The seam with the wall clock (`wall_clock.rs`): it moves the clock until the output's
    /// first buffer, and never after.
    #[test]
    fn the_first_buffer_takes_the_clock_from_the_wall_clock() {
        let (mut renderer, clock, _) = rig(vec![]);
        clock.play();
        let wall = clock.snapshot();
        assert!(clock.advance_unclaimed(&wall, 100_000));
        assert!(!clock.claimed());
        // A tick read before the buffer and ending after it moves nothing.
        let late = clock.snapshot();
        run(&mut renderer, 1, 100);
        assert!(clock.claimed());
        assert!(!clock.advance_unclaimed(&late, 50_000));
        assert!(!clock.advance_unclaimed(&clock.snapshot(), 50_000));
        // A hundred frames at 1000 a second: 100 ms of the piece, after the wall clock's 100.
        assert_eq!(clock.snapshot().position_us, 200_000);
    }

    #[test]
    fn at_the_pieces_own_tempo_a_note_sounds_at_its_frame() {
        let (mut renderer, clock, _) = rig(vec![note(60, 250., 700., Hand::Right)]);
        clock.play();
        // 1000 frames a second: ten buffers of 100 frames are a second of the piece.
        run(&mut renderer, 10, 100);
        assert_eq!(renderer.sink.calls, [Call::On(60, 250), Call::Off(60, 700)]);
        assert_eq!(clock.snapshot().position_us, 1_000_000);
    }

    #[test]
    fn at_half_speed_the_same_notes_take_twice_the_frames() {
        let (mut renderer, clock, _) = rig(vec![note(60, 250., 700., Hand::Right)]);
        clock.set_speed(Speed::new(50));
        clock.play();
        run(&mut renderer, 20, 100);
        assert_eq!(
            renderer.sink.calls,
            [Call::On(60, 500), Call::Off(60, 1400)]
        );
        // Two seconds of sound, one of the piece.
        assert_eq!(renderer.sink.frames, 2000);
        assert_eq!(clock.snapshot().position_us, 1_000_000);
    }

    #[test]
    fn a_speed_that_does_not_divide_the_rate_does_not_drift() {
        let (mut renderer, clock, _) = rig_at(vec![], 44_100);
        clock.set_speed(Speed::new(75));
        clock.play();
        // A second of sound in 100 buffers of 441 frames, 17.006 µs of the piece a frame: a
        // buffer's microseconds are not whole, and the rest must not be lost.
        run(&mut renderer, 100, 441);
        let position = clock.snapshot().position_us;
        assert!(position.abs_diff(750_000) <= 1, "{position}");
    }

    #[test]
    fn a_seek_cuts_the_sound_and_plays_on_from_the_measure() {
        let notes = vec![
            note(60, 0., 900., Hand::Right),
            note(62, 1000., 1500., Hand::Right),
            note(64, 2000., 2500., Hand::Right),
        ];
        let timeline = Timeline::new(notes.clone(), vec![0., 1000., 2000.], 3000.);
        let (mut renderer, clock, _) = rig(notes);
        clock.play();
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls, [Call::On(60, 0)]);

        // The third measure: the note starting on its first instant is not skipped.
        assert!(clock.seek_measure(&timeline, 2));
        run(&mut renderer, 1, 100);
        assert_eq!(
            renderer.sink.calls[1..],
            [Call::Release(100), Call::On(64, 100)]
        );
    }

    #[test]
    fn a_pause_lets_the_notes_go_and_nothing_plays_until_it_resumes() {
        let (mut renderer, clock, _) = rig(vec![note(60, 0., 900., Hand::Right)]);
        clock.play();
        run(&mut renderer, 1, 100);
        clock.pause();
        run(&mut renderer, 5, 100);
        assert_eq!(renderer.sink.calls, [Call::On(60, 0), Call::Release(100)]);
        assert_eq!(clock.snapshot().position_us, 100_000);
        clock.play();
        run(&mut renderer, 9, 100);
        // The note was released by the pause: its end, when it comes, cuts nothing again.
        assert_eq!(renderer.sink.calls[2..], []);
    }

    #[test]
    fn only_the_computers_hands_are_struck() {
        let (mut renderer, clock, hands) = rig(vec![
            note(72, 0., 100., Hand::Right),
            note(48, 0., 100., Hand::Left),
        ]);
        hands.set(false, true);
        clock.play();
        run(&mut renderer, 1, 200);
        let struck: Vec<_> = renderer
            .sink
            .calls
            .iter()
            .filter(|call| matches!(call, Call::On(..)))
            .collect();
        assert_eq!(struck, [&Call::On(48, 0)]);
    }

    #[test]
    fn a_key_is_released_only_when_its_last_note_ends() {
        let (mut renderer, clock, _) = rig(vec![
            note(60, 0., 300., Hand::Right),
            note(60, 100., 500., Hand::Left),
        ]);
        clock.play();
        run(&mut renderer, 1, 600);
        assert_eq!(
            renderer.sink.calls,
            [Call::On(60, 0), Call::On(60, 100), Call::Off(60, 500)]
        );
    }

    #[test]
    fn the_end_of_a_note_the_computer_did_not_play_leaves_the_others_alone() {
        let (mut renderer, clock, hands) = rig(vec![
            note(60, 0., 300., Hand::Right),
            note(60, 100., 500., Hand::Left),
        ]);
        // Only the left hand is the computer's: the right hand's note 60 ends at 300 ms.
        hands.set(false, true);
        clock.play();
        run(&mut renderer, 1, 600);
        assert_eq!(renderer.sink.calls, [Call::On(60, 100), Call::Off(60, 500)]);
    }

    #[test]
    fn a_key_struck_again_at_the_instant_it_ends_is_released_first() {
        let (mut renderer, clock, _) = rig(vec![
            note(60, 0., 500., Hand::Right),
            note(60, 500., 900., Hand::Right),
        ]);
        clock.play();
        run(&mut renderer, 1, 600);
        assert_eq!(
            renderer.sink.calls,
            [Call::On(60, 0), Call::Off(60, 500), Call::On(60, 500)]
        );
    }

    #[test]
    fn the_piece_ends_with_the_clock_and_the_sound_is_released() {
        let (mut renderer, clock, _) = rig(vec![note(60, 2900., 3000., Hand::Right)]);
        clock.seek_ms(2800.);
        clock.play();
        run(&mut renderer, 5, 100);
        assert!(!clock.playing());
        assert_eq!(clock.snapshot().position_us, 3_000_000);
        assert_eq!(renderer.sink.calls.last(), Some(&Call::Release(200)));
    }

    #[test]
    fn a_wake_up_in_the_middle_of_a_piece_starts_from_the_clocks_position() {
        let timeline = Timeline::new(
            vec![
                note(60, 100., 200., Hand::Right),
                note(62, 5000., 5100., Hand::Right),
            ],
            vec![0.],
            6000.,
        );
        let clock = Clock::new(timeline.duration_ms());
        clock.seek_ms(4000.);
        let mut renderer = Renderer::new(
            Recorder::default(),
            events(&timeline),
            clock.clone(),
            Hands::both(),
            RATE,
        );
        clock.play();
        run(&mut renderer, 1, 1500);
        assert_eq!(
            renderer.sink.calls,
            [Call::On(62, 1000), Call::Off(62, 1100)]
        );
    }

    fn live_rig(notes: Vec<Note>) -> (Renderer<Recorder>, Clock, LiveKeys) {
        let (renderer, clock, _) = rig(notes);
        let live = LiveKeys::default();
        (renderer.with_live(live.clone()), clock, live)
    }

    #[test]
    fn a_key_struck_sounds_at_the_next_buffer_with_no_piece_playing() {
        let (mut renderer, clock, live) = live_rig(vec![]);
        assert!(live.push(64, 100));
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls, [Call::On(64, 0)]);
        live.push(64, 0);
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls[1..], [Call::Off(64, 100)]);
        // The clock did not move: nothing plays the piece.
        assert_eq!(clock.snapshot().position_us, 0);
    }

    #[test]
    fn the_piece_ending_a_note_does_not_cut_the_key_the_player_holds() {
        let (mut renderer, clock, live) = live_rig(vec![note(60, 0., 150., Hand::Right)]);
        clock.play();
        run(&mut renderer, 1, 100);
        live.push(60, 90);
        run(&mut renderer, 1, 100);
        // The piece's note ends at 150 ms: the player's key goes on.
        assert_eq!(renderer.sink.calls, [Call::On(60, 0), Call::On(60, 100)]);
        live.push(60, 0);
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls[2..], [Call::Off(60, 200)]);
    }

    #[test]
    fn the_player_letting_a_key_go_does_not_cut_the_pieces_note_on_it() {
        let (mut renderer, clock, live) = live_rig(vec![note(60, 0., 250., Hand::Right)]);
        clock.play();
        run(&mut renderer, 1, 100);
        live.push(60, 90);
        live.push(60, 0);
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls, [Call::On(60, 0), Call::On(60, 100)]);
        // The piece's own end cuts it.
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls[2..], [Call::Off(60, 250)]);
    }

    #[test]
    fn a_pause_or_a_seek_lets_the_piece_go_and_not_the_keys_held() {
        let (mut renderer, clock, live) = live_rig(vec![
            note(60, 0., 900., Hand::Right),
            note(64, 0., 900., Hand::Right),
        ]);
        clock.play();
        live.push(72, 90);
        live.push(64, 90);
        run(&mut renderer, 1, 100);
        clock.pause();
        run(&mut renderer, 1, 100);
        // Only the piece's 60: 64 is held by the player too, 72 is his alone.
        let after: Vec<_> = renderer.sink.calls.iter().skip(4).collect();
        assert_eq!(after, [&Call::Off(60, 100)]);
        clock.play();
        clock.seek_ms(500.);
        run(&mut renderer, 1, 100);
        assert!(
            !renderer
                .sink
                .calls
                .iter()
                .any(|call| matches!(call, Call::Off(72 | 64, _) | Call::Release(_))),
            "{:?}",
            renderer.sink.calls
        );
        // Once the player lets go of everything, a pause releases all again.
        live.push(72, 0);
        live.push(64, 0);
        run(&mut renderer, 1, 100);
        clock.pause();
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls.last(), Some(&Call::Release(400)));
    }

    #[test]
    fn keys_struck_before_the_output_was_there_are_not_played_late() {
        let live = LiveKeys::default();
        live.push(60, 90);
        let (renderer, _, _) = rig(vec![]);
        let mut renderer = renderer.with_live(live.clone());
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls, []);
        live.push(62, 90);
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls, [Call::On(62, 100)]);
    }

    #[test]
    fn the_ring_drops_what_no_one_reads_and_goes_round() {
        let live = LiveKeys::default();
        for _ in 0..LIVE_SLOTS {
            assert!(live.push(60, 90));
        }
        assert!(!live.push(61, 90), "full");
        live.discard();
        assert_eq!(live.pop(), None);
        // Many times round, from other threads, nothing lost or changed.
        let writer = live.clone();
        let thread = std::thread::spawn(move || {
            for n in 0..10_000u32 {
                while !writer.push((n % 128) as u8, (n % 127 + 1) as u8) {
                    std::thread::yield_now();
                }
            }
        });
        let mut n = 0u32;
        while n < 10_000 {
            if let Some(got) = live.pop() {
                assert_eq!(got, ((n % 128) as u8, (n % 127 + 1) as u8));
                n += 1;
            }
        }
        thread.join().unwrap();
        assert_eq!(live.pop(), None);
    }

    #[test]
    fn the_buffer_asked_for_is_small_within_what_the_device_takes() {
        use cpal::{BufferSize, SupportedBufferSize};
        let range = |min, max| buffer_size(&SupportedBufferSize::Range { min, max });
        assert_eq!(range(64, 8192), BufferSize::Fixed(LIVE_FRAMES));
        assert_eq!(range(512, 8192), BufferSize::Fixed(512));
        assert_eq!(range(16, 128), BufferSize::Fixed(128));
        assert_eq!(
            buffer_size(&SupportedBufferSize::Unknown),
            BufferSize::Default
        );
    }

    #[test]
    fn events_are_by_time_with_releases_first() {
        let timeline = Timeline::new(
            vec![
                note(60, 0., 10., Hand::Right),
                note(60, 10., 20., Hand::Left),
            ],
            vec![0.],
            20.,
        );
        let list = events(&timeline);
        let shape: Vec<_> = list.iter().map(|e| (e.at_us, e.velocity != 0)).collect();
        assert_eq!(
            shape,
            [(0, true), (10_000, false), (10_000, true), (20_000, false)]
        );
    }
}
