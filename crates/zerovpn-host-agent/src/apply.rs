//! Non-production transaction coordinator. A real host backend is not wired in yet.

use crate::state::{AppliedState, DesiredState, StateStore};
use anyhow::{Context, Result, anyhow, ensure};

/// The backend must snapshot *both* WireGuard and the dedicated firewall state.
/// `stage` must not activate the candidate. `rollback` must restore the snapshot
/// after a partial stage, activate, or verify failure.
pub trait HostBackend {
    type Snapshot;

    fn snapshot(&mut self) -> Result<Self::Snapshot>;
    fn stage(&mut self, candidate: &DesiredState) -> Result<()>;
    fn activate(&mut self) -> Result<()>;
    fn verify(&mut self, candidate: &DesiredState) -> Result<()>;
    fn rollback(&mut self, snapshot: Self::Snapshot) -> Result<()>;
}

pub struct ApplyOutcome {
    pub applied: AppliedState,
    /// A failed final audit must not be mistaken for a failed host apply.
    pub final_audit_written: bool,
}

pub struct ApplyCoordinator<B> {
    backend: B,
    store: StateStore,
}

impl<B: HostBackend> ApplyCoordinator<B> {
    pub fn new(backend: B, store: StateStore) -> Self { Self { backend, store } }

    /// Serial by `&mut self`. Do not expose this through the socket until a
    /// real backend, process-level lock, and recovery path have been tested.
    pub fn apply(&mut self, candidate: DesiredState) -> Result<ApplyOutcome> {
        let _process_lock = self.store.acquire_apply_lock().context("acquire host apply lock")?;
        let previous = self.store.load().context("load last-known-good")?;
        let prior_revision = previous.as_ref().map_or(0, |s| s.state.revision);
        let digest = candidate.digest()?;
        if candidate.revision <= prior_revision {
            self.store.audit("rejected_stale_revision", candidate.revision, &digest)?;
            return Err(anyhow!("stale_revision"));
        }
        if previous.as_ref().is_some_and(|s| s.state.server_public_key != candidate.server_public_key) {
            self.store.audit("rejected_server_identity_change", candidate.revision, &digest)?;
            return Err(anyhow!("server_identity_change"));
        }
        if let Err(error) = candidate.validate(prior_revision) {
            self.store.audit("rejected_invalid_state", candidate.revision, &digest)?;
            return Err(error.context("invalid_state"));
        }
        self.store.audit("apply_started", candidate.revision, &digest)?;
        let snapshot = match self.backend.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.store.audit("snapshot_failed", candidate.revision, &digest)?;
                return Err(error.context("snapshot failed before mutation"));
            }
        };
        let host_result = self.backend.stage(&candidate)
            .and_then(|_| self.backend.activate())
            .and_then(|_| self.backend.verify(&candidate));
        if let Err(error) = host_result {
            return self.rollback_failure(snapshot, candidate.revision, &digest, error);
        }
        // Audit before committing the durable state. If this fails, roll back.
        if let Err(error) = self.store.audit("apply_verified", candidate.revision, &digest) {
            return self.rollback_failure(snapshot, candidate.revision, &digest, error);
        }
        let target_revision = candidate.revision;
        let applied = match self.store.record_applied(candidate) {
            Ok(applied) => applied,
            Err(error) => {
                // A rename may have succeeded before fsync failed. Rolling back
                // then would make the durable state disagree with the host.
                if self.store.load().ok().flatten().is_some_and(|s|
                    s.state.revision > prior_revision && s.digest == digest)
                {
                    let _ = self.store.audit("commit_uncertain", target_revision, &digest);
                    return Err(error.context("commit_uncertain: host state retained; operator recovery required"));
                }
                return self.rollback_failure(snapshot, target_revision, &digest, error);
            }
        };
        ensure!(applied.digest == digest, "committed digest mismatch");
        let final_audit_written = self.store.audit("applied", applied.state.revision, &digest).is_ok();
        Ok(ApplyOutcome { applied, final_audit_written })
    }

    fn rollback_failure<T>(&mut self, snapshot: B::Snapshot, revision: u64, digest: &str, error: anyhow::Error) -> Result<T> {
        match self.backend.rollback(snapshot) {
            Ok(()) => {
                self.store.audit("apply_rolled_back", revision, digest)?;
                Err(error.context("apply failed; previous host state restored"))
            }
            Err(rollback_error) => {
                let _ = self.store.audit("rollback_failed", revision, digest);
                Err(anyhow!("apply failed: {error}; rollback failed: {rollback_error}; operator recovery required"))
            }
        }
    }

    pub fn backend(&self) -> &B { &self.backend }
    pub fn store(&self) -> &StateStore { &self.store }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DesiredPeer;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[derive(Default)]
    struct MockHost {
        live: Option<DesiredState>,
        staged: Option<DesiredState>,
        fail_at: Option<&'static str>,
        fail_rollback: bool,
        snapshots: usize,
    }

    impl HostBackend for MockHost {
        type Snapshot = Option<DesiredState>;
        fn snapshot(&mut self) -> Result<Self::Snapshot> {
            self.snapshots += 1;
            Ok(self.live.clone())
        }
        fn stage(&mut self, candidate: &DesiredState) -> Result<()> {
            self.staged = Some(candidate.clone());
            ensure!(self.fail_at != Some("stage"), "stage failure");
            Ok(())
        }
        fn activate(&mut self) -> Result<()> {
            self.live = self.staged.take();
            ensure!(self.fail_at != Some("activate"), "activate failure");
            Ok(())
        }
        fn verify(&mut self, candidate: &DesiredState) -> Result<()> {
            ensure!(self.fail_at != Some("verify"), "verify failure");
            ensure!(self.live.as_ref().is_some_and(|s| s.digest().ok() == candidate.digest().ok()), "host mismatch");
            Ok(())
        }
        fn rollback(&mut self, snapshot: Self::Snapshot) -> Result<()> {
            ensure!(!self.fail_rollback, "rollback failure");
            self.staged = None;
            self.live = snapshot;
            Ok(())
        }
    }

    fn candidate(revision: u64) -> DesiredState {
        DesiredState {
            revision,
            server_public_key: STANDARD.encode([1u8; 32]),
            peers: vec![DesiredPeer { public_key: STANDARD.encode([2u8; 32]), vpn_ip: "10.0.0.2".parse().unwrap(), enabled: true, preshared_key: None, persistent_keepalive: 30 }],
        }
    }

    #[test]
    fn success_commits_after_verification_and_replay_does_not_touch_host() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = ApplyCoordinator::new(MockHost::default(), StateStore::new(dir.path().join("state")));
        let result = engine.apply(candidate(1)).unwrap();
        assert_eq!(result.applied.state.revision, 1);
        assert!(result.final_audit_written);
        assert_eq!(engine.store().load().unwrap().unwrap().state.revision, 1);
        assert!(engine.apply(candidate(1)).is_err());
        assert_eq!(engine.backend().snapshots, 1);
    }

    #[test]
    fn each_failure_restores_previous_host_and_durable_state() {
        for phase in ["stage", "activate", "verify"] {
            let dir = tempfile::tempdir().unwrap();
            let host = MockHost { live: Some(candidate(1)), fail_at: Some(phase), ..Default::default() };
            let store = StateStore::new(dir.path().join("state"));
            store.record_applied(candidate(1)).unwrap();
            let mut engine = ApplyCoordinator::new(host, store);
            assert!(engine.apply(candidate(2)).is_err(), "{phase}");
            assert_eq!(engine.backend().live.as_ref().unwrap().revision, 1, "{phase}");
            assert_eq!(engine.store().load().unwrap().unwrap().state.revision, 1, "{phase}");
            let audit = std::fs::read_to_string(engine.store().directory().join("audit.jsonl")).unwrap();
            assert!(audit.contains("apply_rolled_back"), "{phase}");
        }
    }

    #[test]
    fn failed_rollback_is_reported_and_does_not_advance_journal() {
        let dir = tempfile::tempdir().unwrap();
        let host = MockHost {
            live: Some(candidate(1)),
            fail_at: Some("activate"),
            fail_rollback: true,
            ..Default::default()
        };
        let store = StateStore::new(dir.path().join("state"));
        store.record_applied(candidate(1)).unwrap();
        let mut engine = ApplyCoordinator::new(host, store);
        let error = engine.apply(candidate(2)).err().unwrap().to_string();
        assert!(error.contains("operator recovery required"));
        assert_eq!(engine.store().load().unwrap().unwrap().state.revision, 1);
        let audit = std::fs::read_to_string(engine.store().directory().join("audit.jsonl")).unwrap();
        assert!(audit.contains("rollback_failed"));
    }
}
