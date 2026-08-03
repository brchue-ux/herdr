//! Open pull request counts for the forge repository a Space pushes to.
//!
//! Herdr already knows each Space's checkout, repo root and remotes. This module
//! turns that into the one outstanding-work fact git alone cannot answer: how
//! many pull requests are open and waiting on you.
//!
//! Uses `curl` as a subprocess for HTTP, the same way [`crate::update`] does, so
//! this adds no Rust HTTP dependency and no long-lived process. Every fetch is a
//! one-shot subprocess owned by the refresh that asked for it.

use std::time::Duration;

/// How long a fetch may take before it is abandoned.
///
/// A forge that has gone slow must not be able to pin a refresh thread; the
/// counters simply keep their previous value until a later refresh succeeds.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Open pull requests on one forge repository, kept atomic so the renderer can
/// abbreviate at narrow widths rather than being handed a finished string.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullRequestCounts {
    /// Open pull requests that are not drafts.
    pub open: usize,
    /// Open pull requests still marked draft, excluded from `open`.
    pub draft: usize,
    /// Open pull requests that name the authenticated user as a reviewer.
    pub review_requested: usize,
}

/// A repository on a forge, as named by a git remote URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoSlug {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepoSlug {
    /// Parses the owner and repository out of a git remote URL.
    ///
    /// Handles the three forms git writes in practice: `scp`-style SSH
    /// (`git@host:owner/name.git`), a URL with a scheme (`https://`, `ssh://`,
    /// `git://`), and a bare `host/owner/name`. Anything else yields `None`
    /// rather than a guess, because a wrong slug would silently count another
    /// repository's pull requests.
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        if url.is_empty() {
            return None;
        }

        let remainder = match url.split_once("://") {
            // `ssh://git@host/owner/name` - drop scheme, then any userinfo.
            Some((_, after_scheme)) => after_scheme
                .split_once('@')
                .map_or(after_scheme, |(_, after_user)| after_user),
            None => match url.split_once('@') {
                // `git@host:owner/name.git` - the colon separates host from path.
                Some((_, after_user)) => &after_user.replacen(':', "/", 1),
                None => url,
            },
        };

        let mut segments = remainder
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let name = segments.pop()?.trim_end_matches(".git");
        let owner = segments.pop()?;
        let host = segments.first().copied()?;
        // A host with no dot is a scp-style alias resolved by ssh_config, not a
        // forge hostname Herdr can turn into an API endpoint.
        if name.is_empty() || owner.is_empty() || !host.contains('.') {
            return None;
        }

        Some(Self {
            host: strip_port(host).to_ascii_lowercase(),
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    /// The REST base for this slug, honouring GitHub Enterprise's `/api/v3` path.
    fn api_base(&self) -> String {
        if self.host == "github.com" {
            "https://api.github.com".to_string()
        } else {
            format!("https://{}/api/v3", self.host)
        }
    }

    pub fn is_github(&self) -> bool {
        self.host == "github.com" || self.host.ends_with(".github.com")
    }
}

fn strip_port(host: &str) -> &str {
    host.split_once(':').map_or(host, |(host, _)| host)
}

/// Credentials Herdr found for one forge host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeAuth {
    pub token: String,
    /// The authenticated login, when the source named it. Used to decide which
    /// pull requests are waiting on this user specifically.
    pub login: Option<String>,
}

/// Finds a token for `host` without prompting and without storing anything.
///
/// Ordered most explicit first: an environment variable set for this process
/// beats a file on disk, and `gh`'s own config is consulted before shelling out
/// to `gh` itself so the common case costs no subprocess at all.
pub fn resolve_auth(host: &str) -> Option<ForgeAuth> {
    for name in ["HERDR_FORGE_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(token) = std::env::var(name).ok().filter(|t| !t.trim().is_empty()) {
            return Some(ForgeAuth {
                token: token.trim().to_string(),
                login: None,
            });
        }
    }

    if let Some(auth) = auth_from_gh_hosts(host) {
        return Some(auth);
    }

    gh_cli_token(host).map(|token| ForgeAuth { token, login: None })
}

/// Where `gh` keeps its per-host credentials, following `gh`'s own precedence.
fn gh_hosts_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("GH_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .map(|dir| dir.join("gh"))
        })
        .or_else(|| home_dir().map(|home| home.join(".config").join("gh")))?;
    Some(base.join("hosts.yml"))
}

#[cfg(windows)]
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

