use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Proof that a specific SHA was validated on a specific target.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidenceRecord {
    /// Validated git SHA.
    pub sha: String,
    /// Git branch associated with the run.
    pub branch: String,
    /// Logical target name.
    #[serde(rename = "target")]
    pub target_name: String,
    /// Concrete platform label.
    pub platform: String,
    /// Validation result status.
    pub status: String,
    /// Backend that produced this evidence.
    pub backend: String,
    /// Completion timestamp.
    pub completed_at: DateTime<Utc>,
    /// Optional duration in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Optional host identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Primary backend when failover occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_backend: Option<String>,
    /// Failover reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failover_reason: Option<String>,
    /// Cloud provider label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Runner profile label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_profile: Option<String>,
    /// Coarse failure class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    /// Ancestor SHA this record reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_from: Option<String>,
    /// Digest of the contract in effect when the record was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_digest: Option<String>,
    /// Stable signature of the stage pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stages_signature: Option<String>,
}

impl EvidenceRecord {
    /// Whether this record is a passing validation.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.status == "pass"
    }

    /// Whether this record was synthesized from an ancestor pass.
    #[must_use]
    pub fn reused(&self) -> bool {
        self.reused_from.is_some()
    }
}

/// Persistent store for per-branch evidence records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceStore {
    path: PathBuf,
}

impl EvidenceStore {
    /// Open an evidence store at the given path.
    pub fn new(path: PathBuf) -> Result<Self, std::io::Error> {
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// Backing path of the store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Store or replace a record for the same branch and target.
    pub fn record(&self, evidence: &EvidenceRecord) -> Result<(), Box<dyn std::error::Error>> {
        let branch_key = sanitize_branch(&evidence.branch);
        let mut records = self.load_branch(&branch_key)?;
        records.insert(evidence.target_name.clone(), evidence.clone());
        self.save_branch(&branch_key, &records)
    }

    /// Return all evidence for a branch keyed by target name.
    #[must_use]
    pub fn get_branch(&self, branch: &str) -> BTreeMap<String, EvidenceRecord> {
        self.load_branch(&sanitize_branch(branch))
            .unwrap_or_default()
    }

    /// Return evidence for a specific branch and target, if present.
    #[must_use]
    pub fn get_target(&self, branch: &str, target_name: &str) -> Option<EvidenceRecord> {
        self.get_branch(branch).remove(target_name)
    }

    /// Return whether every required platform has passing evidence for the SHA.
    #[must_use]
    pub fn is_merge_ready(
        &self,
        branch: &str,
        sha: &str,
        required_platforms: &[String],
    ) -> (bool, BTreeMap<String, Option<EvidenceRecord>>) {
        let records = self.get_branch(branch);
        let mut evidence_map = BTreeMap::new();
        let mut all_green = true;

        for platform in required_platforms {
            let record = records
                .values()
                .find(|record| record.platform == *platform && record.sha == sha && record.passed())
                .cloned();
            if record.is_none() {
                all_green = false;
            }
            evidence_map.insert(platform.clone(), record);
        }

        (all_green, evidence_map)
    }

    /// Find the highest-ranked passing record for a target across all branches.
    #[must_use]
    pub fn query_passing_for_target(
        &self,
        target_name: &str,
        sha_candidates: &[String],
    ) -> Option<EvidenceRecord> {
        let candidate_ranks = sha_candidates
            .iter()
            .enumerate()
            .map(|(rank, sha)| (sha.as_str(), rank))
            .collect::<BTreeMap<_, _>>();
        let mut best: Option<(usize, EvidenceRecord)> = None;

        let Ok(entries) = fs::read_dir(&self.path) else {
            return None;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
                continue;
            }
            let Some(branch_key) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
                continue;
            };
            let Ok(records) = self.load_branch(branch_key) else {
                continue;
            };
            for record in records.values() {
                if record.target_name != target_name || !record.passed() || record.reused() {
                    continue;
                }
                let Some(rank) = candidate_ranks.get(record.sha.as_str()).copied() else {
                    continue;
                };
                if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
                    best = Some((rank, record.clone()));
                }
            }
        }

        best.map(|(_, record)| record)
    }

    fn branch_file(&self, branch_key: &str) -> PathBuf {
        self.path.join(format!("{branch_key}.json"))
    }

    fn load_branch(
        &self,
        branch_key: &str,
    ) -> Result<BTreeMap<String, EvidenceRecord>, Box<dyn std::error::Error>> {
        let path = self.branch_file(branch_key);
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let contents = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&contents)?)
    }

    fn save_branch(
        &self,
        branch_key: &str,
        records: &BTreeMap<String, EvidenceRecord>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let payload = serde_json::to_string_pretty(records)?;
        let temp = tempfile::NamedTempFile::new_in(&self.path)?;
        fs::write(temp.path(), format!("{payload}\n"))?;
        temp.persist(self.branch_file(branch_key))?;
        Ok(())
    }
}

