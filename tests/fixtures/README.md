`fake.7z`: a 7z of one file, `fake/fake.sf2` ("RIFF 7z stand-in"), compressed with BZip2 as the
small piano's own archive is, made with
`bsdtar --format 7zip --options 7zip:compression=bzip2 -cf fake.7z fake/fake.sf2`. It stands for
that archive in the SoundFont install tests (`src/soundfont.rs`, `src/player.rs`).

`round-and-back.musicxml`: three measures for two hands, the first two repeated, with a chord, a
sharp and a tie, written by hand for the timeline's tests (`src/timeline.rs`). CC0.

`three-voices.mid`: a conductor track, then three note tracks with no names, the left hand first
(a bass, a melody, an inner voice), for the hands of a MIDI file (`src/timeline.rs`). Written
byte by byte for Gamme's tests, CC0.
