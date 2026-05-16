use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct FakeTailscalePeer {
    pub node_key: String,
    pub ip: String,
    pub hostname: String,
    pub ee_version: String,
    pub ee_protocol: String,
    pub workspace_ids: Vec<String>,
    pub tags: Vec<String>,
    pub respond: bool,
    pub latency_ms: u64,
}

#[derive(Clone, Debug)]
pub struct FakeTailscaleScenario {
    pub name: String,
    pub node_key: String,
    pub ip: String,
    pub tailnet_id: String,
    pub display_name: String,
    pub platform: String,
    pub authenticated: bool,
    pub daemon_state: String,
    pub peers: Vec<FakeTailscalePeer>,
}

#[derive(Clone, Debug)]
pub struct FakeTailscaleEnv {
    pub scenario_dir: PathBuf,
    pub env: Vec<(String, String)>,
}

impl FakeTailscaleScenario {
    #[must_use]
    pub fn builder(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            node_key: deterministic_node_key(&format!("{name}:self")),
            name,
            ip: "100.64.0.1".to_owned(),
            tailnet_id: "tailnet-alpha".to_owned(),
            display_name: "ee-local".to_owned(),
            platform: "linux".to_owned(),
            authenticated: true,
            daemon_state: "running".to_owned(),
            peers: Vec::new(),
        }
    }

    #[must_use]
    pub fn self_node(
        mut self,
        node_key: impl Into<String>,
        ip: impl Into<String>,
        tailnet_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        self.node_key = node_key.into();
        self.ip = ip.into();
        self.tailnet_id = tailnet_id.into();
        self.display_name = display_name.into();
        self
    }

    #[must_use]
    pub fn daemon_state(mut self, state: impl Into<String>) -> Self {
        self.daemon_state = state.into();
        self
    }

    #[must_use]
    pub fn platform(mut self, platform: impl Into<String>) -> Self {
        self.platform = platform.into();
        self
    }

    #[must_use]
    pub fn authenticated(mut self, authenticated: bool) -> Self {
        self.authenticated = authenticated;
        self
    }

    #[must_use]
    pub fn swap_tailnet(
        mut self,
        tailnet_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        self.tailnet_id = tailnet_id.into();
        self.display_name = display_name.into();
        self
    }

    #[must_use]
    pub fn add_peer(
        mut self,
        node_key: impl Into<String>,
        ip: impl Into<String>,
        hostname: impl Into<String>,
        workspace_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.peers.push(FakeTailscalePeer {
            node_key: node_key.into(),
            ip: ip.into(),
            hostname: hostname.into(),
            ee_version: "0.2.0".to_owned(),
            ee_protocol: "1.0".to_owned(),
            workspace_ids: workspace_ids.into_iter().map(Into::into).collect(),
            tags: vec!["ee-mesh".to_owned()],
            respond: true,
            latency_ms: 0,
        });
        self.peers
            .sort_by(|left, right| left.node_key.cmp(&right.node_key));
        self
    }

    #[must_use]
    pub fn remove_peer(mut self, node_key: &str) -> Self {
        self.peers.retain(|peer| peer.node_key != node_key);
        self
    }

    #[must_use]
    pub fn set_peer_response(mut self, node_key: &str, respond: bool) -> Self {
        for peer in &mut self.peers {
            if peer.node_key == node_key {
                peer.respond = respond;
            }
        }
        self
    }

    pub fn write_to(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
        fs::create_dir_all(dir.join("bin")).map_err(|error| format!("create bin: {error}"))?;
        fs::create_dir_all(dir.join("responders"))
            .map_err(|error| format!("create responders: {error}"))?;
        fs::create_dir_all(dir.join("events"))
            .map_err(|error| format!("create events: {error}"))?;
        let status = self.status_json();
        fs::write(
            dir.join("tailscale_status.json"),
            serde_json::to_vec_pretty(&status).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("write status: {error}"))?;
        fs::write(
            dir.join("control.json"),
            serde_json::to_vec_pretty(&self.control_json()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("write control: {error}"))?;
        Ok(())
    }

    pub fn corrupt_status_json(dir: &Path, kind: &str) -> Result<(), String> {
        let path = dir.join("tailscale_status.json");
        match kind {
            "truncated" => fs::write(&path, b"{\"Version\":\"fake-tailscale.v1\",\"Peer\":")
                .map_err(|error| format!("write truncated status: {error}")),
            "invalid_utf8" => fs::write(&path, b"{\"Version\":\"fake\"}\xff\n")
                .map_err(|error| format!("write invalid utf8 status: {error}")),
            "wrong_schema" => fs::write(&path, b"{\"schema\":\"wrong\",\"Peer\":[]}\n")
                .map_err(|error| format!("write wrong schema status: {error}")),
            "unknown_fields" => {
                let text = fs::read_to_string(&path)
                    .map_err(|error| format!("read status for unknown_fields: {error}"))?;
                let mut status: Value =
                    serde_json::from_str(&text).map_err(|error| error.to_string())?;
                status["UnexpectedFakeField"] = json!({"why": "corrupt_status_json"});
                fs::write(
                    &path,
                    serde_json::to_vec_pretty(&status).map_err(|error| error.to_string())?,
                )
                .map_err(|error| format!("write unknown_fields status: {error}"))
            }
            other => Err(format!("unknown corrupt status kind: {other}")),
        }
    }

    #[must_use]
    pub fn status_json(&self) -> Value {
        let mut peers = serde_json::Map::new();
        for peer in &self.peers {
            peers.insert(
                peer.node_key.clone(),
                json!({
                    "ID": peer.node_key,
                    "HostName": peer.hostname,
                    "DNSName": format!("{}.tailnet.test.", peer.hostname),
                    "TailscaleIPs": [peer.ip],
                    "Tags": peer.tags,
                    "Online": peer.respond,
                    "Capabilities": {
                        "eeVersion": peer.ee_version,
                        "eeProtocol": peer.ee_protocol,
                        "workspaceIds": peer.workspace_ids,
                        "respond": peer.respond,
                        "latencyMs": peer.latency_ms,
                    }
                }),
            );
        }
        json!({
            "Version": "fake-tailscale.v1",
            "BackendState": if self.daemon_state == "running" { "Running" } else { "NoState" },
            "TUN": self.daemon_state == "running",
            "Self": {
                "ID": self.node_key,
                "HostName": self.display_name,
                "DNSName": format!("{}.tailnet.test.", self.display_name),
                "TailscaleIPs": [self.ip],
                "Tailnet": self.tailnet_id,
                "TailnetName": self.display_name,
                "Authenticated": self.authenticated,
                "Platform": self.platform,
            },
            "Peer": Value::Object(peers),
        })
    }

    #[must_use]
    pub fn control_json(&self) -> Value {
        json!({
            "schema": "ee.fake_tailscale.control.v1",
            "scenario": self.name,
            "daemonState": self.daemon_state,
            "peerCount": self.peers.len(),
        })
    }

    pub fn with_scenario<R>(
        &self,
        dir: &Path,
        f: impl FnOnce(&FakeTailscaleEnv) -> R,
    ) -> Result<R, String> {
        self.write_to(dir)?;
        let env = FakeTailscaleEnv {
            scenario_dir: dir.to_path_buf(),
            env: vec![
                (
                    "EE_TAILSCALE_BINARY_OVERRIDE".to_owned(),
                    dir.join("bin").join("tailscale").display().to_string(),
                ),
                (
                    "EE_TAILSCALE_PROBE_SOCKET_OVERRIDE".to_owned(),
                    dir.join("responders").display().to_string(),
                ),
            ],
        };
        Ok(f(&env))
    }
}