#[cfg(not(windows))]
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Reads `gh`'s `hosts.yml` for a host's token and login.
///
/// Deliberately a narrow line scanner rather than a YAML parser: the file's
/// shape is fixed and shallow, and adding a YAML dependency to read two scalars
/// would be a poor trade. Anything unrecognised yields `None`, never a guess.
fn auth_from_gh_hosts(host: &str) -> Option<ForgeAuth> {
    let contents = std::fs::read_to_string(gh_hosts_path()?).ok()?;
    let mut in_host = false;
    let mut token = None;
    let mut login = None;

    for line in contents.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Top-level keys are the host names.
        if indent == 0 {
            if in_host {
                break;
            }
            in_host = trimmed.strip_suffix(':').is_some_and(|name| name == host);
            continue;
        }
        if !in_host {
            continue;
        }

        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        // The nested `users:` block repeats each login's token in a flow map;
        // only the host-level scalars are read, so the first match wins.
        match key.trim() {
            "oauth_token" if token.is_none() && !value.is_empty() => {
                token = Some(value.to_string());
            }
            "user" if login.is_none() && !value.is_empty() => {
                login = Some(value.to_string());
            }
            _ => {}
        }
    }

    token.map(|token| ForgeAuth { token, login })
}

fn gh_cli_token(host: &str) -> Option<String> {
    let output = crate::noninteractive_process::command("gh")
        .args(["auth", "token", "--hostname", host])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Fetches open pull request counts for one repository.
///
/// Returns `None` when the forge could not be reached or answered with anything
/// other than the expected array; callers keep their previous counts rather than
/// rendering a zero, because "we could not ask" is not "there is nothing open".
pub fn fetch_pull_request_counts(slug: &RepoSlug, auth: &ForgeAuth) -> Option<PullRequestCounts> {
    if !slug.is_github() {
        return None;
    }

    // The plain pulls listing, not the search API: it costs one request against
    // the 5000/hour core budget instead of the search endpoint's 30/minute, and
    // it already carries the draft flag and requested reviewers.
    let url = format!(
        "{}/repos/{}/{}/pulls?state=open&per_page=100",
        slug.api_base(),
        slug.owner,
        slug.name
    );

    let output = crate::noninteractive_process::curl_command()
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            &FETCH_TIMEOUT.as_secs().to_string(),
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            "--header",
            &format!("User-Agent: herdr/{}", env!("CARGO_PKG_VERSION")),
            // Passed via a header file on stdin so the token never appears in
            // this process's argv, where any other user on the box could read it.
            "--header",
            "@-",
            &url,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()?
                .write_all(format!("Authorization: Bearer {}\n", auth.token).as_bytes())
                .ok()?;
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        return None;
    }

    let pulls: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    Some(count_pull_requests(&pulls, auth.login.as_deref()))
}

