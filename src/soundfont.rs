//! The SoundFonts of the built-in piano: which ones, and how one is fetched on first use into
//! `$XDG_DATA_HOME/gamme/soundfonts`.
//!
//! A download is resumable: it goes to `<id>.part` by HTTP `Range` from what that file already
//! holds, is checked (size, then SHA-256) before anything is unpacked, and a corrupt file is
//! thrown away and fetched once more from the start. The network is behind [`Fetcher`] (the real
//! one runs the user's `curl`, as `update.rs` does), so the logic is tested with a fake.
//!
//! The folders are `0700` and the files `0600`, and an install holds a lock on a file of its own,
//! so that two Gammes never resume the same `.part`. The children it runs (`curl`, `tar`) die
//! with the thread that started them, and stop when the install is cancelled.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};
use std::time::Duration;

use sha2::{Digest as _, Sha256};

/// How a SoundFont is packed on its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Archive {
    /// `.7z`, unpacked by `sevenz-rust2` (no tool of the machine reads it).
    SevenZip,
    /// `.tar.xz`, unpacked by the user's `tar`: Gamme has no `xz` decoder of its own.
    Tar,
}

#[derive(Clone, Copy, Debug)]
pub struct Font {
    /// The file's name in the data folder, and the key of the choice in Settings.
    pub id: &'static str,
    pub name: &'static str,
    pub author: &'static str,
    pub licence: &'static str,
    pub url: &'static str,
    pub archive: Archive,
    pub archive_bytes: u64,
    pub archive_sha256: &'static str,
    /// The `.sf2` inside the archive.
    pub member: &'static str,
    /// Its size once unpacked: what tells a finished install from a half one.
    pub bytes: u64,
}

/// The default: an upright Kawai (FreePats), 5.8 MiB to fetch, 9 MiB on disk, public domain.
pub const SMALL_PIANO: Font = Font {
    id: "upright-piano-small",
    name: "Upright piano (small)",
    author: "Gonzalo and Roberto, FreePats (Upright Piano KW)",
    licence: "CC0 1.0",
    url: "https://freepats.zenvoid.org/Piano/UprightPianoKW/UprightPianoKW-small-SF2-20190703.7z",
    archive: Archive::SevenZip,
    archive_bytes: 6_093_984,
    archive_sha256: "3bd025e7c2ffa9e6f3f99215ce383c9cebbac991cfbf9be1453cdb4328ec3492",
    member: "UprightPianoKW-small-SF2-20190703/UprightPianoKW-small-20190703.sf2",
    bytes: 9_456_310,
};

/// The option: the Salamander Grand (a Yamaha C5, 16 velocity layers), 296 MiB to fetch and
/// 1.2 GiB on disk and in memory once loaded.
pub const CONCERT_GRAND: Font = Font {
    id: "salamander-grand",
    name: "Concert grand",
    author: "Alexander Holm (Salamander Grand Piano), packed by FreePats",
    licence: "CC BY 3.0",
    url: "https://freepats.zenvoid.org/Piano/SalamanderGrandPiano/SalamanderGrandPiano-SF2-V3+20200602.tar.xz",
    archive: Archive::Tar,
    archive_bytes: 310_397_984,
    archive_sha256: "15edb061d7ba60d58332f72dba8f8ce40988048cc703f935e6320f37d650e213",
    member: "SalamanderGrandPiano-SF2-V3+20200602/SalamanderGrandPiano-V3+20200602.sf2",
    bytes: 1_266_462_334,
};

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Settings lists the pianos (#16)")
)]
pub const FONTS: [Font; 2] = [SMALL_PIANO, CONCERT_GRAND];

impl Font {
    fn folder(data: &Path) -> PathBuf {
        data.join("soundfonts")
    }

    /// Where the unpacked SoundFont lives, once it does.
    pub fn path(&self, data: &Path) -> PathBuf {
        Self::folder(data).join(format!("{}.sf2", self.id))
    }