fn sanitize_branch(branch: &str) -> String {
    branch.replace(['/', '\\'], "--")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{EvidenceRecord, EvidenceStore};

    fn record(branch: &str, target: &str, sha: &str) -> EvidenceRecord {
        EvidenceRecord {
            sha: sha.to_owned(),
            branch: branch.to_owned(),
            target_name: target.to_owned(),
            platform: format!("{target}-platform"),
            status: "pass".to_owned(),
            backend: "local".to_owned(),
            completed_at: Utc::now(),
            duration_secs: None,
            host: None,
            primary_backend: None,
            failover_reason: None,
            provider: None,
            runner_profile: None,
            failure_class: None,
            reused_from: None,
            contract_digest: None,
            stages_signature: None,
        }
    }

    #[test]
    fn evidence_record_round_trips_reuse_fields() {
        let record = EvidenceRecord {
            sha: "new".to_owned(),
            branch: "feat/x".to_owned(),
            target_name: "mac".to_owned(),
            platform: "macos-arm64".to_owned(),
            status: "pass".to_owned(),
            backend: "reused".to_owned(),
            completed_at: Utc::now(),
            duration_secs: None,
            host: None,
            primary_backend: None,
            failover_reason: None,
            provider: None,
            runner_profile: None,
            failure_class: None,
            reused_from: Some("old".to_owned()),
            contract_digest: Some("abc123".to_owned()),
            stages_signature: Some("build|test".to_owned()),
        };

        assert!(record.passed());
        assert!(record.reused());

        let value = serde_json::to_value(&record).expect("serialize");
        assert_eq!(value["target"], "mac");
        assert_eq!(value["reused_from"], "old");
        assert_eq!(value["contract_digest"], "abc123");
        assert_eq!(value["stages_signature"], "build|test");

        let restored: EvidenceRecord = serde_json::from_value(value).expect("deserialize");
        assert_eq!(restored, record);
    }

    #[test]
    fn evidence_record_omits_reuse_fields_when_absent() {
        let record = record("main", "mac", "abc");
        let value = serde_json::to_value(&record).expect("serialize");
        assert!(value.get("reused_from").is_none());
        assert!(value.get("target_name").is_none());
    }

    #[test]
    fn record_and_retrieve_branch_data() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = EvidenceStore::new(temp.path().join("evidence")).expect("store");
        let record = record("feat/x", "mac", "abc");

        store.record(&record).expect("record");

        assert_eq!(
            store.get_target("feat/x", "mac").expect("record").sha,
            "abc"
        );
        assert_eq!(store.get_branch("feat/x").len(), 1);
    }

    #[test]
    fn latest_record_overwrites_by_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = EvidenceStore::new(temp.path().join("evidence")).expect("store");

        store.record(&record("main", "mac", "old")).expect("record");
        store.record(&record("main", "mac", "new")).expect("record");

        assert_eq!(store.get_target("main", "mac").expect("record").sha, "new");
    }

    #[test]
    fn branch_names_are_safely_sanitized() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = EvidenceStore::new(temp.path().join("evidence")).expect("store");
        store
            .record(&record("feat/x\\nested", "mac", "abc"))
            .expect("record");

        assert!(store.path().join("feat--x--nested.json").exists());
        assert!(store.get_target("feat/x\\nested", "mac").is_some());
    }

    #[test]
    fn merge_ready_requires_all_required_platforms() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = EvidenceStore::new(temp.path().join("evidence")).expect("store");
        let mut mac = record("main", "mac", "abc");
        mac.platform = "macos-arm64".to_owned();
        let mut linux = record("main", "linux", "abc");
        linux.platform = "linux-x64".to_owned();
        store.record(&mac).expect("record");
        store.record(&linux).expect("record");

        let (ready, evidence) = store.is_merge_ready(
            "main",
            "abc",
            &["macos-arm64".to_owned(), "linux-x64".to_owned()],
        );

        assert!(ready);
        assert!(evidence.values().all(Option::is_some));
    }

    #[test]
    fn merge_ready_rejects_missing_wrong_or_failed_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = EvidenceStore::new(temp.path().join("evidence")).expect("store");

        let mut wrong_sha = record("main", "mac", "old");
        wrong_sha.platform = "macos-arm64".to_owned();
        let mut failed = record("main", "linux", "abc");
        failed.platform = "linux-x64".to_owned();
        failed.status = "fail".to_owned();
        store.record(&wrong_sha).expect("record");
        store.record(&failed).expect("record");

        let (ready, evidence) = store.is_merge_ready(
            "main",
            "abc",
            &["macos-arm64".to_owned(), "linux-x64".to_owned()],
        );

        assert!(!ready);
        assert!(evidence["macos-arm64"].is_none());
        assert!(evidence["linux-x64"].is_none());
    }

    #[test]
    fn store_persists_across_instances() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("evidence");
        let store = EvidenceStore::new(path.clone()).expect("store");
        store.record(&record("main", "mac", "abc")).expect("record");

        let reopened = EvidenceStore::new(path).expect("store");
        assert_eq!(
            reopened.get_target("main", "mac").expect("record").sha,
            "abc"
        );
    }

    #[test]
    fn query_passing_for_target_uses_candidate_rank_and_filters_invalid_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = EvidenceStore::new(temp.path().join("evidence")).expect("store");

        store
            .record(&record("ba", "mac", &"a".repeat(40)))
            .expect("record");
        store
            .record(&record("bb", "mac", &"b".repeat(40)))
            .expect("record");
        store
            .record(&record("bc", "mac", &"c".repeat(40)))
            .expect("record");

        let mut reused = record("main", "mac", "abc");
        reused.backend = "reused".to_owned();
        reused.reused_from = Some("parent".to_owned());
        store.record(&reused).expect("record");

        let mut failed = record("main", "linux", "abc");
        failed.status = "fail".to_owned();
        store.record(&failed).expect("record");

        let candidates = vec!["b".repeat(40), "c".repeat(40), "a".repeat(40)];
        let match_record = store
            .query_passing_for_target("mac", &candidates)
            .expect("record");
        assert_eq!(match_record.sha, "b".repeat(40));
        assert!(
            store
                .query_passing_for_target("linux", &["abc".to_owned()])
                .is_none()
        );
        assert!(
            store
                .query_passing_for_target("unknown", &["abc".to_owned()])
                .is_none()
        );
    }
}
