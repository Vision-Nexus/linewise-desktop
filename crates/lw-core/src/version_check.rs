//! Startup version check against the linewise-desktop GitHub repo.
//!
//! Two pieces of remote state are consulted in parallel:
//! - `latest`: `tag_name` from the GitHub Releases API (`/releases/latest`).
//! - `min_supported`: a tiny `version-policy.json` checked into the repo at
//!   the master branch. Lives separately from the release tag so cutting a
//!   release alone never accidentally hard-blocks older clients — that takes
//!   a deliberate edit to the policy file.
//!
//! Best-effort: any network or decode failure is the caller's signal to
//! treat the check as "status unknown" and let the app boot anyway. Only a
//! definite "running < min_supported" answer should gate startup.

use crate::error::VersionCheckError;
use semver::Version;
use serde::Deserialize;

pub const RELEASES_URL: &str =
    "https://api.github.com/repos/Vision-Nexus/linewise-desktop/releases/latest";
pub const POLICY_URL: &str =
    "https://raw.githubusercontent.com/Vision-Nexus/linewise-desktop/master/version-policy.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionStatus {
    UpToDate {
        running: Version,
        latest: Version,
    },
    UpdateAvailable {
        running: Version,
        latest: Version,
        release_url: String,
    },
    Unsupported {
        running: Version,
        min_supported: Version,
        latest: Version,
        release_url: String,
    },
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct VersionPolicy {
    #[serde(rename = "minSupported")]
    min_supported: String,
}

/// Tag names on linewise-desktop releases are conventionally `vX.Y.Z`.
/// Strip a leading `v` before parsing so semver comparison works.
fn parse_version(s: &str) -> Result<Version, VersionCheckError> {
    let trimmed = s.trim().strip_prefix('v').unwrap_or(s.trim());
    Version::parse(trimmed).map_err(|source| VersionCheckError::BadVersion {
        input: s.to_string(),
        source,
    })
}

pub async fn check_version(running: &str) -> Result<VersionStatus, VersionCheckError> {
    check_version_with(running, RELEASES_URL, POLICY_URL).await
}

/// Parameterised variant — kept public so a debug build or test can point
/// the URLs at a stub server. Production code goes through `check_version`.
pub async fn check_version_with(
    running: &str,
    releases_url: &str,
    policy_url: &str,
) -> Result<VersionStatus, VersionCheckError> {
    let running = parse_version(running)?;

    // Fresh, short-lived client rather than reusing `lw_core::api_client::ApiClient`:
    // the version check has no auth and runs before `CoreServices::init()` completes,
    // so it deliberately doesn't depend on app config or auth state. Don't "fix" this
    // by sharing the auth-bearing client — that would couple the check to a session
    // that may not exist yet (e.g. a logged-out user opening the app for the first
    // time after a hard-block release).
    let user_agent = format!("linewise-desktop/{}", env!("CARGO_PKG_VERSION"));
    let client = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(8))
        .build()?;

    let releases_fut = fetch_json::<GhRelease>(&client, releases_url);
    let policy_fut = fetch_json::<VersionPolicy>(&client, policy_url);
    let (release, policy) = tokio::try_join!(releases_fut, policy_fut)?;

    let latest = parse_version(&release.tag_name)?;
    let min_supported = parse_version(&policy.min_supported)?;
    let release_url = release.html_url;

    if running < min_supported {
        Ok(VersionStatus::Unsupported {
            running,
            min_supported,
            latest,
            release_url,
        })
    } else if running < latest {
        Ok(VersionStatus::UpdateAvailable {
            running,
            latest,
            release_url,
        })
    } else {
        Ok(VersionStatus::UpToDate { running, latest })
    }
}

async fn fetch_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, VersionCheckError> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let body = resp.text().await?;
    Ok(serde_json::from_str::<T>(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_strips_v_prefix() {
        assert_eq!(parse_version("v1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse_version("1.2.3").unwrap(), Version::new(1, 2, 3));
    }

    #[test]
    fn parse_version_rejects_garbage() {
        assert!(parse_version("not-a-version").is_err());
    }
}
