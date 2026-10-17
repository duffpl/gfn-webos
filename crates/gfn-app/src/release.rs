//! This build's version, and the newer releases of the app published on GitHub.

use std::time::Duration;

use serde_json::Value;

use crate::{Update, Updates};

pub const VERSION: &str = env!("GFN_VERSION");
/// Empty when built without git.
pub const COMMIT: &str = env!("GFN_COMMIT");
/// `YYYY-MM-DD`, or empty when built without git.
pub const COMMIT_DATE: &str = env!("GFN_COMMIT_DATE");
const REPO: &str = env!("GFN_REPO");

/// Unauthenticated, GitHub allows 60 requests an hour from one address, which may be shared with
/// other devices behind the same router: a check twice a day stays far under it.
const RECHECK: Duration = Duration::from_secs(12 * 60 * 60);
/// The TV may start before its network is up, or wake from standby without one for a while.
const RETRY: Duration = Duration::from_secs(5 * 60);
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Markdown, as written on GitHub.
    pub notes: String,
    pub url: String,
}

pub fn releases_page() -> String {
    format!("https://github.com/{REPO}/releases")
}

/// Checks now and then every [`RECHECK`], reporting the releases newer than this build.
pub fn start(updates: Updates) {
    let worker = move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .user_agent(format!("gfn-webos/{VERSION}"))
            .build()
            .into();
        loop {
            let wait = match fetch(&agent) {
                Ok(list) => {
                    let newer = newer(&list, VERSION);
                    println!("release check: {} newer than {VERSION}", newer.len());
                    if updates.send(Update::Releases(newer)).is_err() {
                        return;
                    }
                    RECHECK
                }
                Err(error) => {
                    eprintln!("release check: {error}");
                    RETRY
                }
            };
            std::thread::sleep(wait);
        }
    };
    if let Err(error) = std::thread::Builder::new().name("gfn-release".into()).spawn(worker) {
        eprintln!("release thread: {error}");
    }
}

fn fetch(agent: &ureq::Agent) -> Result<Value, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
    let mut response = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| e.to_string())?;
    let body = response.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

/// `vX.Y.Z` or `X.Y.Z`; the same three numbers webOS takes as a package version.
fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let version = [parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?];
    parts.next().is_none().then_some(version)
}

/// The published releases in GitHub's list newer than `current`, newest first. Drafts and
/// pre-releases are not offered.
fn newer(list: &Value, current: &str) -> Vec<Release> {
    let Some(current) = parse_version(current) else { return Vec::new() };
    let mut found: Vec<([u64; 3], Release)> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|release| release["draft"] != true && release["prerelease"] != true)
        .filter_map(|release| {
            let tag = release["tag_name"].as_str()?;
            let version = parse_version(tag).filter(|version| *version > current)?;
            let text = |key: &str| release[key].as_str().unwrap_or_default().to_owned();
            let date = text("published_at").get(..10).unwrap_or_default().to_owned();
            Some((version, Release { version: tag.trim_start_matches('v').to_owned(), date, notes: text("body"), url: text("html_url") }))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, release)| release).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Heading(String),
    Bullet(String),
    Text(String),
    Gap,
}

/// GitHub's release notes as plain lines for the TV: headings, bullets and paragraphs, with links
/// reduced to their text and emphasis dropped.
pub fn notes_lines(markdown: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut in_comment = false;
    for raw in markdown.lines() {
        let mut line = raw.trim();
        if in_comment || line.starts_with("<!--") {
            in_comment = !line.contains("-->");
            continue;
        }
        if line.is_empty() {
            if lines.last().is_some_and(|last| *last != Line::Gap) {
                lines.push(Line::Gap);
            }
            continue;
        }
        let heading = line.starts_with('#');
        line = line.trim_start_matches('#').trim_start();
        let bullet = !heading && (line.starts_with("* ") || line.starts_with("- ") || line.starts_with("+ "));
        if bullet {
            line = line[2..].trim_start();
        }
        let text = inline(line);
        lines.push(match (heading, bullet) {
            (true, _) => Line::Heading(text),
            (_, true) => Line::Bullet(text),
            _ => Line::Text(text),
        });
    }
    while lines.last() == Some(&Line::Gap) {
        lines.pop();
    }
    lines
}

/// `[text](url)` to `text`, without `**`, `__` or backticks.
fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let link = rest[open..].find("](").and_then(|mid| rest[open + mid..].find(')').map(|close| (open + mid, open + mid + close)));
        let Some((mid, close)) = link else { break };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..mid]);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn versions_take_three_numbers_with_or_without_a_v() {
        assert_eq!(parse_version("v0.10.2"), Some([0, 10, 2]));
        assert_eq!(parse_version("1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert_eq!(parse_version("v1.2.3-rc1"), None);
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
    }

    #[test]
    fn only_newer_published_releases_are_offered_newest_first() {
        let release = |tag: &str, draft: bool, prerelease: bool| {
            json!({"tag_name": tag, "draft": draft, "prerelease": prerelease, "published_at": "2026-11-02T10:00:00Z",
                   "body": "notes", "html_url": format!("https://github.com/o/r/releases/tag/{tag}")})
        };
        let list = json!([
            release("v0.2.0", false, false),
            release("v0.3.0", true, false),
            release("v0.4.0", false, true),
            release("v0.10.0", false, false),
            release("v0.1.0", false, false),
            release("nightly", false, false),
        ]);
        let newer = newer(&list, "0.1.0");
        assert_eq!(newer.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.10.0", "0.2.0"]);
        assert_eq!(newer[1].date, "2026-11-02");
        assert!(super::newer(&list, "0.10.0").is_empty());
        assert!(super::newer(&json!({"message": "Not Found"}), "0.1.0").is_empty());
    }

    #[test]
    fn notes_become_plain_lines() {
        let notes = "<!-- Release notes generated using configuration in .github/release.yml -->\n\n\
                     ## What's Changed\n\
                     * **Sign-out** from the account menu by @someone in https://github.com/o/r/pull/3\n\
                     - Fixes `AV1` on [newer TVs](https://example.com/av1)\n\n\n\
                     **Full Changelog**: https://github.com/o/r/compare/v0.1.0...v0.2.0\n\n";
        assert_eq!(
            notes_lines(notes),
            [
                Line::Heading("What's Changed".into()),
                Line::Bullet("Sign-out from the account menu by @someone in https://github.com/o/r/pull/3".into()),
                Line::Bullet("Fixes AV1 on newer TVs".into()),
                Line::Gap,
                Line::Text("Full Changelog: https://github.com/o/r/compare/v0.1.0...v0.2.0".into()),
            ]
        );
    }
}