    fn partial(&self, data: &Path) -> PathBuf {
        Self::folder(data).join(format!("{}.part", self.id))
    }

    fn unpacked(&self, data: &Path) -> PathBuf {
        Self::folder(data).join(format!("{}.unpack", self.id))
    }

    /// Whether the SoundFont is there, whole.
    pub fn installed(&self, data: &Path) -> bool {
        fs::metadata(self.path(data)).is_ok_and(|meta| meta.is_file() && meta.len() == self.bytes)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => f.write_str("the download was cancelled"),
            Error::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Failed(error.to_string())
    }
}

fn failed(message: impl Into<String>) -> Error {
    Error::Failed(message.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Downloading,
    Verifying,
    Unpacking,
    Ready,
}

/// What the window shows of an install while it runs on another thread, and its way to stop it.
/// Atomics: the window reads it every frame.
#[derive(Default)]
pub struct Progress {
    stage: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn stage(&self) -> Stage {
        match self.stage.load(Relaxed) {
            1 => Stage::Downloading,
            2 => Stage::Verifying,
            3 => Stage::Unpacking,
            4 => Stage::Ready,
            _ => Stage::Idle,
        }
    }

    fn set_stage(&self, stage: Stage) {
        self.stage.store(stage as u8, Relaxed);
    }

    /// Bytes downloaded so far, and the bytes to download in all.
    pub fn bytes(&self) -> (u64, u64) {
        (self.done.load(Relaxed), self.total.load(Relaxed))
    }

    pub fn set_downloaded(&self, bytes: u64) {
        self.done.store(bytes, Relaxed);
    }

    /// Asks the install to stop at its next look; the partial file is kept for next time.
    pub fn cancel(&self) {
        self.cancelled.store(true, Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Relaxed)
    }

    /// Back to the beginning, for a new install.
    pub fn reset(&self) {
        self.set_stage(Stage::Idle);
        self.done.store(0, Relaxed);
        self.total.store(0, Relaxed);
        self.cancelled.store(false, Relaxed);
    }
}

/// What the network does for an install.
pub trait Fetcher: Send + Sync {
    /// Brings the body of `url` to the end of `partial`: from the length that file already has
    /// (an HTTP `Range` request), from the start when it is not there or the server does not
    /// take ranges. Calls `progress.set_downloaded` as the file grows and returns
    /// [`Error::Cancelled`] when `progress.cancelled()`. A failure leaves what was received in
    /// `partial`.
    fn fetch(&self, url: &str, partial: &Path, progress: &Progress) -> Result<(), Error>;
}

/// The user's `curl`, as `update.rs` uses it: `-C -` continues a file, by `Range`.
pub struct Curl {
    /// What `--proto` and `--proto-redir` allow: only HTTPS, outside the tests.
    protocols: &'static str,
    /// Tests: a folder that stands for the home, so that the user's `~/.curlrc` and proxy
    /// variables do not reach `curl`.
    isolated: Option<PathBuf>,
}

impl Curl {
    pub fn new() -> Self {
        Self {
            protocols: "=https",
            isolated: None,
        }
    }
}

impl Default for Curl {
    fn default() -> Self {
        Self::new()
    }
}

/// Makes `command`'s child die when the thread that starts it does (the app closing, a crash):
/// a download or an unpacking must not outlive Gamme.
fn die_with_parent(command: &mut Command) {
    // SAFETY: `prctl` is async-signal-safe and touches no memory of this process.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Waits for `child`, a hundred milliseconds at a time: it is killed when the install is
/// cancelled, and when it cannot be waited for. `partial` is a file whose growth is the
/// progress.
fn wait(
    child: &mut Child,
    progress: &Progress,
    partial: Option<&Path>,
) -> Result<ExitStatus, Error> {
    loop {
        if let Some(partial) = partial {
            progress.set_downloaded(fs::metadata(partial).map_or(0, |meta| meta.len()));
        }
        if progress.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
    }
}

/// What the child wrote on its error output.
fn said_by(child: &mut Child) -> String {
    let mut message = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut message);
    }
    let lines: Vec<_> = message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines.join(" / ").chars().take(300).collect()
}

