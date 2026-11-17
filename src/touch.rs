//! The touch of the player's keys on the built-in piano: how the velocity a keyboard sends
//! becomes the velocity the synthesizer is struck with, as a digital piano's own "Touch" setting
//! does.
//!
//! Why it is needed: the SoundFont synthesizer attenuates a note by `40 · log10(velocity / 127)`
//! dB (rustysynth's voices, as the SF2 specification says), so a soft key (velocity 30) sounds 25
//! dB below the loudest one, and 13 dB below the piece's notes (velocity 90 or so, 6 dB down). On a
//! laptop's speakers that is almost silent. A learner strikes softly, so the default is
//! [`Touch::Light`].
//!
//! The curve is pure and applied on the writer's side, before the keys enter the audio callback's
//! ring (`piano::LiveKeys`): the callback does no arithmetic of the sort. It is applied to the
//! player's keys on the built-in piano only: not to the piece, and not to a MIDI output, where the
//! piano's own touch setting governs.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The loudest velocity MIDI has.
const MAX: u8 = 127;
/// The velocity every key has under [`Touch::Fixed`]: firm (4 dB under the loudest) without
/// being the harshest.
const FIXED: u8 = 100;

/// How the player's keys are struck on the built-in piano.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Touch {
    /// Soft keys sound louder.
    #[default]
    Light,
    /// The velocity as it was played.
    Normal,
    /// Keys must be struck harder to sound as loud.
    Heavy,
    /// Every key at one volume, whatever the velocity.
    Fixed,
}

impl Touch {
    /// In the order the settings page lists them.
    pub const ALL: [Touch; 4] = [Touch::Light, Touch::Normal, Touch::Heavy, Touch::Fixed];

    /// What is kept in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Touch::Light => "light",
            Touch::Normal => "normal",
            Touch::Heavy => "heavy",
            Touch::Fixed => "fixed",
        }
    }

    /// The touch kept as `id`, if this version knows it.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|touch| touch.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Touch::Light => "Light",
            Touch::Normal => "Normal",
            Touch::Heavy => "Heavy",
            Touch::Fixed => "Fixed",
        }
    }

    /// What it does, in a line.
    pub fn line(self) -> &'static str {
        match self {
            Touch::Light => "Soft keys sound louder",
            Touch::Normal => "As you play it",
            Touch::Heavy => "Keys need a firmer touch",
            Touch::Fixed => "Every key at the same volume",
        }
    }

    /// The velocity a key struck at `velocity` is played with. 0 (a key let go) stays 0; any other
    /// velocity stays within 1 to 127, and a harder key is never softer than a softer one.
    pub fn velocity(self, velocity: u8) -> u8 {
        if velocity == 0 {
            return 0;
        }
        let velocity = velocity.min(MAX);
        // A power of the velocity's share of the loudest one: below 1 lifts the soft keys, above
        // 1 lowers them, and 127 stays 127 either way.
        let shaped = |power: f32| {
            let share = f32::from(velocity) / f32::from(MAX);
            (f32::from(MAX) * share.powf(power))
                .round()
                .clamp(1., f32::from(MAX)) as u8
        };
        match self {
            Touch::Light => shaped(0.5),
            Touch::Normal => velocity,
            Touch::Heavy => shaped(1.5),
            Touch::Fixed => FIXED,
        }
    }
}

impl Serialize for Touch {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

/// Whatever the file holds: only a text this version knows is a touch.
#[derive(Deserialize)]
#[serde(untagged)]
enum Kept {
    Text(String),
    Other(serde::de::IgnoredAny),
}

impl<'de> Deserialize<'de> for Touch {
    /// A touch this version does not know (a file edited by hand, or written by another
    /// version), or not a text, is the default: the file is still read, its other settings kept.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Kept::deserialize(deserializer)? {
            Kept::Text(id) => Self::from_id(&id).unwrap_or_default(),
            Kept::Other(_) => Self::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_let_go_stays_let_go_and_a_key_struck_stays_a_key() {
        for touch in Touch::ALL {
            assert_eq!(touch.velocity(0), 0, "{touch:?}");
            for velocity in 1..=127 {
                let out = touch.velocity(velocity);
                assert!((1..=127).contains(&out), "{touch:?} {velocity} -> {out}");
            }
        }
    }

    #[test]
    fn a_harder_key_is_never_softer() {
        for touch in Touch::ALL {
            for velocity in 1..127 {
                assert!(
                    touch.velocity(velocity) <= touch.velocity(velocity + 1),
                    "{touch:?} at {velocity}"
                );
            }
        }
    }

    #[test]
    fn normal_is_the_velocity_as_played() {
        for velocity in 0..=127 {
            assert_eq!(Touch::Normal.velocity(velocity), velocity);
        }
    }

    #[test]
    fn the_loudest_key_stays_the_loudest() {
        for touch in [Touch::Light, Touch::Normal, Touch::Heavy] {
            assert_eq!(touch.velocity(127), 127, "{touch:?}");
        }
    }

    #[test]
    fn light_lifts_soft_keys_and_heavy_lowers_them() {
        for velocity in 1..127 {
            assert!(Touch::Light.velocity(velocity) >= velocity, "{velocity}");
            assert!(Touch::Heavy.velocity(velocity) <= velocity, "{velocity}");
        }
        assert!(Touch::Light.velocity(30) > 55);
        assert!(Touch::Heavy.velocity(30) < 20);
    }

    #[test]
    fn fixed_is_one_volume_for_every_key_struck() {
        for velocity in 1..=127 {
            assert_eq!(Touch::Fixed.velocity(velocity), 100);
        }
    }

    #[test]
    fn the_curves_values() {
        let at = |touch: Touch| [1, 10, 32, 64, 90, 127].map(|v| touch.velocity(v));
        assert_eq!(at(Touch::Light), [11, 36, 64, 90, 107, 127]);
        assert_eq!(at(Touch::Normal), [1, 10, 32, 64, 90, 127]);
        assert_eq!(at(Touch::Heavy), [1, 3, 16, 45, 76, 127]);
        assert_eq!(at(Touch::Fixed), [100; 6]);
    }

    #[test]
    fn a_velocity_over_127_is_the_loudest() {
        assert_eq!(Touch::Normal.velocity(200), 127);
        assert_eq!(Touch::Light.velocity(200), 127);
    }

    #[test]
    fn light_is_the_default_for_learners() {
        assert_eq!(Touch::default(), Touch::Light);
    }

    #[test]
    fn a_touch_is_kept_by_its_id_and_an_unknown_one_is_the_default() {
        for touch in Touch::ALL {
            let json = serde_json::to_string(&touch).unwrap();
            assert_eq!(json, format!("\"{}\"", touch.id()));
            assert_eq!(serde_json::from_str::<Touch>(&json).unwrap(), touch);
        }
        for text in ["\"firm\"", "\"\"", "3", "null", "[1]", "{\"a\":1}", "true"] {
            assert_eq!(
                serde_json::from_str::<Touch>(text).unwrap(),
                Touch::Light,
                "{text}"
            );
        }
    }
}
