use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataToken {
    value: String,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MetadataTokens {
    entries: HashMap<String, MetadataToken>,
}

pub(crate) const MAX_SEQUENCE_SOURCES: usize = 32;

/// Wall-clock milliseconds since the Unix epoch, or `None` for a clock that
/// reads before the epoch.
fn unix_millis(at: SystemTime) -> Option<u64> {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

pub(crate) fn sequence_is_fresh(
    sequences: &HashMap<String, u64>,
    source: &str,
    seq: Option<u64>,
) -> bool {
    seq.is_none_or(|seq| sequences.get(source).is_none_or(|last| seq > *last))
}

pub(crate) fn accept_sequence(
    sequences: &mut HashMap<String, u64>,
    source: &str,
    seq: Option<u64>,
) -> Result<bool, ()> {
    let Some(seq) = seq else {
        return Ok(true);
    };
    if !sequence_is_fresh(sequences, source, Some(seq)) {
        return Ok(false);
    }
    if !sequences.contains_key(source) && sequences.len() >= MAX_SEQUENCE_SOURCES {
        return Err(());
    }
    sequences.insert(source.to_string(), seq);
    Ok(true)
}

impl MetadataTokens {
    pub(crate) fn patch(
        &mut self,
        patch: HashMap<String, Option<String>>,
        ttl: Option<Duration>,
        now: Instant,
    ) -> bool {
        let expires_at = ttl.and_then(|ttl| now.checked_add(ttl));
        let mut changed = false;
        for (key, value) in patch {
            match value {
                Some(value) => {
                    let token = MetadataToken { value, expires_at };
                    if self.entries.get(&key) != Some(&token) {
                        self.entries.insert(key, token);
                        changed = true;
                    }
                }
                None => {
                    changed |= self.entries.remove(&key).is_some();
                }
            }
        }
        changed
    }

    /// Drop every token. Returns whether anything was there to drop.
    ///
    /// This is the revoke path for a token published without a TTL: it now
    /// outlives both its publisher and the server, so there has to be a way to
    /// clear it that does not require knowing its key.
    pub(crate) fn clear(&mut self) -> bool {
        let had_any = !self.entries.is_empty();
        self.entries.clear();
        had_any
    }

    pub(crate) fn key_count_after_patch(&self, patch: &HashMap<String, Option<String>>) -> usize {
        let mut keys = self
            .values()
            .into_keys()
            .collect::<std::collections::HashSet<_>>();
        for (key, value) in patch {
            if value.is_some() {
                keys.insert(key.clone());
            } else {
                keys.remove(key);
            }
        }
        keys.len()
    }

    /// One token's value, without materialising the whole map.
    ///
    /// [`Self::values`] clones every key and value, which is the right shape
    /// for a consumer that needs the map but pure waste for one asking about a
    /// single token on every pane on every frame.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|token| token.value.as_str())
    }

    pub(crate) fn values(&self) -> HashMap<String, String> {
        self.entries
            .iter()
            .map(|(key, token)| (key.clone(), token.value.clone()))
            .collect()
    }

    pub(crate) fn next_expiry(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|token| token.expires_at)
            .min()
    }

    pub(crate) fn expire_at(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, token| token.expires_at.is_none_or(|deadline| deadline > now));
        self.entries.len() != before
    }

    /// Tokens in the form the session file stores them.
    ///
    /// A handoff carries the TTL that is *left*, because it hands one live
    /// process straight to the next. A session file cannot: the gap between
    /// save and load is unbounded, so a remaining-duration would resurrect a
    /// token hours after it stopped meaning anything. Deadlines therefore
    /// leave as an absolute wall-clock instant and are re-anchored on load,
    /// which is what keeps the TTL honest across a cold restart.
    ///
    /// Already-expired tokens are dropped rather than written, for the same
    /// reason `to_handoff` drops them: a token that was invisible before the
    /// restart must not flicker back into a row.
    pub(crate) fn to_persisted(
        &self,
        now: Instant,
        wall_now: SystemTime,
    ) -> Vec<crate::persist::PersistedMetadataToken> {
        let mut tokens = self
            .entries
            .iter()
            .filter_map(|(name, token)| {
                let expires_at_ms = match token.expires_at {
                    Some(deadline) if deadline <= now => return None,
                    Some(deadline) => Some(unix_millis(
                        wall_now.checked_add(deadline.saturating_duration_since(now))?,
                    )?),
                    None => None,
                };
                Some(crate::persist::PersistedMetadataToken {
                    name: name.clone(),
                    value: token.value.clone(),
                    expires_at_ms,
                })
            })
            .collect::<Vec<_>>();
        // `session.json` is a file people read and diff; HashMap order is not
        // stable, and a session file that reshuffles on every save is
        // needlessly hard to inspect.
        tokens.sort_by(|left, right| left.name.cmp(&right.name));
        tokens
    }

    /// Rebuild persisted tokens against this process's clock.
    ///
    /// A token whose wall-clock deadline passed while the server was down is
    /// dropped, so a TTL a publisher asked for is still respected even though
    /// nothing was running to sweep it.
    pub(crate) fn restore_persisted(
        &mut self,
        tokens: Vec<crate::persist::PersistedMetadataToken>,
        now: Instant,
        wall_now: SystemTime,
    ) {
        let wall_now_ms = unix_millis(wall_now);
        for token in tokens {
            let expires_at = match token.expires_at_ms {
                None => None,
                Some(deadline_ms) => {
                    // No readable wall clock means no way to tell whether the
                    // deadline has passed. Dropping the token is the choice
                    // that cannot show a stale value.
                    let Some(wall_now_ms) = wall_now_ms else {
                        continue;
                    };
                    let Some(left) = deadline_ms
                        .checked_sub(wall_now_ms)
                        .filter(|left| *left > 0)
                    else {
                        continue;
                    };
                    let Some(deadline) = now.checked_add(Duration::from_millis(left)) else {
                        continue;
                    };
                    Some(deadline)
                }
            };
            self.entries.insert(
                token.name,
                MetadataToken {
                    value: token.value,
                    expires_at,
                },
            );
        }
    }

    /// Tokens in the form a live handoff can carry to the next process.
    ///
    /// Already-expired tokens are dropped rather than shipped: the importing
    /// server would only sweep them on its first expiry pass, and a token that
    /// was invisible before the handoff must not flicker back into a row.
    #[cfg(unix)]
    pub(crate) fn to_handoff(&self, now: Instant) -> Vec<crate::handoff_metadata::HandoffToken> {
        let mut tokens = self
            .entries
            .iter()
            .filter_map(|(name, token)| {
                let expires_in = match token.expires_at {
                    Some(deadline) if deadline <= now => return None,
                    Some(deadline) => Some(deadline.duration_since(now)),
                    None => None,
                };
                Some(crate::handoff_metadata::HandoffToken {
                    name: name.clone(),
                    value: token.value.clone(),
                    expires_in,
                })
            })
            .collect::<Vec<_>>();
        // HashMap order is not stable, and a manifest that reorders between
        // runs is needlessly hard to diff when a handoff goes wrong.
        tokens.sort_by(|left, right| left.name.cmp(&right.name));
        tokens
    }

    /// Rebuild handoff-carried tokens against this process's clock.
    #[cfg(unix)]
    pub(crate) fn restore_handoff(
        &mut self,
        tokens: Vec<crate::handoff_metadata::HandoffToken>,
        now: Instant,
    ) {
        for token in tokens {
            self.entries.insert(
                token.name,
                MetadataToken {
                    value: token.value,
                    expires_at: token.expires_in.and_then(|left| now.checked_add(left)),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(items: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        items
            .iter()
            .map(|(key, value)| ((*key).into(), value.map(str::to_string)))
            .collect()
    }

    #[test]
    fn sequence_sources_are_bounded() {
        let mut sequences = HashMap::new();
        for index in 0..MAX_SEQUENCE_SOURCES {
            assert_eq!(
                accept_sequence(&mut sequences, &format!("source-{index}"), Some(1)),
                Ok(true)
            );
        }
        assert_eq!(
            accept_sequence(&mut sequences, "one-too-many", Some(1)),
            Err(())
        );
        assert_eq!(
            accept_sequence(&mut sequences, "source-0", Some(1)),
            Ok(false)
        );
        assert_eq!(
            accept_sequence(&mut sequences, "source-0", Some(2)),
            Ok(true)
        );
    }

    #[test]
    fn patches_and_clears_individual_keys() {
        let now = Instant::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("one")), ("model", Some("opus"))]),
            None,
            now,
        );
        tokens.patch(
            patch(&[("summary", Some("two")), ("model", None)]),
            None,
            now,
        );

        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "two".into())])
        );
    }

    #[test]
    fn ttl_only_changes_keys_in_the_patch() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("short", Some("one"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(patch(&[("persistent", Some("two"))]), None, now);

        assert!(tokens.expire_at(deadline));
        assert_eq!(
            tokens.values(),
            HashMap::from([("persistent".into(), "two".into())])
        );
    }

    #[test]
    fn values_remain_stable_until_expiry_mutates_state() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("temporary"))]),
            Some(Duration::from_secs(1)),
            now,
        );

        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "temporary".into())])
        );
        assert!(tokens.expire_at(deadline));
        assert!(tokens.values().is_empty());
    }

    #[test]
    fn delayed_expiry_sweep_removes_every_token_due_at_now() {
        let now = Instant::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("first", Some("one"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(
            patch(&[("second", Some("two"))]),
            Some(Duration::from_secs(2)),
            now,
        );

        assert!(tokens.expire_at(now + Duration::from_secs(10)));
        assert!(tokens.values().is_empty());
    }

    #[test]
    fn stale_expiry_does_not_clear_replacement() {
        let now = Instant::now();
        let first_deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("old"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(
            patch(&[("summary", Some("new"))]),
            Some(Duration::from_secs(5)),
            now,
        );

        assert!(!tokens.expire_at(first_deadline));
        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "new".into())])
        );
    }

    #[test]
    fn a_token_without_a_ttl_persists_with_no_deadline_and_comes_back_live() {
        let now = Instant::now();
        let wall_now = SystemTime::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(patch(&[("owner", Some("firstmate"))]), None, now);

        let persisted = tokens.to_persisted(now, wall_now);
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].expires_at_ms, None);

        // A whole day later, in a new process, it is still there.
        let mut restored = MetadataTokens::default();
        let later = now + Duration::from_secs(86_400);
        restored.restore_persisted(persisted, later, wall_now + Duration::from_secs(86_400));
        assert_eq!(
            restored.values(),
            HashMap::from([("owner".into(), "firstmate".into())])
        );
        assert_eq!(restored.next_expiry(), None);
    }

    #[test]
    fn a_ttl_survives_the_round_trip_as_the_same_wall_clock_moment() {
        let now = Instant::now();
        let wall_now = SystemTime::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("review"))]),
            Some(Duration::from_secs(600)),
            now,
        );

        let persisted = tokens.to_persisted(now, wall_now);
        assert!(persisted[0].expires_at_ms.is_some());

        // Down for 9 minutes: 1 minute of the TTL is left, not the full 10.
        let restart = now + Duration::from_secs(540);
        let mut restored = MetadataTokens::default();
        restored.restore_persisted(persisted, restart, wall_now + Duration::from_secs(540));
        assert_eq!(
            restored.values(),
            HashMap::from([("summary".into(), "review".into())])
        );
        assert!(!restored.expire_at(restart + Duration::from_secs(59)));
        assert!(restored.expire_at(restart + Duration::from_secs(61)));
    }

    #[test]
    fn a_ttl_that_passed_while_the_server_was_down_is_not_resurrected() {
        let now = Instant::now();
        let wall_now = SystemTime::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("stale"))]),
            Some(Duration::from_secs(60)),
            now,
        );
        let persisted = tokens.to_persisted(now, wall_now);

        // Restart an hour later. Nothing was running to sweep it, so the
        // deadline is only respected if the restore honours it.
        let restart = now + Duration::from_secs(3_600);
        let mut restored = MetadataTokens::default();
        restored.restore_persisted(persisted, restart, wall_now + Duration::from_secs(3_600));
        assert!(restored.values().is_empty());
    }

    #[test]
    fn an_already_expired_token_is_never_written_to_the_session_file() {
        let now = Instant::now();
        let wall_now = SystemTime::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("gone"))]),
            Some(Duration::from_secs(1)),
            now,
        );

        let capture_at = now + Duration::from_secs(5);
        assert!(tokens
            .to_persisted(capture_at, wall_now + Duration::from_secs(5))
            .is_empty());
    }

    #[test]
    fn clear_revokes_every_token_including_ones_that_never_expire() {
        let now = Instant::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("owner", Some("firstmate")), ("summary", Some("review"))]),
            None,
            now,
        );

        assert!(tokens.clear());
        assert!(tokens.values().is_empty());
        // Idempotent: a second revoke reports nothing changed.
        assert!(!tokens.clear());
    }

    #[test]
    fn update_without_ttl_cancels_previous_expiry() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("temporary"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(patch(&[("summary", Some("persistent"))]), None, now);

        assert!(!tokens.expire_at(deadline));
        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "persistent".into())])
        );
    }
}