impl Fetcher for Curl {
    fn fetch(&self, url: &str, partial: &Path, progress: &Progress) -> Result<(), Error> {
        for attempt in 0..2 {
            let mut command = Command::new("curl");
            if let Some(home) = &self.isolated {
                command
                    .arg("-q")
                    .env("HOME", home)
                    .env("CURL_HOME", home)
                    .env("NO_PROXY", "*")
                    .env("no_proxy", "*");
            }
            command
                .args(["--silent", "--show-error", "--location", "--fail"])
                .args(["--proto", self.protocols, "--proto-redir", self.protocols])
                .args([
                    "--connect-timeout",
                    "20",
                    "--retry",
                    "3",
                    "--retry-delay",
                    "2",
                ])
                // A link that gives under 1 KiB/s for half a minute is dead: stop, keep the part.
                .args(["--speed-limit", "1024", "--speed-time", "30"])
                .args(["--continue-at", "-", "--output"])
                .arg(partial)
                .args(["--", url])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            die_with_parent(&mut command);
            let mut child = command
                .spawn()
                .map_err(|e| failed(format!("cannot run curl: {e}")))?;
            let status = wait(&mut child, progress, Some(partial))?;
            progress.set_downloaded(fs::metadata(partial).map_or(0, |meta| meta.len()));
            if status.success() {
                return Ok(());
            }
            // 33: the server does not take ranges, so the part cannot be continued: start over.
            if status.code() == Some(33) && attempt == 0 {
                let _ = fs::remove_file(partial);
                continue;
            }
            let message = said_by(&mut child);
            return Err(failed(format!(
                "cannot download {url}: {}",
                if message.is_empty() {
                    "curl failed"
                } else {
                    &message
                }
            )));
        }
        unreachable!("the second attempt returns");
    }
}

