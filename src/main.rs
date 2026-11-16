mod desktop;
mod home;
mod icons;
mod library;
mod paths;
mod piece_file;
mod update;

use gpui_kit::*;

const USAGE: &str = "usage: gamme
       gamme update [--channel stable|beta|alpha] [--branch <name>] [--version X.Y.Z[-pre]] [--force]
       gamme --version

Learn the piano: the score at the centre, falling notes as a help that fades with mastery.
`gamme update` replaces this binary with the channel's latest build (`gamme update --help`).";

/// What `--version` prints: the version, the channel and the commit (`build.rs`).
fn version() -> String {
    format!("gamme {}", env!("GAMME_VERSION"))
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
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
        Some(other) => {
            eprintln!("gamme: unknown argument {other} (see gamme --help)");
            return std::process::ExitCode::from(2);
        }
    }
    window();
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

fn window() {
    gpui_kit::application()
        .with_assets(icons::Assets)
        .run(|cx| {
            let desktop = gpui_design_system::omarchy::Omarchy::from_system();
            gpui_design_system::init(
                gpui_design_system::Identity::new("Gamme").desktop(desktop),
                cx,
            );
            let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("gamme".into()),
                ..Default::default()
            };
            let opened = gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| home::Home));
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
