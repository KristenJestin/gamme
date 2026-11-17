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
//! Each buffer asks the clock where it goes ([`Clock::step`]) and plays exactly what that goes
//! through: no further than a chord the player owes (the wait, `practice.rs`), and round the
//! A–B loop, whose seam it plays at its own frame: the piece's notes let go at B's end, A's
//! struck from the same frame, none twice. The computer strikes only its own hands (`Hands`);
//! a note it did not strike, it does not end.
//!
//! The keys the player strikes on a MIDI keyboard (`midi_in.rs`), and his sustain pedal, reach
//! the callback through [`LiveKeys`], a ring of atomics made once: the callback plays them at the
//! start of its next buffer, so the output is opened with a small one (`LIVE_FRAMES`). They are
//! played on a channel of the synthesizer of their own (the same piano): the piece ending a note,
//! a pause or a seek do not cut a key the player holds, the player letting a key go does not cut
//! the piece's note on it, and his pedal holds his notes, not the piece's (a pause still silences
//! the piece).

use std::sync::atomic::Ordering::{Acquire, Release};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::clock::Clock;
use crate::timeline::{Hand, Timeline};

/// The largest chunk the callback renders at once; a longer buffer is rendered in several.
const MAX_FRAMES: usize = 4096;
/// The channels of the synthesizer the piece and the player are on; both are the piano (preset
/// 0 of bank 0, a channel's own until told otherwise).
const CHANNEL: i32 = 0;
const LIVE_CHANNEL: i32 = 1;
/// The sustain pedal's controller.
const PEDAL: i32 = 64;
/// The buffer the output is asked for, in frames (5.3 ms at 48 kHz): a key struck sounds at the
/// next buffer. Smaller would gain little and risk gaps on a busy machine; the device's own
/// range has the last word.
const LIVE_FRAMES: u32 = 256;
/// How many keys struck and let go can wait for the callback: hundreds of buffers' worth.
const LIVE_SLOTS: usize = 256;

/// The keys the player strikes and his pedal, on their way to the audio callback: a ring of
/// atomics, written on MIDI's threads and read by the callback, which neither locks nor waits.
/// Clones share it.
#[derive(Clone)]
pub struct LiveKeys(Arc<Ring>);

/// What the player does, as the callback reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Live {
    /// A key struck (`velocity` above 0) or let go.
    Key(u8, u8),
    /// The sustain pedal's value (64 and above: down).
    Pedal(u8),
}

impl Live {
    /// `key << 8 | velocity` for a key; the pedal is the key byte's top bit, and its value.
    const PEDAL: u16 = 0x80 << 8;

    fn pack(self) -> u16 {
        match self {
            Live::Key(key, velocity) => u16::from(key & 127) << 8 | u16::from(velocity & 127),
            Live::Pedal(value) => Self::PEDAL | u16::from(value & 127),
        }
    }

    fn unpack(packed: u16) -> Self {
        let low = (packed & 0x7F) as u8;
        if packed & Self::PEDAL != 0 {
            Live::Pedal(low)
        } else {
            Live::Key((packed >> 8) as u8 & 127, low)
        }
    }
}

struct Ring {
    /// Each a [`Live`], packed.
    slots: [AtomicU16; LIVE_SLOTS],
    /// How many were ever written, and read: a slot is free once read.
    written: AtomicUsize,
    read: AtomicUsize,
    /// The pedal as the player left it last: an output that opens later starts with it.
    pedal: AtomicU8,
    /// The writers take turns: the ring has one writer's index, and two writing at once would
    /// claim the same slot. MIDI in hands the keys of every keyboard on one at a time already,
    /// but `push` is open to any thread, and a lock is cheap there. Only writers take it; the
    /// reader (the callback) never does, so the callback stays lock-free.
    writing: Mutex<()>,
}

impl Default for LiveKeys {
    fn default() -> Self {
        Self(Arc::new(Ring {
            slots: std::array::from_fn(|_| AtomicU16::new(0)),
            written: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            pedal: AtomicU8::new(0),
            writing: Mutex::new(()),
        }))
    }
}

impl LiveKeys {
    /// A key struck (`velocity` above 0) or let go. Not for the audio callback. A ring that is
    /// full (no output reads it) drops it: what is not heard now is not to be heard later.
    pub fn push(&self, key: u8, velocity: u8) -> bool {
        self.write(Live::Key(key, velocity))
    }

    /// The sustain pedal moved to `value`; as [`LiveKeys::push`].
    pub fn push_pedal(&self, value: u8) -> bool {
        self.0.pedal.store(value & 127, Relaxed);
        self.write(Live::Pedal(value))
    }

    fn write(&self, live: Live) -> bool {
        let ring = &*self.0;
        let _turn = ring.writing.lock().expect("the live keys' lock");
        let written = ring.written.load(Relaxed);
        if written.wrapping_sub(ring.read.load(Acquire)) >= LIVE_SLOTS {
            return false;
        }
        ring.slots[written % LIVE_SLOTS].store(live.pack(), Relaxed);
        ring.written.store(written.wrapping_add(1), Release);
        true
    }

