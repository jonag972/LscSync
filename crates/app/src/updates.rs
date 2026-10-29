//! Updates: asks GitHub for the latest release at launch, then once a day,
//! and installs it in one click.
//!
//! Every release publishes SHA256SUMS, the list of its files' SHA-256, signed
//! with the project's Ed25519 key (.github/workflows/release.yml). Before
//! installing anything, the app checks that signature with the public key it
//! embeds, then the downloaded file's hash: a tampered update is refused.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use lscsync_core::version;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = "https://github.com/jonag972/LscSync";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Lets the window open before the first check.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(5);

/// Ignores pre-releases and drafts.
const LATEST_RELEASE: &str = "https://api.github.com/repos/jonag972/LscSync/releases/latest";
const TIMEOUT: Duration = Duration::from_secs(15);
/// Downloads are slower than API calls: a whole installer.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;
/// Public half of the key that signs every release's SHA256SUMS.
const UPDATE_PUBLIC_KEY: &str = "9y+CWFCqrVMUMsn16kMzw9MV0CkQpBs8XyE3CKRQc00=";
const SUMS: &str = "SHA256SUMS";
const SUMS_SIGNATURE: &str = "SHA256SUMS.sig";

#[derive(Clone, Debug)]
pub struct Release {
    /// Without the tag's "v".
    pub version: String,
    /// Release page on GitHub.
    pub url: String,
    /// Downloadable files, by name.
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
}

#[derive(Clone, Debug, Default)]
pub enum UpdateState {
    #[default]
    NotChecked,
    Checking,
    UpToDate,
    Available(Release),
    Failed,
    /// Downloading and checking the update.
    Installing(Release),
    /// The update could not be installed; the release page still can.
    InstallFailed(Release),
}

impl UpdateState {
    /// The newer release, while it is not installed yet.
    pub fn available(&self) -> Option<&Release> {
        match self {
            Self::Available(release) | Self::Installing(release) | Self::InstallFailed(release) => {
                Some(release)
            }
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("network: {0}")]
    Network(#[from] ureq::Error),
    #[error("file: {0}")]
    Io(#[from] std::io::Error),
    #[error("this release has no {0}")]
    MissingFile(String),
    #[error("the list of files is not signed by LscSync")]
    BadSignature,
    #[error("{0} does not match its published hash")]
    BadHash(String),
    #[error("install: {0}")]
    Install(String),
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// Blocking: call it from a background thread. The request carries no data
/// about the user, only the app's version in its User-Agent.
pub fn check() -> Result<UpdateState, ureq::Error> {
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        assets: Vec<Asset>,
    }

    let latest: Latest = agent(TIMEOUT)
        .get(LATEST_RELEASE)
        .header("User-Agent", concat!("LscSync/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_json()?;
    if !version::is_newer(&latest.tag_name, CURRENT_VERSION) {
        return Ok(UpdateState::UpToDate);
    }
    Ok(UpdateState::Available(Release {
        version: latest.tag_name.trim_start_matches('v').to_owned(),
        url: latest.html_url,
        assets: latest.assets,
    }))
}

/// How this copy of the app was installed, which says how to replace it.
#[derive(Clone, Debug, PartialEq)]
pub enum Installation {
    /// Windows installer, in the user's programs folder.
    WindowsSetup,
    /// macOS app bundle, wherever the user put it.
    MacBundle(PathBuf),
    /// Linux AppImage file.
    AppImage(PathBuf),
    /// Linux .deb package.
    Deb,
}

impl Installation {
    /// None for copies that cannot replace themselves: the portable .zip,
    /// development builds.
    pub fn current() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        if cfg!(target_os = "windows") {
            let programs = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
            let folder = programs.join("Programs").join("LscSync");
            return (exe.parent() == Some(folder.as_path())).then_some(Self::WindowsSetup);
        }
        if cfg!(target_os = "macos") {
            // …/LscSync.app/Contents/MacOS/lscsync
            let bundle = exe.ancestors().nth(3)?;
            return (bundle.extension()? == "app").then(|| Self::MacBundle(bundle.to_owned()));
        }
        if let Some(path) = std::env::var_os("APPIMAGE") {
            return Some(Self::AppImage(path.into()));
        }
        (exe == Path::new("/usr/bin/lscsync")).then_some(Self::Deb)
    }

    /// The release file that replaces this installation.
    fn asset_suffix(&self) -> &'static str {
        match self {
            Self::WindowsSetup => "-windows-setup.exe",
            Self::MacBundle(_) => "-macos.dmg",
            Self::AppImage(_) => "-x86_64.AppImage",
            Self::Deb => "-amd64.deb",
        }
    }
}

/// Downloads the update for `installation` and checks it. Blocking.
pub fn download(release: &Release, installation: &Installation) -> Result<PathBuf, UpdateError> {
    let find = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| UpdateError::MissingFile(name.to_owned()))
    };
    let file = release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(installation.asset_suffix()))
        .ok_or_else(|| UpdateError::MissingFile(installation.asset_suffix().to_owned()))?;
    let sums = fetch(&find(SUMS)?.url)?;
    let signature = fetch(&find(SUMS_SIGNATURE)?.url)?;
    verify_signature(&sums, &signature)?;
    let expected = expected_hash(&sums, &file.name)
        .ok_or_else(|| UpdateError::MissingFile(format!("{} in {SUMS}", file.name)))?;

    let folder = lscsync_platform::data_dir()
        .join("updates")
        .join(&release.version);
    std::fs::create_dir_all(&folder)?;
    let path = folder.join(&file.name);
    let hash = save(&file.url, &path)?;
    if hash != expected {
        let _ = std::fs::remove_file(&path);
        return Err(UpdateError::BadHash(file.name.clone()));
    }
    Ok(path)
}

