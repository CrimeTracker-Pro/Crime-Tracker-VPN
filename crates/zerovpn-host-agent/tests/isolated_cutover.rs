//! Opt-in only: run inside a disposable Docker network namespace with NET_ADMIN.
//! Never run this test on the deployment host network namespace.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use zerovpn_host_agent::apply::HostBackend;
use zerovpn_host_agent::apply_request::apply_state;
use zerovpn_host_agent::firewall_preflight::parse_iptables_save;
use zerovpn_host_agent::firewall_executor::FirewallCommands;
use zerovpn_host_agent::host_commands::{CommandHostOperations, HostCommandRunner, SystemHostCommandRunner};
use zerovpn_host_agent::host_cutover::HostCutoverBackend;
use zerovpn_host_agent::host_preflight::{HostProbe, SystemHostProbe};
use zerovpn_host_agent::peer_update::PeerUpdateBackend;
use zerovpn_host_agent::policy::{compile_host_forwarding, parse_ipv4_route_get};
use zerovpn_host_agent::recovery::{restore_from_journal, verify_restarted_host};
use zerovpn_host_agent::state::{AppliedState, DesiredPeer, DesiredState, StateStore};
use zerovpn_host_agent::startup::{StartupStatus, prepare_startup};
use zerovpn_host_agent::{Operation, Response, query};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program).args(args).status().unwrap();
    assert!(status.success(), "isolated setup command failed: {program} {args:?}");
}

