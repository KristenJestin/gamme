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

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::clock::Clock;
use crate::timeline::{Hand, Timeline};

/// The largest chunk the callback renders at once; a longer buffer is rendered in several.
const MAX_FRAMES: usize = 4096;
/// The channel of a MIDI synthesizer the piano is on.
const CHANNEL: i32 = 0;

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
        .flat_map(|note| {
            [
                Event {
                    at_us: micros(note.on_ms),
                    key: note.key,
                    velocity: note.velocity.max(1),
                    hand: note.hand,
                },
                Event {
                    at_us: micros(note.off_ms),
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
        }
        .following()
    }

    /// Puts `next` where the clock is.
    fn following(mut self) -> Self {
        let position = self.clock.snapshot().position_us;
        self.next = self.events.partition_point(|event| event.at_us < position);
        self
    }

    /// Fills `left` and `right` (the same length) with the next frames of sound.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len());
        let snapshot = self.clock.snapshot();
        if snapshot.seeks != self.seeks {
            // The position jumped: what sounded belongs to the old one.
            self.seeks = snapshot.seeks;
            self.carry = 0.;
            self.sink.release_all();
            self.next = self
                .events
                .partition_point(|event| event.at_us < snapshot.position_us);
        }
        if self.was_playing && !snapshot.playing {
            self.sink.release_all();
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
            if event.velocity == 0 {
                self.sink.note_off(event.key);
            } else if self.hands.plays(event.hand) {
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

/// Opens the default output and plays the piece on `font` as `clock` moves. Slow (it loads the
/// synthesizer): not on the main thread.
pub fn start(
    font: &Arc<SoundFont>,
    timeline: &Timeline,
    clock: &Clock,
    hands: &Hands,
) -> Result<Output, String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output")?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("the audio output has no usable setup: {e}"))?;
    let sample_rate = supported.sample_rate();
    let channels = usize::from(supported.channels());
    let config = cpal::StreamConfig {
        channels: supported.channels(),
        sample_rate,
        // The device's own size: a small one is lower latency, but the clock and the cursor do
        // not need it, and a size the device refuses is an error.
        buffer_size: cpal::BufferSize::Default,
    };
    let synth = Synthesizer::new(font, &SynthesizerSettings::new(sample_rate as i32))
        .map_err(|e| format!("cannot start the synthesizer: {e}"))?;
    let renderer = Renderer::new(
        synth,
        events(timeline),
        clock.clone(),
        hands.clone(),
        sample_rate,
    );
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, channels, renderer),
        cpal::SampleFormat::I16 => build::<i16>(&device, config, channels, renderer),
        cpal::SampleFormat::U16 => build::<u16>(&device, config, channels, renderer),
        other => {
            return Err(format!(
                "the audio output's sample format ({other}) is not supported"
            ));
        }
    }
    .map_err(|e| format!("cannot open the audio output: {e}"))?;
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
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    // The callback owns its buffers: nothing is made in it.
    let mut left = vec![0f32; MAX_FRAMES];
    let mut right = vec![0f32; MAX_FRAMES];
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
        |error| eprintln!("gamme: audio: {error}"),
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
        assert_eq!(renderer.sink.calls[2..], [Call::Off(60, 600 + 800)]);
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