    /// The next thing waiting, for the callback: no lock, no allocation.
    fn pop(&self) -> Option<Live> {
        let ring = &*self.0;
        let read = ring.read.load(Relaxed);
        if read == ring.written.load(Acquire) {
            return None;
        }
        let packed = ring.slots[read % LIVE_SLOTS].load(Relaxed);
        ring.read.store(read.wrapping_add(1), Release);
        Some(Live::unpack(packed))
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
    /// Lets every note of the piece go, with its release; none of the player's.
    fn release_all(&mut self);
    /// The player's own keys and pedal, apart from the piece's.
    fn live_on(&mut self, key: u8, velocity: u8);
    fn live_off(&mut self, key: u8);
    fn live_pedal(&mut self, value: u8);
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
        self.note_off_all_channel(CHANNEL, false);
    }

    fn live_on(&mut self, key: u8, velocity: u8) {
        Synthesizer::note_on(self, LIVE_CHANNEL, i32::from(key), i32::from(velocity));
    }

    fn live_off(&mut self, key: u8) {
        Synthesizer::note_off(self, LIVE_CHANNEL, i32::from(key));
    }

    fn live_pedal(&mut self, value: u8) {
        self.process_midi_message(LIVE_CHANNEL, 0xB0, PEDAL, i32::from(value));
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
    /// The keys the player strikes, when this output sounds them.
    live: Option<LiveKeys>,
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
        }
        .following()
    }

    /// What it plays on, for the tests to read.
    #[cfg(test)]
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// Sounds the keys the player strikes too, from now on, his pedal as he holds it now.
    pub fn with_live(mut self, live: LiveKeys) -> Self {
        live.discard();
        let pedal = live.0.pedal.load(Relaxed);
        if pedal > 0 {
            self.sink.live_pedal(pedal);
        }
        self.live = Some(live);
        self
    }

    /// Plays what the player did since the last buffer, at its start.
    fn play_live(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        while let Some(played) = live.pop() {
            match played {
                Live::Key(key, 0) => self.sink.live_off(key),
                Live::Key(key, velocity) => self.sink.live_on(key, velocity),
                Live::Pedal(value) => self.sink.live_pedal(value),
            }
        }
    }

    /// Puts `next` where the clock is.
    fn following(mut self) -> Self {
        let position = self.clock.snapshot().position_us;
        self.next = self.events.partition_point(|event| event.at_us < position);
        self
    }

    /// Lets the piece's notes go, and none of the player's.
    fn release_all(&mut self) {
        self.held = [0; 128];
        self.struck.fill(false);
        self.sink.release_all();
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
        let total = self.carry + frames as f64 * us_per_frame;
        let whole = total.floor();
        // Where the clock goes: no further than a chord the player owes, and on from the loop's
        // start past its end. What is played is what it goes through, and nothing else.
        let step = self.clock.step(&snapshot, whole as u64);
        let carry = self.carry;
        // The frame of a moment `offset` µs into the step.
        let frame_of =
            |offset: u64| (((offset as f64 - carry) / us_per_frame).max(0.) as usize).min(frames);
        let mut rendered = 0;
        let first = |at: u64| frame_of(at.saturating_sub(step.from_us));
        self.play_until(step.to_us, first, &mut rendered, left, right);
        if let Some((start, to)) = step.wrapped {
            // The seam of the loop: what the piece sounds is let go at B's end, and the notes
            // from A's start are struck from the same frame on, none twice.
            let seam = frame_of(step.to_us - step.from_us);
            if seam > rendered {
                self.sink
                    .render(&mut left[rendered..seam], &mut right[rendered..seam]);
                rendered = seam;
            }
            self.release_all();
            self.next = self.events.partition_point(|event| event.at_us < start);
            let before = step.to_us - step.from_us;
            let after = |at: u64| frame_of(before + at.saturating_sub(start));
            self.play_until(to, after, &mut rendered, left, right);
        }
        let held = step.moved_us < whole as u64;
        if held {
            // The clock stands on a chord owed: the computer's notes that end there end now,
            // not when the chord is struck (they come first at that instant, before any start).
            let stop = frame_of(step.moved_us);
            self.end_at(step.position_us(), stop, &mut rendered, left, right);
        }
        self.sink
            .render(&mut left[rendered..], &mut right[rendered..]);

        // Held by a chord, the clock stands: nothing of this buffer is owed to the next one.
        self.carry = if held { 0. } else { total - whole };
        if self.clock.commit(&snapshot, &step) {
            // The loop's wrap is a seek this renderer made itself and followed already.
            self.seeks = step.seeks;
        }
    }

    /// Plays the events before `until` (µs of the piece), each at the frame `frame_of` gives it,
    /// rendering the frames between them.
    fn play_until(
        &mut self,
        until: u64,
        frame_of: impl Fn(u64) -> usize,
        rendered: &mut usize,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        while let Some(&event) = self.events.get(self.next)
            && event.at_us < until
        {
            let frame = frame_of(event.at_us);
            if frame > *rendered {
                self.sink
                    .render(&mut left[*rendered..frame], &mut right[*rendered..frame]);
                *rendered = frame;
            }
            self.play(event);
            self.next += 1;
        }
    }

    /// Plays the ends of notes at exactly `at` (µs of the piece), at frame `frame`.
    fn end_at(
        &mut self,
        at: u64,
        frame: usize,
        rendered: &mut usize,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        while let Some(&event) = self.events.get(self.next)
            && event.at_us == at
            && event.velocity == 0
        {
            if frame > *rendered {
                self.sink
                    .render(&mut left[*rendered..frame], &mut right[*rendered..frame]);
                *rendered = frame;
            }
            self.play(event);
            self.next += 1;
        }
    }

    /// Strikes or releases what `event` says, if it is the computer's to.
    fn play(&mut self, event: Event) {
        let held = &mut self.held[usize::from(event.key & 127)];
        let struck = &mut self.struck[event.note as usize];
        if event.velocity == 0 {
            // The end of a note the computer did not strike (the player's hand) is not its own.
            if std::mem::take(struck) {
                *held = held.saturating_sub(1);
                if *held == 0 {
                    self.sink.note_off(event.key);
                }
            }
        } else if self.hands.plays(event.hand) {
            *struck = true;
            *held = held.saturating_add(1);
            self.sink.note_on(event.key, event.velocity);
        }
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
        /// The player's, on his own channel.
        LiveOn(u8, usize),
        LiveOff(u8, usize),
        Pedal(u8, usize),
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
        fn live_on(&mut self, key: u8, _: u8) {
            self.calls.push(Call::LiveOn(key, self.frames));
        }
        fn live_off(&mut self, key: u8) {
            self.calls.push(Call::LiveOff(key, self.frames));
        }
        fn live_pedal(&mut self, value: u8) {
            self.calls.push(Call::Pedal(value, self.frames));
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
        assert_eq!(renderer.sink.calls, [Call::LiveOn(64, 0)]);
        live.push(64, 0);
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls[1..], [Call::LiveOff(64, 100)]);
        // The clock did not move: nothing plays the piece.
        assert_eq!(clock.snapshot().position_us, 0);
    }

    #[test]
    fn the_piece_and_the_player_end_their_own_notes_on_one_key() {
        let (mut renderer, clock, live) = live_rig(vec![note(60, 0., 150., Hand::Right)]);
        clock.play();
        run(&mut renderer, 1, 100);
        live.push(60, 90);
        run(&mut renderer, 1, 100);
        // The piece's note ends at 150 ms, on its own channel: the player's key goes on.
        assert_eq!(
            renderer.sink.calls,
            [Call::On(60, 0), Call::LiveOn(60, 100), Call::Off(60, 150)]
        );
        live.push(60, 0);
        run(&mut renderer, 1, 100);
        assert_eq!(renderer.sink.calls[3..], [Call::LiveOff(60, 200)]);
    }

    #[test]
    fn a_pause_or_a_seek_lets_the_piece_go_and_not_the_player() {
        let (mut renderer, clock, live) = live_rig(vec![note(60, 0., 900., Hand::Right)]);
        clock.play();
        live.push(72, 90);
        live.push_pedal(127);
        run(&mut renderer, 1, 100);
        clock.pause();
        run(&mut renderer, 1, 100);
        clock.play();
        clock.seek_ms(500.);
        run(&mut renderer, 1, 100);
        assert_eq!(
            renderer.sink.calls,
            [
                Call::LiveOn(72, 0),
                Call::Pedal(127, 0),
                Call::On(60, 0),
                Call::Release(100),
                Call::Release(200),
            ]
        );
    }

    #[test]
    fn the_pedal_goes_to_the_player_at_the_next_buffer() {
        let (mut renderer, _, live) = live_rig(vec![]);
        assert!(live.push_pedal(100));
        live.push(60, 80);
        live.push(60, 0);
        run(&mut renderer, 1, 100);
        live.push_pedal(0);
        run(&mut renderer, 1, 100);
        assert_eq!(
            renderer.sink.calls,
            [
                Call::Pedal(100, 0),
                Call::LiveOn(60, 0),
                Call::LiveOff(60, 0),
                Call::Pedal(0, 100),
            ]
        );
    }

    #[test]
    fn an_output_that_opens_with_the_pedal_down_starts_with_it() {
        let live = LiveKeys::default();
        live.push_pedal(127);
        let (renderer, _, _) = rig(vec![]);
        let renderer = renderer.with_live(live.clone());
        assert_eq!(renderer.sink.calls, [Call::Pedal(127, 0)]);
        live.push_pedal(0);
        let (renderer, _, _) = rig(vec![]);
        assert_eq!(renderer.with_live(live).sink.calls, []);
    }

    #[test]
    fn what_the_player_does_is_packed_and_read_back_whole() {
        for live in [
            Live::Key(0, 0),
            Live::Key(127, 127),
            Live::Key(60, 1),
            Live::Pedal(0),
            Live::Pedal(64),
            Live::Pedal(127),
        ] {
            assert_eq!(Live::unpack(live.pack()), live);
        }
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
        assert_eq!(renderer.sink.calls, [Call::LiveOn(62, 100)]);
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
                assert_eq!(got, Live::Key((n % 128) as u8, (n % 127 + 1) as u8));
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
