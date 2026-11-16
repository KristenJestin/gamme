//! `gamme update` (`docs/releases.md`, "Updating"): replaces this binary with the latest build of
//! a channel, its SHA-256 sum checked first. It follows `flottille update`.
//!
//! The releases are read from GitHub's API with the user's own `curl` and unpacked with their
//! own `tar`, both spawned without a shell, as git is.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{desktop, paths};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Where the releases are read: GitHub's API for the repository, or, for tests, what
/// `GAMME_RELEASES` names.
fn api() -> String {
    std::env::var("GAMME_RELEASES")
        .unwrap_or_else(|_| "https://api.github.com/repos/KristenJestin/gamme".into())
}

/// The channel this binary was built on (`build.rs`); a local build updates from stable.
const CHANNEL: &str = env!("GAMME_CHANNEL");
/// The branch an alpha was built from, empty for any other build.
const BRANCH: &str = env!("GAMME_BRANCH");
/// This build's version alone, `0.3.0-beta.12`.
const VERSION: &str = env!("GAMME_VERSION_NUMBER");

/// What `gamme update` was asked.
#[derive(Debug, Default, PartialEq)]
pub struct Request {
    pub channel: Option<String>,
    pub branch: Option<String>,
    pub version: Option<String>,
    pub force: bool,
}

pub const USAGE: &str = "usage: gamme update [--channel stable|beta|alpha] [--branch <name>]
                    [--version <X.Y.Z[-pre]>] [--force]

Replaces this binary with the channel's latest build: the one last chosen with --channel, else
the one this build was made on (a local build takes stable). --branch goes with --channel alpha.
--version installs that release exactly, of any channel. --force installs the same version again,
or an older one.";

/// The arguments after `update`; an error says what is wrong with them.
pub fn parse(args: impl IntoIterator<Item = String>) -> std::result::Result<Request, String> {
    let mut request = Request::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--channel" => request.channel = Some(value("--channel")?),
            "--branch" => request.branch = Some(value("--branch")?),
            "--version" => request.version = Some(value("--version")?),
            "--force" => request.force = true,
            other => return Err(format!("unknown option {other} (see gamme update --help)")),
        }
    }
    if let Some(channel) = &request.channel
        && !["stable", "beta", "alpha"].contains(&channel.as_str())
    {
        return Err("the channel is stable, beta or alpha".into());
    }
    if request.version.is_some() && (request.channel.is_some() || request.branch.is_some()) {
        return Err(
            "--version names a release of any channel: it goes without --channel and --branch"
                .into(),
        );
    }
    if request.branch.is_some() && request.channel.as_deref() != Some("alpha") {
        return Err("--branch names an alpha: give it with --channel alpha".into());
    }
    Ok(request)
}

/// The channel last chosen with `--channel`, kept in the state folder (not a setting to edit: it
/// is Gamme's own memory). A plain `gamme update` follows it.
#[derive(Default, Serialize, Deserialize)]
struct Choice {
    channel: Option<String>,
    branch: Option<String>,
}

fn choice_file(state: &Path) -> PathBuf {
    state.join("update-channel.json")
}

/// The remembered choice; none when there is no file, or one that is not ours to read.
fn remembered(state: &Path) -> Choice {
    std::fs::read(choice_file(state))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn remember(state: &Path, choice: &Choice) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)?;
    // Written beside the file and renamed over it, so that the file is the old choice or the new
    // one, never half of one. The process's id keeps two updates from writing the same file.
    let file = choice_file(state);
    let temp = file.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, serde_json::to_vec(choice)?)?;
    std::fs::rename(&temp, &file)?;
    Ok(())
}