/// Reduces a pulls listing to the atomic counters the sidebar renders.
fn count_pull_requests(pulls: &[serde_json::Value], login: Option<&str>) -> PullRequestCounts {
    let mut counts = PullRequestCounts::default();
    for pull in pulls {
        if pull.get("draft").and_then(serde_json::Value::as_bool) == Some(true) {
            counts.draft += 1;
            continue;
        }
        counts.open += 1;

        let Some(login) = login else { continue };
        let requested = pull
            .get("requested_reviewers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|reviewers| {
                reviewers.iter().any(|reviewer| {
                    reviewer
                        .get("login")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|name| name.eq_ignore_ascii_case(login))
                })
            });
        if requested {
            counts.review_requested += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_remote_url_forms_git_actually_writes() {
        let cases = [
            (
                "git@github.com:owner/name.git",
                "github.com",
                "owner",
                "name",
            ),
            (
                "https://github.com/owner/name.git",
                "github.com",
                "owner",
                "name",
            ),
            (
                "https://github.com/owner/name",
                "github.com",
                "owner",
                "name",
            ),
            (
                "ssh://git@github.com/owner/name.git",
                "github.com",
                "owner",
                "name",
            ),
            (
                "git://github.com/owner/name.git",
                "github.com",
                "owner",
                "name",
            ),
            (
                "https://ghe.example.com/owner/name.git",
                "ghe.example.com",
                "owner",
                "name",
            ),
        ];

        for (url, host, owner, name) in cases {
            let slug = RepoSlug::parse(url).unwrap_or_else(|| panic!("should parse {url}"));
            assert_eq!(slug.host, host, "host for {url}");
            assert_eq!(slug.owner, owner, "owner for {url}");
            assert_eq!(slug.name, name, "name for {url}");
        }
    }

    #[test]
    fn refuses_urls_it_cannot_name_a_repository_from() {
        // An ssh_config alias has no forge hostname behind it, and counting some
        // other repository's pull requests is worse than counting none.
        for url in ["", "git@myalias:owner/name.git", "name.git", "/local/path"] {
            assert_eq!(RepoSlug::parse(url), None, "should refuse {url}");
        }
    }

    #[test]
    fn strips_a_port_from_the_host() {
        let slug =
            RepoSlug::parse("ssh://git@ghe.example.com:2222/owner/name.git").expect("should parse");
        assert_eq!(slug.host, "ghe.example.com");
        assert_eq!(slug.owner, "owner");
    }

    #[test]
    fn enterprise_hosts_use_the_v3_api_path() {
        let slug = RepoSlug::parse("https://ghe.example.com/o/n").expect("should parse");
        assert_eq!(slug.api_base(), "https://ghe.example.com/api/v3");
        assert!(!slug.is_github());

        let slug = RepoSlug::parse("https://github.com/o/n").expect("should parse");
        assert_eq!(slug.api_base(), "https://api.github.com");
        assert!(slug.is_github());
    }

    #[test]
    fn drafts_are_counted_apart_from_open_pull_requests() {
        let pulls = serde_json::json!([
            {"draft": false, "requested_reviewers": []},
            {"draft": true, "requested_reviewers": []},
            {"draft": false, "requested_reviewers": [{"login": "captain"}]},
        ]);
        let counts =
            count_pull_requests(pulls.as_array().expect("array of pulls"), Some("captain"));

        assert_eq!(counts.open, 2);
        assert_eq!(counts.draft, 1);
        assert_eq!(counts.review_requested, 1);
    }

    #[test]
    fn review_requests_need_a_known_login_to_be_attributed() {
        let pulls = serde_json::json!([
            {"draft": false, "requested_reviewers": [{"login": "someone"}]},
        ]);
        let counts = count_pull_requests(pulls.as_array().expect("array of pulls"), None);

        assert_eq!(counts.open, 1);
        // Without a login there is nobody to compare against, so the emphasised
        // figure stays at zero rather than claiming every review is yours.
        assert_eq!(counts.review_requested, 0);
    }

    #[test]
    fn a_draft_that_requested_review_is_not_counted_as_waiting() {
        let pulls = serde_json::json!([
            {"draft": true, "requested_reviewers": [{"login": "captain"}]},
        ]);
        let counts =
            count_pull_requests(pulls.as_array().expect("array of pulls"), Some("captain"));

        assert_eq!(counts.open, 0);
        assert_eq!(counts.draft, 1);
        assert_eq!(counts.review_requested, 0);
    }

    #[test]
    fn reads_token_and_login_out_of_a_gh_hosts_file() {
        let dir = std::env::temp_dir().join(format!("herdr-forge-auth-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gh")).expect("create gh config dir");
        std::fs::write(
            dir.join("gh/hosts.yml"),
            "github.com:\n    users:\n        captain: {oauth_token: nested}\n    user: captain\n    oauth_token: gho_example\n",
        )
        .expect("write hosts.yml");

        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let previous = std::env::var_os("GH_CONFIG_DIR");
        std::env::set_var("GH_CONFIG_DIR", dir.join("gh"));
        let auth = auth_from_gh_hosts("github.com");
        match previous {
            Some(value) => std::env::set_var("GH_CONFIG_DIR", value),
            None => std::env::remove_var("GH_CONFIG_DIR"),
        }

        let auth = auth.expect("hosts.yml should yield auth");
        // The host-level scalar wins over the repeated per-user one above it.
        assert_eq!(auth.token, "gho_example");
        assert_eq!(auth.login.as_deref(), Some("captain"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_host_absent_from_gh_hosts_yields_no_auth() {
        let dir = std::env::temp_dir().join(format!("herdr-forge-miss-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gh")).expect("create gh config dir");
        std::fs::write(
            dir.join("gh/hosts.yml"),
            "ghe.example.com:\n    oauth_token: gho_other\n",
        )
        .expect("write hosts.yml");

        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let previous = std::env::var_os("GH_CONFIG_DIR");
        std::env::set_var("GH_CONFIG_DIR", dir.join("gh"));
        let auth = auth_from_gh_hosts("github.com");
        match previous {
            Some(value) => std::env::set_var("GH_CONFIG_DIR", value),
            None => std::env::remove_var("GH_CONFIG_DIR"),
        }

        assert_eq!(auth, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
