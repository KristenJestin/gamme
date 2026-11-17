//! What the player chose in the settings page, kept in `$XDG_CONFIG_HOME/gamme/settings.json`:
//! which piano the built-in synthesizer plays (its SoundFont), whether the piece goes to a MIDI
//! output instead, which MIDI input the keys come from, and the touch of the player's keys on the
//! built-in piano.
//!
//! The colours are the kit's appearance state and stay in the state folder (`appearance.rs`).
//!
//! The file is written beside and renamed over the old one, the folder `0700` and the file `0600`.
//! Absent, or a key missing: the defaults (the small piano, no MIDI out, every input port, a light
//! touch).

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::midi_out::{MidiOut, Ports};
use crate::paths;
use crate::player::{Audio, Builtin};
use crate::soundfont::{self, Font};
use crate::touch::Touch;

/// The settings, as they are kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The SoundFont the built-in piano plays, by its `Font::id`: the small upright (the
    /// default) or the concert grand.
    pub piano: String,
    /// The MIDI output the piece sounds on, by the port's name (`midi_out::key`), instead of the
    /// built-in piano. None: the built-in piano.
    pub midi_out: Option<String>,
    /// The only MIDI input port the player's keys are read from (`midi_in.rs`). None: every port,
    /// as it appears.
    pub midi_in: Option<String>,
    /// How the player's keys are struck on the built-in piano (`touch.rs`). A value this version
    /// does not know is the default, and the file is read all the same.
    pub touch: Touch,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            piano: soundfont::SMALL_PIANO.id.to_owned(),
            midi_out: None,
            midi_in: None,
            touch: Touch::default(),
        }
    }
}

impl Settings {
    /// The SoundFont chosen; one this version does not know (a file edited by hand, or written by
    /// another version) is the default.
    pub fn font(&self) -> Font {
        soundfont::FONTS
            .iter()
            .find(|font| font.id == self.piano)
            .copied()
            .unwrap_or(soundfont::SMALL_PIANO)
    }

    /// What plays the piece, and the SoundFont it would use: the MIDI output when one is chosen,
    /// the built-in piano otherwise (and when the MIDI output cannot be used: see
    /// [`Audio::fallback`]).
    pub fn audio(&self, ports: Arc<dyn Ports>) -> (Arc<dyn Audio>, Font) {
        let builtin: Arc<dyn Audio> = Arc::new(Builtin::new(self.touch));
        let audio: Arc<dyn Audio> = match &self.midi_out {
            Some(port) => Arc::new(MidiOut::new(ports, port.clone(), builtin)),
            None => builtin,
        };
        (audio, self.font())
    }
}

fn file(config: &Path) -> PathBuf {
    config.join("settings.json")
}

/// The settings kept: the defaults when there is no file, and why when there is one that cannot
/// be read.
pub fn read(config: &Path) -> Result<Settings, String> {
    let bytes = match std::fs::read(file(config)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(format!("settings.json: {error}")),
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("settings.json: {error}"))
}

/// The settings kept, for a start: the defaults when none are kept, or when they cannot be read,
/// and then why, for the window to say (it is on the error output too). Reads a file: not for the
/// main thread.
pub fn load() -> (Settings, Option<String>) {
    let Some(config) = paths::config() else {
        return (Settings::default(), None);
    };
    match read(&config) {
        Ok(settings) => (settings, None),
        Err(why) => {
            eprintln!("gamme: the kept settings cannot be read, the defaults are used: {why}");
            (Settings::default(), Some(why))
        }
    }
}

/// Keeps `settings`.
pub fn save(config: &Path, settings: &Settings) -> std::io::Result<()> {
    write_private(
        config,
        "settings.json",
        &serde_json::to_vec_pretty(settings)?,
    )
}

