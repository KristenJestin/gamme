mod appearance;
mod clock;
mod config;
mod desktop;
mod engraver;
mod falling;
mod flash;
mod fonts;
mod home;
mod icons;
mod judge;
mod keyboard;
mod latency;
mod library;
mod midi_in;
mod midi_out;
mod paths;
mod piano;
mod piece_file;
mod play_screen;
mod player;
mod practice;
mod progress;
mod raster;
mod run_end;
mod score;
mod score_view;
mod screens;
mod settings;
mod soundfont;
mod stars;
mod tally;
mod theme;
mod timeline;
mod touch;
mod transport;
mod update;
mod wall_clock;

use gpui_kit::*;

const USAGE: &str = "usage: gamme
       gamme <piece> [--at <seconds>] [--play] [--view both|falling|score]
       gamme play <piece> [--piano small|grand]
       gamme update [--channel stable|beta|alpha] [--branch <name>] [--version X.Y.Z[-pre]] [--force]
       gamme --version

Learn the piano: the score at the centre, falling notes as a help that fades with mastery.
`gamme <piece>` opens a piece (MusicXML, MEI or MIDI) without importing it: its score over its
falling notes, or the view --view names, at --at, playing with --play.
`gamme play` opens it as well, its built-in piano chosen: its SoundFont is downloaded the first
time (`--piano grand` is the concert grand, 296 MiB; the default is the settings' choice, a small
upright of 6 MiB at first). Either way the piece sounds on the MIDI output the settings choose,
if they choose one.
`gamme update` replaces this binary with the channel's latest build (`gamme update --help`).";

/// What `--version` prints: the version, the channel and the commit (`build.rs`).
fn version() -> String {
    format!("gamme {}", env!("GAMME_VERSION"))
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut opening = None;
    match args.next().as_deref() {
        None => {}
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return std::process::ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("{}", version());
            return std::process::ExitCode::SUCCESS;
        }
        Some("update") => return update_command(args.collect()),
        // Before a piece: `play` is a command, as `update` is.
        Some("play") => return play_command(args.collect()),
        Some(piece) if !piece.starts_with('-') => {
            match play_screen::Opening::parse(piece.to_owned(), args) {
                Ok(piece) => opening = Some(piece),
                Err(error) => {
                    eprintln!("gamme: {error}");
                    return std::process::ExitCode::from(2);
                }
            }
        }
        Some(other) => {
            eprintln!("gamme: unknown argument {other} (see gamme --help)");
            return std::process::ExitCode::from(2);
        }
    }
    window(opening);
    std::process::ExitCode::SUCCESS
}

fn play_command(args: Vec<String>) -> std::process::ExitCode {
    let fail = |message: String| {
        eprintln!("gamme: {message}");
        std::process::ExitCode::from(2)
    };
    let mut file = None;
    let mut font = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--piano" => match args.next().as_deref() {
                Some("small") => font = Some(soundfont::SMALL_PIANO),
                Some("grand") => font = Some(soundfont::CONCERT_GRAND),
                _ => return fail("--piano is small or grand".into()),
            },
            _ if file.is_none() && !arg.starts_with('-') => file = Some(arg),
            _ => return fail(format!("unknown argument {arg} (see gamme --help)")),
        }
    }
    let Some(file) = file else {
        return fail("gamme play needs a piece (see gamme --help)".into());
    };
    window(Some(play_screen::Opening {
        font,
        ..play_screen::Opening::file(file)
    }));
    std::process::ExitCode::SUCCESS
}

fn update_command(args: Vec<String>) -> std::process::ExitCode {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", update::USAGE);
        return std::process::ExitCode::SUCCESS;
    }
    let request = match update::parse(args) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("gamme: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    update::run(request).unwrap_or_else(|error| {
        eprintln!("gamme: {error}");
        std::process::ExitCode::FAILURE
    })
}

/// Opens the window on the home screen, or on `opening` played.
fn window(opening: Option<play_screen::Opening>) {
    gpui_kit::application()
        .with_assets(icons::Assets)
        .run(|cx| {
            let desktop = gpui_design_system::omarchy::Omarchy::from_system();
            let identity = gpui_design_system::Identity::new("Gamme")
                .themes([gpui_design_system::Theme::new(
                    theme::ID,
                    "Gamme",
                    theme::nocturne,
                )])
                .desktop(desktop);
            gpui_design_system::init(identity, cx);
            fonts::register(cx);
            appearance::restore(cx);
            let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("gamme".into()),
                icon: Some(desktop::window_icon()),
                ..Default::default()
            };
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| screens::Screens::new(opening, window, cx))
            });
            if let Err(error) = opened {
                eprintln!("gamme: the window cannot open: {error}");
                cx.quit();
            }
        });
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's own `test` attribute would come with it.
    use super::version;

    #[test]
    fn the_version_names_its_channel_and_commit() {
        let number = env!("GAMME_VERSION_NUMBER");
        let channel = env!("GAMME_CHANNEL");
        let rest = version()
            .strip_prefix(&format!("gamme {number} ({channel}"))
            .expect("the number, then the channel")
            .to_owned();
        let commit = rest
            .strip_prefix(", ")
            .and_then(|rest| rest.strip_suffix(')'));
        match commit {
            Some(commit) => {
                assert_eq!(commit.len(), 12, "{commit}");
                assert!(commit.bytes().all(|b| b.is_ascii_hexdigit()), "{commit}");
            }
            None => assert_eq!(rest, ")"),
        }
        // A build CI did not make is a local one, at the crate's version.
        if option_env!("GAMME_BUILD_CHANNEL").is_none_or(str::is_empty) {
            assert_eq!(channel, "local");
        }
        if option_env!("GAMME_BUILD_VERSION").is_none_or(str::is_empty) {
            assert_eq!(number, env!("CARGO_PKG_VERSION"));
        }
    }
}
