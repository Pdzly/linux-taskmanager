//! Update checker: asks GitHub for the latest release and compares versions.
//! Uses the system `curl` (every desktop distro has it) instead of bundling a TLS
//! stack into the binary. It only reads; installing stays with the user's package
//! manager or a download, since TaskMonitor ships as .deb, .rpm, AppImage and AUR.

use std::process::Command;

const LATEST: &str = "https://api.github.com/repos/anuzdhk/linux-taskmanager/releases/latest";

/// How long automatic checks wait between checks.
pub const INTERVAL_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    UpToDate,
    Available(Release),
}

/// Fetches the latest release and compares it with `current` (e.g. "0.1.2").
pub fn check(current: &str) -> Result<Outcome, String> {
    let out = Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--location", "--max-time", "15"])
        .args(["--header", "Accept: application/vnd.github+json"])
        .args(["--user-agent", &format!("TaskMonitor/{current}")])
        .arg(LATEST)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "curl isn't installed".to_owned()
            } else {
                format!("couldn't run curl: {e}")
            }
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(if err.contains("Could not resolve") || err.contains("Failed to connect") {
            "no internet connection".to_owned()
        } else {
            err.trim().trim_start_matches("curl: ").to_owned()
        });
    }
    let release = parse_latest(&String::from_utf8_lossy(&out.stdout))?;
    Ok(if is_newer(&release.version, current) { Outcome::Available(release) } else { Outcome::UpToDate })
}

/// Reads `tag_name` and `html_url` from GitHub's "latest release" JSON.
pub fn parse_latest(json: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|_| "GitHub sent an unexpected answer".to_owned())?;
    let tag = v["tag_name"].as_str().ok_or("GitHub didn't say which version is latest")?;
    let url = v["html_url"].as_str().unwrap_or("https://github.com/anuzdhk/linux-taskmanager/releases/latest");
    Ok(Release { version: tag.trim_start_matches('v').to_owned(), url: url.to_owned() })
}

/// "0.1.10" > "0.1.9"; anything unparsable counts as not newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Option<Vec<u64>> {
        v.trim_start_matches('v').split(['.', '-', '+']).take(3).map(|p| p.parse().ok()).collect()
    }
    match (parts(candidate), parts(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(!is_newer("nightly", "0.1.1"));
    }

    #[test]
    fn parses_github_release() {
        let json = r#"{"tag_name":"v0.1.3","name":"TaskMonitor 0.1.3","html_url":"https://github.com/anuzdhk/linux-taskmanager/releases/tag/v0.1.3","draft":false}"#;
        assert_eq!(
            parse_latest(json).unwrap(),
            Release { version: "0.1.3".into(), url: "https://github.com/anuzdhk/linux-taskmanager/releases/tag/v0.1.3".into() }
        );
        assert!(parse_latest(r#"{"message":"Not Found"}"#).is_err());
        assert!(parse_latest("<html>").is_err());
    }
}