/// Starts the installation of a checked update. Once it returns, the app
/// must quit: the installer (or the new copy) takes over.
pub fn install(installation: &Installation, update: &Path) -> Result<(), UpdateError> {
    match installation {
        Installation::WindowsSetup => {
            // Silent install; the installer closes this app and starts the
            // new one (installer/windows.iss).
            std::process::Command::new(update)
                .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
                .spawn()?;
        }
        Installation::MacBundle(bundle) => install_mac(bundle, update)?,
        Installation::AppImage(path) => {
            let fresh = path.with_extension("new");
            std::fs::copy(update, &fresh)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))?;
            }
            // The running AppImage keeps its open file: replacing it is safe.
            std::fs::rename(&fresh, path)?;
            std::process::Command::new(path).spawn()?;
        }
        Installation::Deb => {
            // pkexec asks for the password in a system window.
            let status = std::process::Command::new("pkexec")
                .args(["apt-get", "install", "-y"])
                .arg(update)
                .status()?;
            if !status.success() {
                return Err(UpdateError::Install(format!("apt-get: {status}")));
            }
            std::process::Command::new("/usr/bin/lscsync").spawn()?;
        }
    }
    Ok(())
}

/// Replaces the app bundle with the one in the disk image, then starts it
/// once this app has quit.
fn install_mac(bundle: &Path, dmg: &Path) -> Result<(), UpdateError> {
    let run = |program: &str, args: &[&std::ffi::OsStr]| -> Result<(), UpdateError> {
        let status = std::process::Command::new(program).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(UpdateError::Install(format!("{program}: {status}")))
        }
    };
    let mount = dmg.with_extension("mount");
    let _ = std::fs::remove_dir_all(&mount);
    run(
        "hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )?;
    let fresh = bundle.with_extension("app.new");
    let copied = run(
        "ditto",
        &[mount.join("LscSync.app").as_os_str(), fresh.as_os_str()],
    );
    let _ = run("hdiutil", &["detach".as_ref(), mount.as_os_str()]);
    copied?;
    // The running app keeps its open files: the bundle can move under it.
    let old = bundle.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old)?;
    if let Err(error) = std::fs::rename(&fresh, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(error.into());
    }
    // Starts the new app a moment after this one quits, then tidies up.
    let script = format!(
        "sleep 2; /usr/bin/open {bundle}; rm -rf {old}",
        bundle = shell_quote(bundle),
        old = shell_quote(&old),
    );
    std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .spawn()?;
    Ok(())
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// A small file, in memory.
fn fetch(url: &str) -> Result<Vec<u8>, UpdateError> {
    let mut bytes = Vec::new();
    agent(TIMEOUT)
        .get(url)
        .header("User-Agent", concat!("LscSync/", env!("CARGO_PKG_VERSION")))
        .call()?
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .reader()
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Saves a download to `path`, returning its SHA-256 in hexadecimal.
fn save(url: &str, path: &Path) -> Result<String, UpdateError> {
    let mut response = agent(DOWNLOAD_TIMEOUT)
        .get(url)
        .header("User-Agent", concat!("LscSync/", env!("CARGO_PKG_VERSION")))
        .call()?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .reader();
    let mut file = std::fs::File::create(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
    }
    file.sync_all()?;
    Ok(hex(&hasher.finalize()))
}

/// Checks that `sums` was signed with the project's key. `signature` is the
/// base64 of the raw Ed25519 signature (as `openssl pkeyutl -sign` writes it).
fn verify_signature(sums: &[u8], signature: &[u8]) -> Result<(), UpdateError> {
    let key: [u8; 32] = STANDARD
        .decode(UPDATE_PUBLIC_KEY)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(UpdateError::BadSignature)?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| UpdateError::BadSignature)?;
    let text: String = String::from_utf8_lossy(signature)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let signature = STANDARD
        .decode(text)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .ok_or(UpdateError::BadSignature)?;
    key.verify_strict(sums, &signature)
        .map_err(|_| UpdateError::BadSignature)
}

/// The hash `sha256sum` listed for `name`.
fn expected_hash(sums: &[u8], name: &str) -> Option<String> {
    String::from_utf8_lossy(sums).lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signed with the real release key by `openssl pkeyutl -sign -rawin`.
    const SIGNED: &[u8] = b"LscSync update signature test\n";
    const SIGNATURE: &[u8] =
        b"LPxoVEuMDfwJYNhS/VPWXLcdVeqY0XtopYE7IWsevxO99TN0KVDCo8zaPHKi0qg0CG5F4q4NqIvWsefiGcx1Bw==\n";

    #[test]
    fn accepts_the_release_key_signature() {
        assert!(verify_signature(SIGNED, SIGNATURE).is_ok());
    }

    #[test]
    fn refuses_a_changed_list() {
        assert!(matches!(
            verify_signature(b"LscSync update signature test!\n", SIGNATURE),
            Err(UpdateError::BadSignature)
        ));
        assert!(verify_signature(SIGNED, b"not a signature").is_err());
    }

    #[test]
    fn reads_the_hash_of_a_file() {
        let sums = b"aa11  LscSync-0.3.0-macos.dmg\nBB22  LscSync-0.3.0-windows-setup.exe\n";
        assert_eq!(
            expected_hash(sums, "LscSync-0.3.0-windows-setup.exe").as_deref(),
            Some("bb22")
        );
        assert_eq!(expected_hash(sums, "other"), None);
        assert_eq!(hex(&[0x0a, 0xff]), "0aff");
    }
}
