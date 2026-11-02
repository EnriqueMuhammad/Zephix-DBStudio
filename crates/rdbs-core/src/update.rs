//! Update check: release versions and the GitHub releases list.
//!
//! Only parsing and comparison live here. The worker fetches [`RELEASES_API_URL`] off the UI
//! thread ([`crate::Command::CheckForUpdates`]) and the UI compares the result with the
//! running version.

use std::fmt;

/// GitHub repository that publishes Zephix releases.
pub const RELEASES_REPO: &str = "EnriqueMuhammad/Zephix-DBStudio";

/// Releases list of [`RELEASES_REPO`] (newest first). The latest-release endpoint is not used
/// because it neither checks the tag format nor picks the highest version.
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/EnriqueMuhammad/Zephix-DBStudio/releases?per_page=30";

/// Human-readable releases page, used when no specific release is known.
pub const RELEASES_PAGE_URL: &str = "https://github.com/EnriqueMuhammad/Zephix-DBStudio/releases";

/// A `major.minor.patch` version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major version.
    pub major: u64,
    /// Minor version.
    pub minor: u64,
    /// Patch version.
    pub patch: u64,
}

impl Version {
    /// Parses `X.Y.Z` (exactly three numeric parts, no suffix such as `-rc1`).
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// Parses a release tag of the form `vX.Y.Z`.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.strip_prefix('v')?)
    }

    /// The version of this build (`[workspace.package] version`).
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Version from the tag.
    pub version: Version,
    /// The tag, e.g. `v0.3.2`.
    pub tag: String,
    /// Release page on GitHub.
    pub url: String,
    /// Release title (falls back to the tag).
    pub name: String,
}

/// Picks the highest stable release from a GitHub releases list (JSON array).
///
/// Drafts, pre-releases and tags that are not `vX.Y.Z` are ignored. `Ok(None)` means the
/// list has no such release.
pub fn latest_release(json: &str) -> Result<Option<Release>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Unexpected response from GitHub: {e}"))?;
    let Some(list) = value.as_array() else {
        let message = value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("not a list of releases");
        return Err(format!("Unexpected response from GitHub: {message}"));
    };
    let flag = |r: &serde_json::Value, key: &str| r.get(key).and_then(|v| v.as_bool());
    let latest = list
        .iter()
        .filter(|r| flag(r, "draft") == Some(false) && flag(r, "prerelease") == Some(false))
        .filter_map(|r| {
            let tag = r.get("tag_name")?.as_str()?;
            let version = Version::from_tag(tag)?;
            let url = r
                .get("html_url")
                .and_then(|u| u.as_str())
                .filter(|u| u.starts_with("https://github.com/"))
                .map_or_else(
                    || format!("https://github.com/{RELEASES_REPO}/releases/tag/{tag}"),
                    str::to_owned,
                );
            let name = r
                .get("name")
                .and_then(|n| n.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(tag)
                .to_owned();
            Some(Release {
                version,
                tag: tag.to_owned(),
                url,
                name,
            })
        })
        .max_by_key(|r| r.version);
    Ok(latest)
}

/// The release to offer: `Some` when `latest` is newer than `current`.
pub fn newer_release(current: Version, latest: Option<Release>) -> Option<Release> {
    latest.filter(|r| r.version > current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn parses_versions_and_tags() {
        assert_eq!(Version::parse("0.3.2"), Some(v(0, 3, 2)));
        assert_eq!(Version::parse("10.20.30"), Some(v(10, 20, 30)));
        assert_eq!(Version::from_tag("v1.2.3"), Some(v(1, 2, 3)));
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "1.2.3-rc1", "+1.2.3", "1..3"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::from_tag("1.2.3"), None);
        assert_eq!(Version::from_tag("release-1.2.3"), None);
        assert_eq!(v(0, 3, 2).to_string(), "0.3.2");
    }

    #[test]
    fn compares_numerically() {
        assert!(v(0, 10, 0) > v(0, 9, 9));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert!(v(0, 3, 10) > v(0, 3, 2));
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn picks_highest_stable_release() {
        let json = r#"[
            {"tag_name": "v0.5.0", "draft": true, "prerelease": false, "html_url": "https://github.com/x/y/releases/tag/v0.5.0", "name": "draft"},
            {"tag_name": "v0.4.0-rc1", "draft": false, "prerelease": true},
            {"tag_name": "v0.4.1", "draft": false, "prerelease": true},
            {"tag_name": "nightly", "draft": false, "prerelease": false},
            {"tag_name": "v0.3.10", "draft": false, "prerelease": false, "html_url": "https://github.com/x/y/releases/tag/v0.3.10", "name": "Zephix 0.3.10"},
            {"tag_name": "v0.3.9", "draft": false, "prerelease": false, "html_url": "https://github.com/x/y/releases/tag/v0.3.9", "name": ""}
        ]"#;
        let latest = latest_release(json).ok().flatten();
        let latest = latest.as_ref();
        assert_eq!(latest.map(|r| r.version), Some(v(0, 3, 10)));
        assert_eq!(latest.map(|r| r.name.as_str()), Some("Zephix 0.3.10"));
        assert_eq!(
            latest.map(|r| r.url.as_str()),
            Some("https://github.com/x/y/releases/tag/v0.3.10")
        );
    }

    #[test]
    fn falls_back_for_missing_fields() {
        let json = r#"[{"tag_name": "v1.0.0", "draft": false, "prerelease": false,
                       "html_url": "javascript:alert(1)"}]"#;
        let latest = latest_release(json).ok().flatten();
        assert_eq!(latest.as_ref().map(|r| r.name.as_str()), Some("v1.0.0"));
        assert_eq!(
            latest.map(|r| r.url),
            Some(format!(
                "https://github.com/{RELEASES_REPO}/releases/tag/v1.0.0"
            ))
        );
        // Missing draft / prerelease flags are not treated as stable.
        assert_eq!(latest_release(r#"[{"tag_name": "v1.0.0"}]"#), Ok(None));
        assert_eq!(latest_release("[]"), Ok(None));
    }

    #[test]
    fn reports_bad_responses() {
        let err = latest_release(r#"{"message": "API rate limit exceeded"}"#);
        assert!(err.is_err_and(|e| e.contains("API rate limit exceeded")));
        assert!(latest_release("<html>").is_err());
    }

    #[test]
    fn offers_only_newer_releases() {
        let release = |version| Release {
            version,
            tag: format!("v{version}"),
            url: String::new(),
            name: String::new(),
        };
        assert!(newer_release(v(0, 3, 2), Some(release(v(0, 3, 3)))).is_some());
        assert!(newer_release(v(0, 3, 2), Some(release(v(0, 3, 2)))).is_none());
        assert!(newer_release(v(0, 3, 2), Some(release(v(0, 3, 1)))).is_none());
        assert!(newer_release(v(0, 3, 2), None).is_none());
    }
}
