//! Existing-interface peer update transaction. Not exposed through the socket.

use crate::apply::HostBackend;
use crate::host_commands::{HostCommandRunner, verify_wireguard_state};
use crate::host_preflight::HostProbe;
use crate::recovery::verify_restarted_host;
use crate::state::{AppliedState, DesiredState};
use crate::wireguard_config::{SensitiveWgConfig, render, verify_server_identity};
use anyhow::{Result, ensure};
use zeroize::Zeroizing;

pub struct PeerUpdateBackend<R, P> {
    runner: R,
    probe: P,
    previous: AppliedState,
    server_private_key: Zeroizing<String>,
    staged: Option<SensitiveWgConfig>,
}

impl<R: HostCommandRunner, P: HostProbe> PeerUpdateBackend<R, P> {
    pub async fn new(runner: R, probe: P, previous: AppliedState, private_key: String) -> Result<Self> {
        let private_key = Zeroizing::new(private_key);
        verify_server_identity(&private_key, &previous.state.server_public_key).await?;
        Ok(Self { runner, probe, previous, server_private_key: private_key, staged: None })
    }
}

impl<R: HostCommandRunner, P: HostProbe> HostBackend for PeerUpdateBackend<R, P> {
    /// Secret-bearing `wg showconf` output, cleared on drop.
    type Snapshot = Zeroizing<String>;

    fn snapshot(&mut self) -> Result<Self::Snapshot> {
        verify_restarted_host(&self.previous, &mut self.probe, &mut self.runner)?;
        let snapshot = Zeroizing::new(self.runner.output("wg", &["showconf", "wg0"])?);
        // Detect an external mutation during the two observations.
        verify_restarted_host(&self.previous, &mut self.probe, &mut self.runner)?;
        Ok(snapshot)
    }

    fn stage(&mut self, candidate: &DesiredState) -> Result<()> {
        ensure!(candidate.revision > self.previous.state.revision, "stale peer update revision");
        ensure!(candidate.server_public_key == self.previous.state.server_public_key,
            "server identity changed");
        self.staged = Some(render(candidate, &self.server_private_key)?);
        Ok(())
    }

    fn activate(&mut self) -> Result<()> {
        let config = self.staged.as_ref().ok_or_else(|| anyhow::anyhow!("candidate not staged"))?;
        self.runner.run("wg", &["syncconf", "wg0", "/dev/stdin"], Some(config.as_bytes()))
    }

    fn verify(&mut self, candidate: &DesiredState) -> Result<()> {
        verify_wireguard_state(&mut self.runner, candidate)?;
        let applied = AppliedState { state: candidate.clone(), digest: candidate.digest()? };
        verify_restarted_host(&applied, &mut self.probe, &mut self.runner)
    }

    fn rollback(&mut self, snapshot: Self::Snapshot) -> Result<()> {
        self.runner.run("wg", &["syncconf", "wg0", "/dev/stdin"], Some(snapshot.as_bytes()))?;
        verify_restarted_host(&self.previous, &mut self.probe, &mut self.runner)?;
        self.staged = None;
        Ok(())
    }
}