fn sha256_of(path: &Path, progress: &Progress) -> Result<String, Error> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        if progress.cancelled() {
            return Err(Error::Cancelled);
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Makes sure `font` is in `data`'s `soundfonts` folder, fetching it when it is not. Slow:
/// run it off the main thread; `progress` says where it is. The path of the `.sf2`.
pub fn install(
    font: &Font,
    data: &Path,
    fetcher: &dyn Fetcher,
    progress: &Progress,
) -> Result<PathBuf, Error> {
    let target = font.path(data);
    let folder = Font::folder(data);
    // 0700, as everything Gamme keeps.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)?;
    // One install of a font at a time, across processes. The lock is let go of when the install
    // ends, not only closed: a program spawned on another thread holds a copy of every open file
    // between its fork and its exec, and the lock (flock) is shared by all of the file's copies.
    let lock = private_file(&folder.join(format!("{}.lock", font.id)))?;
    let _held = match lock.try_lock() {
        Ok(()) => Unlock(lock),
        Err(fs::TryLockError::WouldBlock) => {
            return Err(failed(format!(
                "the {} is already being downloaded by another Gamme",
                font.name
            )));
        }
        Err(fs::TryLockError::Error(error)) => return Err(error.into()),
    };
    if font.installed(data) {
        // What an earlier install left beside it is of no use.
        let _ = fs::remove_file(font.partial(data));
        let _ = fs::remove_dir_all(font.unpacked(data));
        progress.set_stage(Stage::Ready);
        return Ok(target);
    }
    let result = install_inner(font, data, fetcher, progress);
    progress.set_stage(if result.is_ok() {
        Stage::Ready
    } else {
        Stage::Idle
    });
    result
}

/// A file locked, unlocked when it goes.
struct Unlock(File);

impl Drop for Unlock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn install_inner(
    font: &Font,
    data: &Path,
    fetcher: &dyn Fetcher,
    progress: &Progress,
) -> Result<PathBuf, Error> {
    let (target, partial) = (font.path(data), font.partial(data));
    progress.total.store(font.archive_bytes, Relaxed);

    // A part longer than the archive is not one of its beginnings.
    if fs::metadata(&partial).is_ok_and(|meta| meta.len() > font.archive_bytes) {
        fs::remove_file(&partial)?;
    }
    for attempt in 0..2 {
        // The part is made here, `0600`, rather than by the fetcher with the umask's modes.
        private_file(&partial)?;
        let have = fs::metadata(&partial).map_or(0, |meta| meta.len());
        progress.set_downloaded(have);
        if have < font.archive_bytes {
            progress.set_stage(Stage::Downloading);
            fetcher.fetch(font.url, &partial, progress)?;
        }
        progress.set_stage(Stage::Verifying);
        let size_is_right = fs::metadata(&partial).is_ok_and(|m| m.len() == font.archive_bytes);
        if size_is_right && sha256_of(&partial, progress)? == font.archive_sha256 {
            break;
        }
        // Not what was published (a part from another version, a cut that went unseen): once
        // from the start, then it is the server's fault, not the part's.
        fs::remove_file(&partial)?;
        progress.set_downloaded(0);
        if attempt == 1 {
            return Err(failed(format!(
                "the {} download is not the published file (checked by its size and sum)",
                font.name
            )));
        }
    }

    progress.set_stage(Stage::Unpacking);
    let unpacked = font.unpacked(data);
    let _ = fs::remove_dir_all(&unpacked);
    fs::DirBuilder::new().mode(0o700).create(&unpacked)?;
    let result = unpack(font, &partial, &unpacked, progress).and_then(|member| {
        // Only a plain file of the right size, whatever the archive held (a link, say).
        let is_font = fs::symlink_metadata(&member)
            .is_ok_and(|meta| meta.is_file() && meta.len() == font.bytes);
        if !is_font {
            return Err(failed(format!(
                "the {} archive is not what was expected",
                font.name
            )));
        }
        fs::set_permissions(&member, fs::Permissions::from_mode(0o600))?;
        // On disk before it has its name: never half of a font under it, even after a crash.
        File::open(&member)?.sync_all()?;
        fs::rename(&member, &target)?;
        Ok(())
    });
    let _ = fs::remove_dir_all(&unpacked);
    result?;
    let _ = fs::remove_file(&partial);
    Ok(target)
}

/// A file of this program's own, `0600`, made if it is not there.
fn private_file(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Takes the font out of `archive` into `into`, and nothing else; the path it is at.
fn unpack(font: &Font, archive: &Path, into: &Path, progress: &Progress) -> Result<PathBuf, Error> {
    match font.archive {
        Archive::SevenZip => {
            // A fixed name, never the archive's, and never more bytes than the font has.
            let out = into.join("font.sf2");
            let mut found = false;
            let mut cancelled = false;
            let result =
                sevenz_rust2::decompress_file_with_extract_fn(archive, into, |entry, reader, _| {
                    if entry.is_directory() || entry.name() != font.member {
                        return Ok(true);
                    }
                    let mut file = private_file(&out)?;
                    let mut buffer = vec![0; 1 << 20];
                    let mut copied = 0u64;
                    loop {
                        if progress.cancelled() {
                            cancelled = true;
                            return Err(io::Error::other("cancelled").into());
                        }
                        let read = reader.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        copied += read as u64;
                        if copied > font.bytes {
                            return Err(io::Error::other("the font is bigger than expected").into());
                        }
                        file.write_all(&buffer[..read])?;
                    }
                    found = true;
                    Ok(false)
                });
            if cancelled {
                return Err(Error::Cancelled);
            }
            result.map_err(|e| failed(format!("cannot unpack {}: {e}", font.name)))?;
            if !found {
                return Err(failed(format!("the {} archive holds no font", font.name)));
            }
            Ok(out)
        }
        Archive::Tar => {
            let mut command = Command::new("tar");
            command
                .args(["--no-same-owner", "-xf"])
                .arg(archive)
                .arg("-C")
                .arg(into)
                .arg(font.member)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            die_with_parent(&mut command);
            let mut child = command
                .spawn()
                .map_err(|e| failed(format!("cannot run tar: {e}")))?;
            let status = wait(&mut child, progress, None)?;
            if !status.success() {
                return Err(failed(format!(
                    "cannot unpack {}: tar says {}",
                    font.name,
                    said_by(&mut child)
                )));
            }
            Ok(into.join(font.member))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader};
    use std::net::TcpListener;
    use std::sync::Mutex;

    use super::*;

    /// A folder of its own, removed after.
    pub struct Temp(pub PathBuf);

    impl Temp {
        pub fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("gamme-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Temp(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A `.tar` holding `member` (the stand-in of a SoundFont), made by the user's `tar`.
    fn tar_of(folder: &Path, member: &str, content: &[u8]) -> Vec<u8> {
        let tree = folder.join("tree");
        let path = tree.join(member);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        let archive = folder.join("fake.tar");
        let status = Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&tree)
            .arg(member)
            .status()
            .unwrap();
        assert!(status.success());
        fs::read(archive).unwrap()
    }

    fn leaked(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    /// A font of the `tar` kind whose archive is `archive` and whose member is `content`.
    fn font_of(archive: &[u8], content: &[u8]) -> Font {
        Font {
            id: "fake",
            name: "Fake piano",
            author: "nobody",
            licence: "CC0 1.0",
            url: "https://example.test/fake.tar",
            archive: Archive::Tar,
            archive_bytes: archive.len() as u64,
            archive_sha256: leaked(hex(archive)),
            member: "fake/fake.sf2",
            bytes: content.len() as u64,
        }
    }

    /// A server in memory: it takes ranges, and can be cut off after a number of bytes.
    struct Fake {
        body: Vec<u8>,
        cut_after: Mutex<Option<usize>>,
        ranges: Mutex<Vec<u64>>,
        takes_ranges: bool,
    }

    impl Fake {
        fn new(body: Vec<u8>) -> Self {
            Self {
                body,
                cut_after: Mutex::new(None),
                ranges: Mutex::new(Vec::new()),
                takes_ranges: true,
            }
        }
    }

    impl Fetcher for Fake {
        fn fetch(&self, _: &str, partial: &Path, progress: &Progress) -> Result<(), Error> {
            let mut have = fs::metadata(partial).map_or(0, |meta| meta.len());
            if !self.takes_ranges {
                have = 0;
            }
            self.ranges.lock().unwrap().push(have);
            let mut rest = &self.body[have as usize..];
            if let Some(cut) = self.cut_after.lock().unwrap().take() {
                rest = &rest[..cut.min(rest.len())];
                let mut file = fs::OpenOptions::new()
                    .create(true)
                    .append(have > 0)
                    .write(true)
                    .truncate(have == 0)
                    .open(partial)?;
                file.write_all(rest)?;
                progress.set_downloaded(have + rest.len() as u64);
                return Err(failed("connection reset"));
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(have > 0)
                .write(true)
                .truncate(have == 0)
                .open(partial)?;
            file.write_all(rest)?;
            progress.set_downloaded(self.body.len() as u64);
            Ok(())
        }
    }

    #[test]
    fn the_catalog_names_two_pianos_with_a_licence_that_allows_downloading_them() {
        assert_eq!(FONTS.len(), 2);
        for font in FONTS {
            assert!(font.url.starts_with("https://"), "{}", font.id);
            assert_eq!(font.archive_sha256.len(), 64, "{}", font.id);
            assert!(font.member.ends_with(".sf2"), "{}", font.id);
            assert!(font.licence.starts_with("CC"), "{}", font.id);
        }
        const { assert!(SMALL_PIANO.archive_bytes < CONCERT_GRAND.archive_bytes) };
    }

    #[test]
    fn a_first_use_downloads_checks_unpacks_and_installs() {
        let temp = Temp::new("install");
        let content = b"RIFFsf2 stand-in".as_slice();
        let archive = tar_of(&temp.0, "fake/fake.sf2", content);
        let font = font_of(&archive, content);
        let fake = Fake::new(archive);
        let data = temp.0.join("data");
        let progress = Progress::default();

        assert!(!font.installed(&data));
        let path = install(&font, &data, &fake, &progress).unwrap();
        assert_eq!(path, data.join("soundfonts/fake.sf2"));
        assert_eq!(fs::read(&path).unwrap(), content);
        assert!(font.installed(&data));
        assert_eq!(progress.stage(), Stage::Ready);
        assert_eq!(progress.bytes().0, progress.bytes().1);
        // Nothing is left beside it.
        // The lock file stays: it costs nothing and is what keeps two installs apart.
        let mut left: Vec<_> = fs::read_dir(data.join("soundfonts"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["fake.lock", "fake.sf2"]);

        // The second use does not touch the network.
        let again = Fake::new(Vec::new());
        install(&font, &data, &again, &Progress::default()).unwrap();
        assert!(again.ranges.lock().unwrap().is_empty());
    }

    #[test]
    fn a_cut_download_resumes_from_what_it_has() {
        let temp = Temp::new("resume");
        let content = vec![7u8; 4000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let font = font_of(&archive, &content);
        let fake = Fake::new(archive.clone());
        *fake.cut_after.lock().unwrap() = Some(3000);
        let data = temp.0.join("data");
        let progress = Progress::default();

        let error = install(&font, &data, &fake, &progress).unwrap_err();
        assert_eq!(error, failed("connection reset"));
        assert!(!font.installed(&data));
        let partial = data.join("soundfonts/fake.part");
        assert_eq!(fs::metadata(&partial).unwrap().len(), 3000);
        assert_eq!(progress.bytes().0, 3000);

        install(&font, &data, &fake, &progress).unwrap();
        assert_eq!(*fake.ranges.lock().unwrap(), [0, 3000]);
        assert!(font.installed(&data));
        assert!(!partial.exists());
    }

    #[test]
    fn a_part_that_is_not_the_beginning_of_the_archive_is_dropped_and_fetched_again() {
        let temp = Temp::new("corrupt");
        let content = vec![1u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let font = font_of(&archive, &content);
        let data = temp.0.join("data");
        fs::create_dir_all(data.join("soundfonts")).unwrap();
        // The right length, the wrong bytes: it passes the size and fails the sum.
        fs::write(data.join("soundfonts/fake.part"), vec![0u8; archive.len()]).unwrap();

        let fake = Fake::new(archive);
        install(&font, &data, &fake, &Progress::default()).unwrap();
        assert_eq!(*fake.ranges.lock().unwrap(), [0]);
        assert!(font.installed(&data));
    }

    #[test]
    fn a_server_that_ignores_ranges_gives_the_whole_file_again() {
        let temp = Temp::new("no-ranges");
        let content = vec![3u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let font = font_of(&archive, &content);
        let data = temp.0.join("data");
        fs::create_dir_all(data.join("soundfonts")).unwrap();
        fs::write(data.join("soundfonts/fake.part"), &archive[..100]).unwrap();

        let mut fake = Fake::new(archive);
        fake.takes_ranges = false;
        install(&font, &data, &fake, &Progress::default()).unwrap();
        assert!(font.installed(&data));
    }

    #[test]
    fn what_never_matches_the_published_sum_fails_after_one_new_try() {
        let temp = Temp::new("bad-sum");
        let content = vec![3u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let mut font = font_of(&archive, &content);
        font.archive_sha256 = "0".repeat(64).leak();
        let data = temp.0.join("data");
        let fake = Fake::new(archive);
        let error = install(&font, &data, &fake, &Progress::default()).unwrap_err();
        assert!(matches!(error, Error::Failed(m) if m.contains("not the published file")));
        assert_eq!(fake.ranges.lock().unwrap().len(), 2);
        assert!(!font.installed(&data));
        assert!(!data.join("soundfonts/fake.part").exists());
    }

    #[test]
    fn a_cancelled_install_keeps_its_part() {
        struct Cancelling;
        impl Fetcher for Cancelling {
            fn fetch(&self, _: &str, partial: &Path, progress: &Progress) -> Result<(), Error> {
                fs::write(partial, b"abc")?;
                progress.cancel();
                Err(Error::Cancelled)
            }
        }
        let temp = Temp::new("cancel");
        let content = vec![3u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let font = font_of(&archive, &content);
        let data = temp.0.join("data");
        let progress = Progress::default();
        assert_eq!(
            install(&font, &data, &Cancelling, &progress),
            Err(Error::Cancelled)
        );
        assert!(data.join("soundfonts/fake.part").exists());
        assert_eq!(progress.stage(), Stage::Idle);
    }

    #[test]
    fn two_installs_of_one_font_never_share_a_part() {
        let temp = Temp::new("lock");
        let content = vec![3u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let font = font_of(&archive, &content);
        let data = temp.0.join("data");
        fs::create_dir_all(data.join("soundfonts")).unwrap();
        // Another process, as far as the lock can tell: its own open file.
        let other = private_file(&data.join("soundfonts/fake.lock")).unwrap();
        other.lock().unwrap();

        let fake = Fake::new(archive);
        let error = install(&font, &data, &fake, &Progress::default()).unwrap_err();
        assert!(matches!(error, Error::Failed(m) if m.contains("another Gamme")));
        assert!(fake.ranges.lock().unwrap().is_empty());
        // Unlocked, not only closed: a test spawning `tar` or `curl` on another thread holds a
        // copy of every open file between its fork and its exec, and the lock (flock) belongs
        // to the file as all its copies share it. Closing ours would leave it held a moment.
        other.unlock().unwrap();
        drop(other);
        install(&font, &data, &fake, &Progress::default()).unwrap();
    }

    #[test]
    fn what_is_kept_is_private() {
        let temp = Temp::new("modes");
        let content = vec![3u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let font = font_of(&archive, &content);
        let data = temp.0.join("data");
        let path = install(&font, &data, &Fake::new(archive), &Progress::default()).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&data.join("soundfonts")), 0o700);
    }

    #[test]
    fn what_an_earlier_install_left_is_removed_once_the_font_is_there() {
        let temp = Temp::new("stale");
        let content = vec![3u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let font = font_of(&archive, &content);
        let data = temp.0.join("data");
        install(&font, &data, &Fake::new(archive), &Progress::default()).unwrap();
        let folder = data.join("soundfonts");
        fs::write(folder.join("fake.part"), b"old").unwrap();
        fs::create_dir(folder.join("fake.unpack")).unwrap();
        install(&font, &data, &Fake::new(Vec::new()), &Progress::default()).unwrap();
        assert!(!folder.join("fake.part").exists());
        assert!(!folder.join("fake.unpack").exists());
    }

    #[test]
    fn a_member_that_is_a_link_is_refused() {
        let temp = Temp::new("link");
        let tree = temp.0.join("tree/fake");
        fs::create_dir_all(&tree).unwrap();
        fs::write(temp.0.join("secret"), vec![3u8; 2000]).unwrap();
        std::os::unix::fs::symlink(temp.0.join("secret"), tree.join("fake.sf2")).unwrap();
        let archive = temp.0.join("link.tar");
        let status = Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(temp.0.join("tree"))
            .arg("fake/fake.sf2")
            .status()
            .unwrap();
        assert!(status.success());
        let archive = fs::read(archive).unwrap();
        // Same size as the real font would have: only its kind is wrong.
        let font = font_of(&archive, &vec![3u8; 2000]);
        let data = temp.0.join("data");
        let error = install(&font, &data, &Fake::new(archive), &Progress::default()).unwrap_err();
        assert!(matches!(error, Error::Failed(m) if m.contains("not what was expected")));
        assert!(!font.installed(&data));
    }

    #[test]
    fn a_failing_tar_says_why() {
        let temp = Temp::new("tar-fails");
        let content = vec![3u8; 2000];
        let archive = tar_of(&temp.0, "fake/fake.sf2", &content);
        let mut font = font_of(&archive, &content);
        // A member the archive does not hold.
        font.member = "fake/other.sf2";
        let data = temp.0.join("data");
        let error = install(&font, &data, &Fake::new(archive), &Progress::default()).unwrap_err();
        assert!(
            matches!(&error, Error::Failed(m) if m.contains("tar says") && m.contains("other.sf2")),
            "{error}"
        );
    }

    /// A 7z of the same shape as the small piano's (made with `bsdtar --format 7zip`): it holds
    /// `fake/fake.sf2`, "RIFF 7z stand-in".
    const SEVEN_ZIP: &[u8] = include_bytes!("../tests/fixtures/fake.7z");

    #[test]
    fn a_7z_is_unpacked_without_any_tool_of_the_machine() {
        let temp = Temp::new("7z");
        let content = b"RIFF 7z stand-in";
        let mut font = font_of(SEVEN_ZIP, content);
        font.archive = Archive::SevenZip;
        let data = temp.0.join("data");
        let path = install(
            &font,
            &data,
            &Fake::new(SEVEN_ZIP.to_vec()),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), content);
    }

    #[test]
    fn a_7z_member_bigger_than_the_font_is_refused() {
        let temp = Temp::new("7z-big");
        let mut font = font_of(SEVEN_ZIP, b"RIFF");
        font.archive = Archive::SevenZip;
        let data = temp.0.join("data");
        let error = install(
            &font,
            &data,
            &Fake::new(SEVEN_ZIP.to_vec()),
            &Progress::default(),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Failed(m) if m.contains("bigger than expected")));
        assert!(!font.installed(&data));
    }

    /// The real `Curl` against a server on the loopback that takes ranges.
    #[test]
    fn curl_resumes_a_part_with_a_range_request() {
        if Command::new("curl").arg("--version").output().is_err() {
            eprintln!("curl is not installed: test skipped");
            return;
        }
        let body: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = body.clone();
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_by_server = seen.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut range = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                    range = value.trim().trim_end_matches('-').parse::<usize>().ok();
                    seen_by_server.lock().unwrap().push(line.trim().to_owned());
                }
            }
            let (head, rest) = match range {
                Some(from) => (
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{}/{}\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        served.len() - 1,
                        served.len(),
                        served.len() - from
                    ),
                    &served[from..],
                ),
                None => (
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        served.len()
                    ),
                    &served[..],
                ),
            };
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(rest).unwrap();
        });

        let temp = Temp::new("curl");
        let partial = temp.0.join("body.part");
        fs::write(&partial, &body[..50_000]).unwrap();
        let progress = Progress::default();
        // Neither the user's `~/.curlrc` nor their proxy variables reach this one.
        let curl = Curl {
            protocols: "=http",
            isolated: Some(temp.0.clone()),
        };
        curl.fetch(
            &format!("http://127.0.0.1:{port}/body"),
            &partial,
            &progress,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&partial).unwrap(), body);
        assert_eq!(*seen.lock().unwrap(), ["Range: bytes=50000-"]);
        assert_eq!(progress.bytes().0, body.len() as u64);
    }
}
