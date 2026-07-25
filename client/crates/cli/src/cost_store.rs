//! Durable local cost store under `~/.portzero/cost/`.
//!
//! Consent-gated (see `agents::cost_hook`): the store only exists for repos the
//! user has turned cost tracking on for. It caches agent-session cost metadata
//! and per-commit attributions so the MCP cost tools (`mcp_cost`) can answer
//! locally first — offline, and before merging cloud data.
//!
//! **Metadata only.** Nothing here holds prompt, code, or session *content* —
//! only token counts, model ids, dollar figures, commit SHAs, and timestamps.
//! The hook that populates it reads a transcript's `usage`/`model` fields and
//! never its message text.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One captured agent session (usage metadata only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredSession {
    pub session_id: String,
    pub agent: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    pub cost_provenance: String,
    pub repo: String,
    pub started_at: String,
    pub ended_at: String,
}

/// One commit's attributed cost within a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredCommitCost {
    pub repo: String,
    pub commit_sha: String,
    pub attributed_cost_usd: f64,
    pub confidence: String,
    pub cost_provenance: String,
    pub session_id: String,
}

/// The on-disk store: a flat list of sessions and per-commit costs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostStore {
    #[serde(default)]
    pub sessions: Vec<StoredSession>,
    #[serde(default)]
    pub commits: Vec<StoredCommitCost>,
}

/// Directory holding the local cost store (`~/.portzero/cost/`).
pub fn store_dir() -> Result<PathBuf> {
    let home = crate::client_home().ok_or_else(|| {
        anyhow::anyhow!(
            "Could not determine your home directory, so PortZero cannot locate \
             its cost store (~/.portzero/cost/store.json).\n\n\
             On Linux and macOS, set the HOME environment variable and try again. \
             On Windows this means the user profile folder could not be resolved, \
             which usually indicates a damaged profile or a service account with \
             no profile loaded."
        )
    })?;
    Ok(home.join(".portzero").join("cost"))
}

/// Path to the store file (`~/.portzero/cost/store.json`).
pub fn store_path() -> Result<PathBuf> {
    Ok(store_dir()?.join("store.json"))
}

impl CostStore {
    /// Load the store, returning an empty store when the file is absent.
    ///
    /// A malformed store file is an error (rather than silently discarding
    /// history), so the caller can surface it instead of overwriting data.
    pub fn load() -> Result<Self> {
        let path = store_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading local cost store at {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "parsing local cost store at {}.\n\n\
                 The file may be corrupted. Remove it to start fresh: rm {}",
                path.display(),
                path.display()
            )
        })
    }

    /// Atomically persist the store, creating `~/.portzero/cost/` if needed.
    pub fn save(&self) -> Result<()> {
        let dir = store_dir()?;
        portzero_daemon::secure_file::ensure_private_dir(&dir)
            .with_context(|| format!("creating cost store directory {}", dir.display()))?;
        let path = store_path()?;
        let json = serde_json::to_string_pretty(self).context("serializing local cost store")?;
        portzero_daemon::secure_file::write_secret_atomic(&path, json.as_bytes())
            .with_context(|| format!("writing local cost store to {}", path.display()))
    }

    /// Look up the stored cost for a single commit in a repo, if any.
    pub fn lookup_commit(&self, repo: &str, sha: &str) -> Option<&StoredCommitCost> {
        self.commits
            .iter()
            .find(|c| c.repo == repo && c.commit_sha == sha)
    }

    /// Sum the stored attributed cost for a set of commit SHAs in a repo.
    /// Returns the matched commit records so callers can build a breakdown.
    pub fn lookup_commits<'a>(&'a self, repo: &str, shas: &[String]) -> Vec<&'a StoredCommitCost> {
        shas.iter()
            .filter_map(|sha| self.lookup_commit(repo, sha))
            .collect()
    }

    /// Insert or replace a session, keyed by `session_id` (idempotent).
    pub fn upsert_session(&mut self, session: StoredSession) {
        if let Some(slot) = self
            .sessions
            .iter_mut()
            .find(|s| s.session_id == session.session_id)
        {
            *slot = session;
        } else {
            self.sessions.push(session);
        }
    }

    /// Insert or replace a commit cost, keyed by `(session_id, commit_sha)`.
    pub fn upsert_commit(&mut self, commit: StoredCommitCost) {
        if let Some(slot) = self
            .commits
            .iter_mut()
            .find(|c| c.session_id == commit.session_id && c.commit_sha == commit.commit_sha)
        {
            *slot = commit;
        } else {
            self.commits.push(commit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        crate::with_temp_home("cost-store", |_| f())
    }

    fn sample_commit(sha: &str, session: &str, cost: f64) -> StoredCommitCost {
        StoredCommitCost {
            repo: "acme/widget".to_string(),
            commit_sha: sha.to_string(),
            attributed_cost_usd: cost,
            confidence: "high".to_string(),
            cost_provenance: "measured".to_string(),
            session_id: session.to_string(),
        }
    }

    #[test]
    fn load_missing_store_is_empty() {
        with_temp_home(|| {
            let store = CostStore::load().unwrap();
            assert!(store.sessions.is_empty());
            assert!(store.commits.is_empty());
        });
    }

    #[test]
    fn roundtrip_persists_sessions_and_commits() {
        with_temp_home(|| {
            let mut store = CostStore::default();
            store.upsert_session(StoredSession {
                session_id: "s-1".to_string(),
                agent: "claude-code".to_string(),
                model: "claude-opus-4-8".to_string(),
                tokens_in: 100,
                tokens_out: 200,
                cost_usd: 0.42,
                cost_provenance: "measured".to_string(),
                repo: "acme/widget".to_string(),
                started_at: "2026-07-23 00:00:00".to_string(),
                ended_at: "2026-07-23 00:10:00".to_string(),
            });
            store.upsert_commit(sample_commit("abc123", "s-1", 0.42));
            store.save().unwrap();

            let reloaded = CostStore::load().unwrap();
            assert_eq!(reloaded.sessions.len(), 1);
            assert_eq!(reloaded.commits.len(), 1);
            let hit = reloaded.lookup_commit("acme/widget", "abc123").unwrap();
            assert_eq!(hit.attributed_cost_usd, 0.42);
            assert_eq!(hit.confidence, "high");
        });
    }

    #[test]
    fn upsert_is_idempotent_by_key() {
        let mut store = CostStore::default();
        store.upsert_commit(sample_commit("abc", "s-1", 1.0));
        store.upsert_commit(sample_commit("abc", "s-1", 2.0));
        assert_eq!(store.commits.len(), 1);
        assert_eq!(store.commits[0].attributed_cost_usd, 2.0);

        // Same sha, different session is a distinct row (cumulative-across-reopen).
        store.upsert_commit(sample_commit("abc", "s-2", 3.0));
        assert_eq!(store.commits.len(), 2);
    }

    #[test]
    fn lookup_commits_collects_matches_only() {
        let mut store = CostStore::default();
        store.upsert_commit(sample_commit("aaa", "s-1", 1.0));
        store.upsert_commit(sample_commit("bbb", "s-1", 2.0));
        let shas = vec!["aaa".to_string(), "ccc".to_string(), "bbb".to_string()];
        let hits = store.lookup_commits("acme/widget", &shas);
        assert_eq!(hits.len(), 2);
    }
}