fn public_key(private: &str) -> String {
    let mut child = Command::new("wg").arg("pubkey")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(format!("{private}\n").as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct FailLinkUp { inner: SystemHostCommandRunner }
impl HostCommandRunner for FailLinkUp {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&[u8]>) -> anyhow::Result<()> {
        if program == "ip" && args == ["link", "set", "dev", "wg0", "up"] {
            anyhow::bail!("injected link-up failure");
        }
        self.inner.run(program, args, stdin)
    }
    fn output(&mut self, program: &str, args: &[&str]) -> anyhow::Result<String> {
        self.inner.output(program, args)
    }
}

#[tokio::test]
#[ignore = "requires disposable Docker network namespace with NET_ADMIN"]
async fn real_namespace_first_cutover_and_rollback() {
    assert_eq!(std::env::var("VPN_AGENT_ISOLATED_NETNS").as_deref(), Ok("1"));
    assert!(std::path::Path::new("/.dockerenv").exists(), "must run inside Docker");

    run("ip", &["link", "add", "br-test", "type", "dummy"]);
    run("ip", &["address", "add", "172.18.0.1/24", "dev", "br-test"]);
    run("ip", &["link", "set", "dev", "br-test", "up"]);
    run("iptables", &["-P", "INPUT", "ACCEPT"]);
    run("iptables", &["-P", "FORWARD", "DROP"]);
    run("iptables", &["-N", "DOCKER-USER"]);
    run("iptables", &["-I", "FORWARD", "1", "-j", "DOCKER-USER"]);
    run("iptables", &["-t", "nat", "-N", "DOCKER"]);
    run("iptables", &["-t", "nat", "-A", "DOCKER", "!", "-i", "br-test", "-p", "tcp",
        "--dport", "443", "-j", "DNAT", "--to-destination", "172.18.0.2:443"]);

    let mut probe = SystemHostProbe;
    let facts = parse_iptables_save(&probe.iptables_save().unwrap()).unwrap();
    let target = "172.18.0.2".parse().unwrap();
    let route = parse_ipv4_route_get(&probe.route_get(target).unwrap(), target).unwrap();
    let server_private = STANDARD.encode([17u8; 32]);
    let server_public = public_key(&server_private);
    let peer_public = public_key(&STANDARD.encode([23u8; 32]));
    let state = DesiredState { revision: 1, server_public_key: server_public.clone(),
        peers: vec![DesiredPeer { public_key: peer_public, vpn_ip: "10.0.0.2".parse().unwrap(),
            enabled: true, preshared_key: Some(STANDARD.encode([29u8; 32])), persistent_keepalive: 30 }] };

    let operations = CommandHostOperations::new(SystemHostCommandRunner::default(), SystemHostProbe);
    let mut backend = HostCutoverBackend::new(operations, &facts, &route, server_private.clone(), server_public.clone())
        .await.unwrap();
    backend.snapshot().unwrap();
    backend.stage(&state).unwrap();
    backend.activate().unwrap();
    backend.verify(&state).unwrap();
    let active = SystemHostProbe.iptables_save().unwrap();
    assert!(active.contains("ZEROVPN-HOST-FWD"));
    assert!(active.contains("ZEROVPN-HOST-NAT"));
    let applied = AppliedState { digest: state.digest().unwrap(), state: state.clone() };
    verify_restarted_host(&applied, &mut SystemHostProbe, &mut SystemHostCommandRunner::default()).unwrap();
    let mut tampered = applied.clone();
    tampered.state.peers[0].vpn_ip = "10.0.0.3".parse().unwrap();
    assert!(verify_restarted_host(&tampered, &mut SystemHostProbe,
        &mut SystemHostCommandRunner::default()).is_err());
    let mut wrong_psk = applied.clone();
    wrong_psk.state.peers[0].preshared_key = Some(STANDARD.encode([30u8; 32]));
    wrong_psk.digest = wrong_psk.state.digest().unwrap();
    assert!(verify_restarted_host(&wrong_psk, &mut SystemHostProbe,
        &mut SystemHostCommandRunner::default()).is_err());
    let restarted = CommandHostOperations::new(SystemHostCommandRunner::default(), SystemHostProbe);
    let mut restarted = HostCutoverBackend::new(restarted, &facts, &route,
        server_private.clone(), server_public.clone()).await.unwrap();
    assert!(restarted.snapshot().is_err(), "new first-cutover process must not seize a live wg0");
    backend.rollback(()).unwrap();
    assert!(!SystemHostProbe.wg0_exists().unwrap());
    let restored = SystemHostProbe.iptables_save().unwrap();
    assert!(!restored.contains("ZEROVPN-HOST-FWD"));
    assert!(!restored.contains("ZEROVPN-HOST-NAT"));

    let directory = tempfile::tempdir().unwrap();
    let store = StateStore::new(directory.path().join("agent"));
    store.record_applied(state.clone()).unwrap();
    let restoring = CommandHostOperations::new(SystemHostCommandRunner::default(), SystemHostProbe);
    let mut restoring = HostCutoverBackend::new(restoring, &facts, &route,
        server_private.clone(), server_public.clone()).await.unwrap();
    assert_eq!(restore_from_journal(&store, &mut restoring).unwrap().state.revision, 1);
    assert!(SystemHostProbe.wg0_exists().unwrap());
    let previous = store.load().unwrap().unwrap();
    let mut updater = PeerUpdateBackend::new(SystemHostCommandRunner::default(), SystemHostProbe,
        previous, server_private.clone()).await.unwrap();
    let old_config = updater.snapshot().unwrap();
    let mut updated = state.clone();
    updated.revision = 2;
    updated.peers[0].persistent_keepalive = 25;
    updater.stage(&updated).unwrap();
    updater.activate().unwrap();
    updater.verify(&updated).unwrap();
    updater.rollback(old_config).unwrap();
    verify_restarted_host(&store.load().unwrap().unwrap(), &mut SystemHostProbe,
        &mut SystemHostCommandRunner::default()).unwrap();
    restoring.rollback(()).unwrap();

    let failing = CommandHostOperations::new(FailLinkUp { inner: SystemHostCommandRunner::default() }, SystemHostProbe);
    let mut failing = HostCutoverBackend::new(failing, &facts, &route,
        server_private.clone(), server_public)
        .await.unwrap();
    assert!(restore_from_journal(&store, &mut failing).is_err());
    assert!(!SystemHostProbe.wg0_exists().unwrap());
    let restored = SystemHostProbe.iptables_save().unwrap();
    assert!(!restored.contains("ZEROVPN-HOST-FWD"));
    assert!(!restored.contains("ZEROVPN-HOST-NAT"));

    let key_path = directory.path().join("server.private");
    assert!(prepare_startup(&store, &key_path, SystemHostCommandRunner::default(), SystemHostProbe)
        .await.is_err());
    assert!(!SystemHostProbe.wg0_exists().unwrap());
    std::fs::write(&key_path, format!("{server_private}\n")).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(prepare_startup(&store, &key_path, SystemHostCommandRunner::default(), SystemHostProbe)
        .await.unwrap(), StartupStatus::Restored);
    assert_eq!(prepare_startup(&store, &key_path, SystemHostCommandRunner::default(), SystemHostProbe)
        .await.unwrap(), StartupStatus::ActiveVerified);
    let mut next = state.clone();
    next.revision = 2;
    next.peers[0].persistent_keepalive = 25;
    assert_eq!(apply_state(next.clone(), store.directory(), &key_path).await.unwrap().revision, 2);
    assert_eq!(store.load().unwrap().unwrap().state.revision, 2);
    verify_restarted_host(&store.load().unwrap().unwrap(), &mut SystemHostProbe,
        &mut SystemHostCommandRunner::default()).unwrap();
    assert!(apply_state(next, store.directory(), &key_path).await.is_err());

    SystemHostCommandRunner::default().run("ip", &["link", "delete", "dev", "wg0"], None).unwrap();
    let firewall = FirewallCommands::from_plan(&compile_host_forwarding(&facts, &route).unwrap()).unwrap();
    let mut operations = CommandHostOperations::new(SystemHostCommandRunner::default(), SystemHostProbe);
    firewall.uninstall(&mut operations).unwrap();
    let first_directory = tempfile::tempdir().unwrap();
    let first_store = StateStore::new(first_directory.path().join("agent"));
    assert_eq!(apply_state(state.clone(), first_store.directory(), &key_path).await.unwrap().revision, 1);
    assert_eq!(first_store.load().unwrap().unwrap().state.revision, 1);
    verify_restarted_host(&first_store.load().unwrap().unwrap(), &mut SystemHostProbe,
        &mut SystemHostCommandRunner::default()).unwrap();

    let read_token_path = first_directory.path().join("read.token");
    let apply_token_path = first_directory.path().join("apply.token");
    let read_token = "read-token-0123456789-abcdefghijkl";
    let apply_token = "apply-token-0123456789-abcdefghijk";
    for (path, token) in [(&read_token_path, read_token), (&apply_token_path, apply_token)] {
        std::fs::write(path, format!("{token}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let socket = first_directory.path().join("agent.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_zerovpn-host-agent"))
        .env("VPN_AGENT_SOCKET", &socket)
        .env("VPN_AGENT_TOKEN_FILE", &read_token_path)
        .env("VPN_AGENT_APPLY_TOKEN_FILE", &apply_token_path)
        .env("VPN_AGENT_SERVER_PRIVATE_KEY_FILE", &key_path)
        .env("VPN_AGENT_STATE_DIR", first_store.directory())
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().unwrap();
    let mut child = ChildGuard(child);
    for _ in 0..100 {
        if socket.exists() { break; }
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("isolated agent exited before opening socket: {status}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(socket.exists(), "isolated agent socket did not appear");
    assert!(matches!(query(&socket, read_token, Operation::Health).await.unwrap(), Response::Health { .. }));
    let mut socket_update = state.clone();
    socket_update.revision = 2;
    socket_update.peers[0].persistent_keepalive = 26;
    assert!(matches!(query(&socket, read_token, Operation::ApplyState { state: socket_update.clone() }).await.unwrap(),
        Response::Error { code, .. } if code == "unauthorized"));
    assert!(matches!(query(&socket, apply_token, Operation::ApplyState { state: socket_update }).await.unwrap(),
        Response::Applied { revision: 2, .. }));
    assert_eq!(first_store.load().unwrap().unwrap().state.revision, 2);
}