pub fn run(request: Request) -> Result<ExitCode> {
    let Request {
        channel,
        branch,
        version,
        force,
    } = request;
    let state = paths::state().ok_or("neither XDG_STATE_HOME nor HOME is set")?;
    let data_home = paths::data_home().ok_or("neither XDG_DATA_HOME nor HOME is set")?;
    let wanted = version.as_deref().map(exact).transpose()?;
    // A version says nothing of the channel: it is neither read from nor written to the memory.
    let chosen = channel.is_some();
    let kept = remembered(&state);
    let channel = channel
        .or(kept.channel.clone())
        .unwrap_or_else(|| match CHANNEL {
            "beta" | "alpha" => CHANNEL.to_owned(),
            _ => "stable".to_owned(),
        });
    let branch = branch.or_else(|| {
        kept.branch
            .filter(|_| kept.channel.as_deref() == Some(&channel))
    });
    if !chosen && wanted.is_none() && kept.channel.is_some() {
        match (channel.as_str(), branch.as_deref()) {
            ("alpha", Some(branch)) => println!(
                "Following the channel last chosen: alpha ({branch}); --channel beta goes back to dev's builds."
            ),
            _ => println!("Following the channel last chosen: {channel}."),
        }
    }
    let url = match &wanted {
        Some(version) => exact_url(version)?,
        None => archive_url(&channel, branch.as_deref())?,
    };
    let name = url.rsplit('/').next().unwrap_or_default().to_owned();
    // What the user asked for, to say it right when it is refused.
    let which = match &wanted {
        Some(_) => "",
        None => &channel,
    };
    if let (Some(new), Ok(this)) = (version_of(&name), Version::parse(VERSION)) {
        if new == this && !force {
            println!("Gamme is already {this}: nothing to update (--force installs it again).");
            // The binary may have moved since the entry was written.
            let followed = std::env::current_exe()
                .and_then(|binary| binary.canonicalize())
                .and_then(|binary| desktop::follow(&data_home, &binary));
            if let Err(error) = followed {
                eprintln!("gamme: the launcher entry was not updated: {error}");
            }
            if chosen {
                remember_choice(&state, &channel, branch.as_deref());
            }
            return Ok(ExitCode::SUCCESS);
        }
        if new < this {
            if !force {
                return Err(if which.is_empty() {
                    format!("{new} is older than this {this}: --force goes back to it.")
                } else {
                    format!(
                        "{new} is the latest of {which}, but older than this {this}: --force goes to it."
                    )
                }
                .into());
            }
            println!("{new} is older than this {this}.");
        }
    }
    let archive = fetch(&url)?;
    let published = String::from_utf8(fetch(&format!("{url}.sha256"))?)?;
    let sum = published.split_whitespace().next().unwrap_or_default();
    if sum != sha256(&archive) {
        return Err(format!(
            "the SHA-256 sum of {name} is not the one published: nothing was replaced"
        )
        .into());
    }
    let binary = std::env::current_exe()?.canonicalize()?;
    let version = replace(&data_home, &binary, &name, &archive)?;
    println!("{} is now {version}.", binary.display());
    if chosen {
        remember_choice(&state, &channel, branch.as_deref());
    }
    Ok(ExitCode::SUCCESS)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The binary is in place: a memory that cannot be written is said, not undone.
fn remember_choice(state: &Path, channel: &str, branch: Option<&str>) {
    let choice = Choice {
        channel: Some(channel.to_owned()),
        branch: branch.filter(|_| channel == "alpha").map(str::to_owned),
    };
    if let Err(error) = remember(state, &choice) {
        eprintln!("gamme: the channel was not remembered: {error}");
    }
}

/// `text` as the exact version of a release: a semantic version, nothing else (it is written
/// into an address).
fn exact(text: &str) -> Result<Version> {
    match Version::parse(text) {
        Ok(version) if version.build.is_empty() && version.to_string() == text => Ok(version),
        _ => Err(format!("{text} is not a version (X.Y.Z, or X.Y.Z-pre)").into()),
    }
}

/// The archive of exactly this version; when there is none, the error names the versions
/// published nearest to it.
fn exact_url(version: &Version) -> Result<String> {
    let name = format!("gamme-{version}-x86_64-linux.tar.gz");
    let tagged = json(&format!("{}/releases/tags/v{version}", api()))?;
    let found = tagged
        .iter()
        .flat_map(archives)
        .find(|url| url.ends_with(&format!("/{name}")));
    if let Some(url) = found {
        return Ok(url);
    }
    let mut known: Vec<Version> = json(&format!("{}/releases?per_page=100", api()))?
        .unwrap_or_default()
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(archives)
        .filter_map(|url| version_of(url.rsplit('/').next()?))
        .collect();
    known.sort();
    known.dedup();
    let at = known.partition_point(|v| v < version);
    let close: Vec<String> = known
        .iter()
        .skip(at.saturating_sub(2))
        .take(5)
        .map(Version::to_string)
        .collect();
    let mut said = format!("there is no release {version} for Linux x86_64");
    if !close.is_empty() {
        said.push_str(&format!(" (close versions: {})", close.join(", ")));
    }
    Err(said.into())
}

/// The archive of the release published last among these (ties go to the higher version): a
/// channel's latest is the newest build, not the highest number, which a numbering change can
/// leave behind. The dates are UTC and all of one shape, so they compare as text. A draft has no
/// date and is not published: it is not offered.
fn latest_published(releases: &[Value]) -> Option<String> {
    releases
        .iter()
        .filter_map(|release| Some((release["published_at"].as_str()?, release)))
        .flat_map(|(at, release)| {
            archives(release)
                .into_iter()
                .filter_map(move |url| Some(((at, version_of(url.rsplit('/').next()?)?), url)))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, url)| url)
}

/// The archive of the highest version among the alphas of the branch whose slug this is, in
/// these releases: `*-alpha.<slug>.<n>`, the slug whole (`feat-a` is not `feat-a-b`), each alpha
/// being a release of its own. A draft has no date and is not published: it is not offered.
fn highest_alpha(releases: &[Value], slug: &str) -> Option<String> {
    releases
        .iter()
        .filter(|release| release["published_at"].as_str().is_some())
        .flat_map(archives)
        .filter_map(|url| Some((version_of(url.rsplit('/').next()?)?, url)))
        .filter(|(version, _)| {
            version
                .pre
                .as_str()
                .strip_prefix("alpha.")
                .and_then(|rest| rest.rsplit_once('.'))
                .is_some_and(|(of, n)| of == slug && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, url)| url)
}

/// The version an archive is of, `0.3.0-beta.12` in `gamme-0.3.0-beta.12-x86_64-linux.tar.gz`.
fn version_of(name: &str) -> Option<Version> {
    let version = name
        .strip_prefix("gamme-")?
        .strip_suffix("-x86_64-linux.tar.gz")?;
    Version::parse(version).ok()
}

/// The addresses of the archives for this machine that a release holds.
fn archives(release: &Value) -> Vec<String> {
    release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["browser_download_url"].as_str())
        .filter(|url| url.ends_with("-x86_64-linux.tar.gz"))
        .map(str::to_owned)
        .collect()
}

/// The slug of a branch's name, as CI names its alphas: each byte but ASCII letters, digits and
/// dashes is a dash.
fn slug(branch: &str) -> String {
    branch
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                b as char
            } else {
                '-'
            }
        })
        .collect()
}

