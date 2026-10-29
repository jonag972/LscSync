//! Update check: asks GitHub for the latest release at launch, then once a
//! day. Nothing is downloaded or installed: the user gets a link to the
//! release page.

use std::time::Duration;

use lscsync_core::version;
use serde::Deserialize;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = "https://github.com/jonag972/LscSync";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Lets the window open before the first check.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(5);

/// Ignores pre-releases and drafts.
const LATEST_RELEASE: &str = "https://api.github.com/repos/jonag972/LscSync/releases/latest";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct Release {
    /// Without the tag's "v".
    pub version: String,
    /// Release page on GitHub.
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
}

impl UpdateState {
    pub fn available(&self) -> Option<&Release> {
        match self {
            Self::Available(release) => Some(release),
            _ => None,
        }
    }
}

/// Blocking: call it from a background thread. The request carries no data
/// about the user, only the app's version in its User-Agent.
pub fn check() -> Result<UpdateState, ureq::Error> {
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
        html_url: String,
    }

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let latest: Latest = agent
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
    }))
}
