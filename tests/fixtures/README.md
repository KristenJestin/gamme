`fake.7z`: a 7z of one file, `fake/fake.sf2` ("RIFF 7z stand-in"), compressed with BZip2 as the
small piano's own archive is, made with
`bsdtar --format 7zip --options 7zip:compression=bzip2 -cf fake.7z fake/fake.sf2`. It stands for
that archive in the SoundFont install tests (`src/soundfont.rs`, `src/player.rs`).