/// The address of the channel's latest archive for this machine.
fn archive_url(channel: &str, branch: Option<&str>) -> Result<String> {
    let found = match channel {
        "stable" => {
            let release = json(&format!("{}/releases/latest", api()))?
                .ok_or("there is no stable release yet")?;
            archives(&release).into_iter().next()
        }
        "beta" => latest_published(
            &json(&format!("{}/releases?per_page=100", api()))?
                .unwrap_or_default()
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["tag_name"].as_str().is_some_and(|t| t.contains("-beta.")))
                .cloned()
                .collect::<Vec<_>>(),
        ),
        "alpha" => {
            let branch = branch
                .or(Some(BRANCH).filter(|b| !b.is_empty()))
                .ok_or("the alpha channel needs --branch <name>")?;
            let releases = json(&format!("{}/releases?per_page=100", api()))?.unwrap_or_default();
            let Some(url) = highest_alpha(
                releases.as_array().map_or(&[], Vec::as_slice),
                &slug(branch),
            ) else {
                return Err(format!(
                    "there is no alpha release for {branch}: --channel beta (or another --branch) \
                     chooses another channel."
                )
                .into());
            };
            Some(url)
        }
        _ => return Err("the channel is stable, beta or alpha".into()),
    };
    found.ok_or_else(|| format!("there is no {channel} release for Linux x86_64").into())
}