/// Writes `name` in `folder` (made `0700` if it is new) as `0600`, beside and then renamed over
/// the old one, so that it is the old content or the new one, never half of one.
pub fn write_private(folder: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    // A folder that was there already keeps the mode it had: it is made private.
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
    let path = folder.join(name);
    let temp = folder.join(format!("{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if written.is_err() {
        // Not left beside the file kept.
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_out::testing::FakePorts;

    /// A folder of its own in the temporary one, removed on drop: never the real config.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("gamme-config-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn nothing_kept_is_the_small_piano_on_every_input() {
        let config = Temp::new("default");
        let settings = read(&config.0).unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.piano, soundfont::SMALL_PIANO.id);
        assert_eq!(settings.midi_out, None);
        assert_eq!(settings.midi_in, None);
        assert_eq!(settings.touch, Touch::Light);
        assert_eq!(settings.font().id, soundfont::SMALL_PIANO.id);
    }

    #[test]
    fn a_touch_is_kept_with_the_other_settings_as_they_are() {
        let config = Temp::new("touch");
        let kept = Settings {
            piano: soundfont::CONCERT_GRAND.id.into(),
            midi_out: Some("Digital Piano MIDI 1".into()),
            midi_in: Some("Keystation 49".into()),
            touch: Touch::Normal,
        };
        save(&config.0, &kept).unwrap();
        // The touch changes, the rest of what was kept does not.
        let mut read_back = read(&config.0).unwrap();
        read_back.touch = Touch::Heavy;
        save(&config.0, &read_back).unwrap();
        assert_eq!(
            read(&config.0).unwrap(),
            Settings {
                touch: Touch::Heavy,
                ..kept
            }
        );
        let text = std::fs::read_to_string(file(&config.0)).unwrap();
        assert!(text.contains("\"touch\": \"heavy\""), "{text}");
    }

    #[test]
    fn a_touch_missing_or_unknown_is_light_and_the_rest_is_kept() {
        let config = Temp::new("touch-unknown");
        std::fs::create_dir_all(&config.0).unwrap();
        for text in [
            r#"{"piano":"grand","midi_in":"Keystation 49"}"#,
            r#"{"piano":"grand","midi_in":"Keystation 49","touch":"firm"}"#,
            r#"{"piano":"grand","midi_in":"Keystation 49","touch":7}"#,
            r#"{"piano":"grand","midi_in":"Keystation 49","touch":null}"#,
        ] {
            std::fs::write(file(&config.0), text).unwrap();
            let settings = read(&config.0).unwrap();
            assert_eq!(settings.touch, Touch::Light, "{text}");
            assert_eq!(settings.piano, "grand", "{text}");
            assert_eq!(settings.midi_in.as_deref(), Some("Keystation 49"), "{text}");
        }
    }

    #[test]
    fn what_is_chosen_is_kept_and_read_again() {
        let config = Temp::new("roundtrip");
        let chosen = Settings {
            piano: soundfont::CONCERT_GRAND.id.into(),
            midi_out: Some("Digital Piano MIDI 1".into()),
            midi_in: Some("Keystation 49".into()),
            touch: Touch::Heavy,
        };
        save(&config.0, &chosen).unwrap();
        assert_eq!(read(&config.0).unwrap(), chosen);
        assert_eq!(chosen.font().id, soundfont::CONCERT_GRAND.id);
        // A new choice replaces the last, and no temporary file is left.
        let back = Settings::default();
        save(&config.0, &back).unwrap();
        assert_eq!(read(&config.0).unwrap(), back);
        let leftovers: Vec<_> = std::fs::read_dir(&config.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["settings.json"]);
    }

    #[test]
    fn the_folder_is_0700_and_the_file_0600() {
        let config = Temp::new("modes");
        save(&config.0.join("gamme"), &Settings::default()).unwrap();
        assert_eq!(mode(&config.0.join("gamme")), 0o700);
        assert_eq!(mode(&file(&config.0.join("gamme"))), 0o600);
    }

    #[test]
    fn a_folder_that_was_there_with_another_mode_is_made_0700() {
        let config = Temp::new("existing");
        std::fs::create_dir_all(&config.0).unwrap();
        std::fs::set_permissions(&config.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        save(&config.0, &Settings::default()).unwrap();
        assert_eq!(mode(&config.0), 0o700);
    }

    #[test]
    fn a_write_that_fails_leaves_no_temporary_file() {
        let config = Temp::new("failed");
        // The name is a folder: the rename onto it fails after the temporary file was written.
        std::fs::create_dir_all(config.0.join("settings.json")).unwrap();
        assert!(save(&config.0, &Settings::default()).is_err());
        let names: Vec<_> = std::fs::read_dir(&config.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["settings.json"]);
    }

    #[test]
    fn a_key_missing_is_its_default_and_a_file_that_cannot_be_read_is_a_reason() {
        let config = Temp::new("unreadable");
        std::fs::create_dir_all(&config.0).unwrap();
        std::fs::write(file(&config.0), r#"{"midi_in":"Keystation 49"}"#).unwrap();
        assert_eq!(
            read(&config.0).unwrap(),
            Settings {
                midi_in: Some("Keystation 49".into()),
                ..Settings::default()
            }
        );
        // A piano this version does not know is the default one, and is kept as it is.
        std::fs::write(file(&config.0), r#"{"piano":"giant"}"#).unwrap();
        let unknown = read(&config.0).unwrap();
        assert_eq!(unknown.piano, "giant");
        assert_eq!(unknown.font().id, soundfont::SMALL_PIANO.id);
        for text in ["not json", r#"{"midi_out":3}"#, r#"{"piano":null}"#] {
            std::fs::write(file(&config.0), text).unwrap();
            let why = read(&config.0).unwrap_err();
            assert!(why.starts_with("settings.json: "), "{text}: {why}");
        }
    }

    #[test]
    fn a_midi_output_chosen_replaces_the_built_in_piano() {
        let ports = Arc::new(FakePorts::new(&["Digital Piano 20:0"], &[]));
        let (built_in, font) = Settings::default().audio(ports.clone());
        assert_eq!(built_in.label(), None);
        assert!(built_in.needs_soundfont());
        assert_eq!(font.id, soundfont::SMALL_PIANO.id);

        let chosen = Settings {
            piano: soundfont::CONCERT_GRAND.id.into(),
            midi_out: Some("Digital Piano".into()),
            midi_in: None,
            touch: Touch::Fixed,
        };
        let (midi, font) = chosen.audio(ports);
        assert_eq!(midi.label().unwrap(), "MIDI output “Digital Piano”");
        assert!(!midi.needs_soundfont());
        assert!(midi.fallback().is_some(), "the built-in piano stands by");
        assert_eq!(font.id, soundfont::CONCERT_GRAND.id);
    }
}