impl FakeTailscaleEnv {
    #[must_use]
    pub fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn expect_event(&self, phase: &str, valid: bool) -> Result<(), String> {
        let event_path = self.scenario_dir.join("events.jsonl");
        let text = fs::read_to_string(&event_path)
            .map_err(|error| format!("read {}: {error}", event_path.display()))?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let event: Value = serde_json::from_str(line)
                .map_err(|error| format!("parse event: {error}; line={line}"))?;
            if event["phase"] == phase && event["valid"] == valid {
                return Ok(());
            }
        }
        Err(format!(
            "missing event phase={phase} valid={valid} in {}",
            event_path.display()
        ))
    }

    pub fn emit_event(&self, phase: &str, valid: bool, detail: &str) -> Result<(), String> {
        let event_path = self.scenario_dir.join("events.jsonl");
        let event = json!({
            "schema": "ee.test_event.v1",
            "kind": "fake_tailscale_harness_rust_helper",
            "phase": phase,
            "valid": valid,
            "detail": detail,
            "workspace_id": "fake-workspace",
            "request_id": "fake-tailscale-rust-helper",
            "bead_id": "bd-36bbk.1.10",
            "surface": "fake_tailscale_e2e_harness",
            "elapsed_ms": 0_u64,
            "artifactHash": "",
        });
        let mut handle = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&event_path)
            .map_err(|error| format!("open {}: {error}", event_path.display()))?;
        writeln!(
            handle,
            "{}",
            serde_json::to_string(&event).map_err(|error| error.to_string())?
        )
        .map_err(|error| format!("write event: {error}"))
    }
}

#[must_use]
pub fn deterministic_node_key(seed: &str) -> String {
    let digest = blake3::hash(seed.as_bytes());
    format!("nodekey:{}", &digest.to_hex()[..32])
}