/// The API's answer at `url`; none when it is not there (404), which is not the failure of the
/// network or of GitHub.
fn json(url: &str) -> Result<Option<Value>> {
    // The status follows the body, on a line of its own.
    let body = curl(url, &["--write-out", "\n%{http_code}"])?;
    let text = String::from_utf8(body)?;
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    match code {
        "200" => Ok(Some(serde_json::from_str(body)?)),
        "404" => Ok(None),
        code => Err(format!("cannot read {url}: GitHub answered {code}").into()),
    }
}

/// The file at `url`.
fn fetch(url: &str) -> Result<Vec<u8>> {
    curl(url, &["--fail"])
}

/// What the user's `curl` reads at `url`.
fn curl(url: &str, options: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("curl")
        .args(options)
        .args(["--silent", "--show-error", "--location", "--", url])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot read {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(output.stdout)
}

/// Unpacks the archive beside the binary and renames its `gamme` over it: the binary is the old
/// one or the new one, never half of one, and nothing is left beside it. The launcher's files of
/// the archive are written too. The version the archive is of.
fn replace(data_home: &Path, binary: &Path, name: &str, archive: &[u8]) -> Result<String> {
    let dir = binary.parent().ok_or("the binary has no folder")?;
    let work = tempfile_dir(dir)?;
    let unpacked = (|| -> Result<String> {
        let file = work.join(name);
        std::fs::write(&file, archive)?;
        let status = Command::new("tar")
            .args(["-xzf"])
            .arg(&file)
            .arg("-C")
            .arg(&work)
            .stdin(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(format!("cannot unpack {name}").into());
        }
        let folder = name.trim_end_matches("-x86_64-linux.tar.gz");
        std::fs::rename(work.join(folder).join("gamme"), binary)?;
        // The binary is replaced: failing to write the launcher's files is said, not undone.
        if let Err(error) = desktop::write(data_home, &work.join(folder).join("share"), binary) {
            eprintln!("gamme: the launcher entry was not updated: {error}");
        }
        Ok(folder.trim_start_matches("gamme-").to_owned())
    })();
    let _ = std::fs::remove_dir_all(&work);
    unpacked
}

/// A folder of its own beside the binary, so that the rename stays on one file system.
fn tempfile_dir(dir: &Path) -> Result<PathBuf> {
    for n in 0..100 {
        let path = dir.join(format!(".gamme-update-{}-{n}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err("cannot make a folder beside the binary".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A release of `version`, published at `at` (UTC, one shape; none for a draft).
    fn release(version: &str, at: Option<&str>) -> Value {
        json!({
            "tag_name": format!("v{version}"),
            "published_at": at,
            "assets": [
                {"browser_download_url": format!("https://example.test/gamme-{version}-x86_64-linux.tar.gz")},
                {"browser_download_url": format!("https://example.test/gamme-{version}-x86_64-linux.tar.gz.sha256")},
            ],
        })
    }

    fn version_taken(url: Option<String>) -> Option<String> {
        Some(version_of(url?.rsplit('/').next()?)?.to_string())
    }

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn an_archive_name_gives_its_version() {
        let name = "gamme-0.3.0-beta.12-x86_64-linux.tar.gz";
        assert_eq!(
            version_of(name),
            Some(Version::parse("0.3.0-beta.12").unwrap())
        );
        assert_eq!(version_of("gamme-0.3.0-x86_64-linux.tar.gz.sha256"), None);
        assert_eq!(version_of("rature-0.3.0-x86_64-linux.tar.gz"), None);
    }

    #[test]
    fn only_archives_for_this_machine_are_offered() {
        let release = json!({"assets": [
            {"browser_download_url": "https://x.test/gamme-1.0.0-x86_64-linux.tar.gz"},
            {"browser_download_url": "https://x.test/gamme-1.0.0-x86_64-linux.tar.gz.sha256"},
            {"browser_download_url": "https://x.test/gamme-1.0.0-aarch64-linux.tar.gz"},
        ]});
        assert_eq!(
            archives(&release),
            ["https://x.test/gamme-1.0.0-x86_64-linux.tar.gz"]
        );
    }

    #[test]
    fn the_beta_is_the_one_published_last_never_a_draft() {
        let releases = [
            release("0.2.0-beta.13", None),
            release("0.1.2-beta.3", Some("2026-11-10T09:00:00Z")),
            release("0.2.0-beta.12", Some("2026-11-08T09:00:00Z")),
        ];
        assert_eq!(
            version_taken(latest_published(&releases)).as_deref(),
            Some("0.1.2-beta.3"),
            "published last, not the highest, not the draft"
        );
    }

    #[test]
    fn an_alpha_is_the_highest_of_its_own_branch() {
        let releases = [
            release("0.2.0-alpha.feat-a-b.3", Some("2026-11-10T12:00:00Z")),
            release("0.2.0-alpha.feat-a-b.10", Some("2026-11-09T10:00:00Z")),
            release("0.2.0-alpha.feat-a-b-c.20", Some("2026-11-09T12:00:00Z")),
            release("0.2.0-alpha.feat-a.30", Some("2026-11-09T11:00:00Z")),
            release("0.2.0-alpha.feat-a.31", None),
        ];
        let of = |branch: &str| version_taken(highest_alpha(&releases, &slug(branch)));
        assert_eq!(of("feat/a_b").as_deref(), Some("0.2.0-alpha.feat-a-b.10"));
        assert_eq!(of("feat/a").as_deref(), Some("0.2.0-alpha.feat-a.30"));
        assert_eq!(of("feat/none"), None);
    }

    #[test]
    fn a_branch_slug_is_made_as_ci_makes_it() {
        assert_eq!(slug("feat/12-search"), "feat-12-search");
        assert_eq!(slug("fix/a_b.c"), "fix-a-b-c");
    }

    #[test]
    fn an_exact_version_is_a_semantic_version_and_nothing_else() {
        assert!(exact("0.1.0").is_ok());
        assert!(exact("0.2.0-beta.12").is_ok());
        for bad in ["1.2", "v1.2.3", "01.2.3", "1.2.3/../x", "1.2.3+build", ""] {
            assert!(exact(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_sum_is_written_as_sha256sum_does() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_arguments_are_checked_before_anything_is_asked() {
        assert_eq!(parse(args("")).unwrap(), Request::default());
        let beta = parse(args("--channel beta --force")).unwrap();
        assert_eq!(beta.channel.as_deref(), Some("beta"));
        assert!(beta.force);
        let alpha = parse(args("--channel alpha --branch feat/x")).unwrap();
        assert_eq!(alpha.branch.as_deref(), Some("feat/x"));
        for bad in [
            "--channel",
            "--channel nightly",
            "--branch feat/x",
            "--channel beta --branch feat/x",
            "--version 0.1.0 --channel beta",
            "--version 0.1.0 --branch feat/x",
            "--bogus",
        ] {
            assert!(parse(args(bad)).is_err(), "{bad}");
        }
    }
}
