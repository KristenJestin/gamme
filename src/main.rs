mod appearance;
mod clock;
mod desktop;
mod engraver;
mod home;
mod icons;
mod library;
mod paths;
mod piano;
mod piece_file;
mod player;
mod raster;
mod score;
mod score_view;
mod soundfont;
mod theme;
mod timeline;
mod transport;
mod update;

use std::sync::Arc;

use gpui_kit::*;

const USAGE: &str = "usage: gamme
       gamme <piece> [--at <seconds>] [--play]
       gamme play <piece.mid> [--piano small|grand]
       gamme update [--channel stable|beta|alpha] [--branch <name>] [--version X.Y.Z[-pre]] [--force]
       gamme --version

Learn the piano: the score at the centre, falling notes as a help that fades with mastery.
`gamme <piece>` engraves a MusicXML or MEI piece and shows it, its cursor at --at, moving at the
piece's tempo with --play (until the library opens pieces).
`gamme play` plays a MIDI file with the built-in piano, whose SoundFont is downloaded the first
time (`--piano grand` is the concert grand, 296 MiB; the default is a small upright, 6 MiB).
`gamme update` replaces this binary with the channel's latest build (`gamme update --help`).";

/// What `--version` prints: the version, the channel and the commit (`build.rs`).
fn version() -> String {
    format!("gamme {}", env!("GAMME_VERSION"))
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut screen = Screen::Home;
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
            match score_view::Opening::parse(piece.to_owned(), args) {
                Ok(piece) => screen = Screen::Score(piece),
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
    window(screen);
    std::process::ExitCode::SUCCESS
}

fn play_command(args: Vec<String>) -> std::process::ExitCode {
    let fail = |message: String| {
        eprintln!("gamme: {message}");
        std::process::ExitCode::from(2)
    };
    let mut file = None;
    let mut font = soundfont::SMALL_PIANO;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--piano" => match args.next().as_deref() {
                Some("small") => font = soundfont::SMALL_PIANO,
                Some("grand") => font = soundfont::CONCERT_GRAND,
                _ => return fail("--piano is small or grand".into()),
            },
            _ if file.is_none() && !arg.starts_with('-') => file = Some(arg),
            _ => return fail(format!("unknown argument {arg} (see gamme --help)")),
        }
    }
    let Some(file) = file else {
        return fail("gamme play needs a MIDI file (see gamme --help)".into());
    };
    let Some(data) = paths::data() else {
        return fail("cannot find the data folder: HOME is not set".into());
    };
    let timeline = std::fs::read(&file)
        .map_err(|e| format!("cannot read {file}: {e}"))
        .and_then(|bytes| timeline::Timeline::from_smf(&bytes).map_err(|e| format!("{file}: {e}")));
    let timeline = match timeline {
        Ok(timeline) => timeline,
        Err(message) => return fail(message),
    };
    let title = std::path::Path::new(&file)
        .file_stem()
        .map_or_else(|| file.clone(), |stem| stem.to_string_lossy().into_owned());
    let player = player::Player::new(
        timeline,
        data,
        font,
        Arc::new(soundfont::Curl::new()),
        Arc::new(player::Builtin),
    );
    window(Screen::Play(title, Box::new(player)));
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

/// What the window opens on.
enum Screen {
    Home,
    /// `gamme <piece>`: the piece engraved, with its cursor.
    Score(score_view::Opening),
    /// `gamme play`: a MIDI file played with the transport, under its title.
    Play(String, Box<player::Player>),
}

fn window(screen: Screen) {
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
            appearance::restore(cx);
            let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("gamme".into()),
                icon: Some(desktop::window_icon()),
                ..Default::default()
            };
            let opened = match screen {
                Screen::Home => {
                    gpui_kit::open_window(options, cx, |_, cx| cx.new(home::Home::new)).map(drop)
                }
                Screen::Score(opening) => gpui_kit::open_window(options, cx, |window, cx| {
                    cx.new(|cx| score_view::ScoreView::new(opening, window, cx))
                })
                .map(drop),
                Screen::Play(title, player) => gpui_kit::open_window(options, cx, |_, cx| {
                    cx.new(|cx| transport::Transport::new(title, *player, cx))
                })
                .map(drop),
            };
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
