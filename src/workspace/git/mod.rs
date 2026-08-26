mod config;
#[cfg(test)]
mod config_tests;
mod discovery;
mod status;
#[cfg(test)]
pub(super) mod test_support;

pub(crate) use self::discovery::automatic_workspace_label;
pub(crate) use self::status::{parse_unified_diff_lines, GIT_DIFF_MAX_LINES};

pub use self::{
    discovery::{
        derive_label_from_cwd, fallback_label_from_cwd, git_branch, git_space_identity,
        git_space_metadata, GitSpaceMetadata,
    },
    status::{
        git_remote_url_for_checkout, git_status_cache_key, git_status_cache_key_for_space,
        git_status_snapshot_for_cwd_with_demand, GitDiffLine, GitDiffLineKind, GitDiffText,
        GitDirtyCounts, GitStatusCacheEntry, GitStatusRefreshDemand,
    },
};

#[cfg(test)]
pub(super) use self::status::git_ahead_behind;
