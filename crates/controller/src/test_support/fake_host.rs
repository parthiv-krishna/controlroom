//! An in-process fake host daemon (host API) behind a [`RemoteHost`]: it listens on the
//! daemon socket under the remote `$HOME`, so the installed `controlroomd connect` bridges the
//! controller's SSH exec channel to it.
//!
//! It keeps a journal by call id like the real host: `tool.run` runs a call once (a repeat
//! attaches to the running call or returns the journaled result), counts executions, and holds
//! `bash` commands that start with `hold` until the test releases them. A [`FakeHost::restart`]
//! takes a new epoch and drops every connection; calls that were running are then `lost`.
//! Other methods are recorded and answered with `{}` (`fs.list` with an empty listing,
//! `doctor.report` with `unsupported`).
//!
//! This stands in for the real host daemon until R1a merges; the socket path is the
//! per-machine daemon's (`~/.local/state/controlroom/run/daemon.sock`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use controlroom_protocol::host::notifications::ToolOutput;
use controlroom_protocol::host::{HostHelloResult, ToolOutputParams};
use controlroom_protocol::ids::{CallId, DaemonId, Epoch};
use controlroom_protocol::version::Capability;
use controlroom_protocol::{ErrorData, HOST_PROTOCOL_VERSION, RpcError};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio::sync::watch;

use super::wire::{Peer, serve};
use super::{DAEMON_VERSION, RemoteHost, wait_until};

struct State {
    epoch: Epoch,
    daemon_id: DaemonId,
    peers: Vec<Peer>,
    hellos: u32,
    received: Vec<(String, Value)>,
    /// Calls started in this epoch.
    started: HashSet<String>,
    /// Calls that were running when the daemon restarted.
    lost: HashSet<String>,
    executions: HashMap<String, u32>,
    journal: HashMap<String, Value>,
    gates: HashMap<String, watch::Sender<bool>>,
}

/// The fake host daemon. Dropping it closes its connections and stops listening.
pub struct FakeHost {
    state: Arc<Mutex<State>>,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.accept.abort();
        for peer in self.state.lock().unwrap().peers.drain(..) {
            peer.close();
        }
    }
}

impl FakeHost {
    /// Listens on `host`'s daemon socket.
    pub fn start(host: &RemoteHost) -> Self {
        let run = host.home().join(".local/state/controlroom/run");
        std::fs::create_dir_all(&run).unwrap();
        let listener = UnixListener::bind(run.join("daemon.sock")).unwrap();
        let state = Arc::new(Mutex::new(State {
            epoch: Epoch::generate(),
            daemon_id: DaemonId::generate(),
            peers: Vec::new(),
            hellos: 0,
            received: Vec::new(),
            started: HashSet::new(),
            lost: HashSet::new(),
            executions: HashMap::new(),
            journal: HashMap::new(),
            gates: HashMap::new(),
        }));
        let accept = tokio::spawn({
            let state = state.clone();
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let st = state.clone();
                    let peer = serve(stream, move |_, method, params| {
                        let st = st.clone();
                        async move { handle(&st, &method, params).await }
                    });
                    state.lock().unwrap().peers.push(peer);
                }
            }
        });
        Self { state, accept }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// The current epoch.
    pub fn epoch(&self) -> Epoch {
        self.lock().epoch
    }

    /// How many `hello`s it answered.
    pub fn hellos(&self) -> u32 {
        self.lock().hellos
    }

    /// Waits until it answered `n` `hello`s.
    pub async fn wait_hellos(&self, n: u32) {
        wait_until(&format!("{n} host hellos"), || self.hellos() >= n).await;
    }

    /// How often call `id` actually ran.
    pub fn executions(&self, id: &str) -> u32 {
        self.lock().executions.get(id).copied().unwrap_or(0)
    }

    /// Waits until call `id` is running (or ran).
    pub async fn wait_started(&self, id: &str) {
        wait_until(&format!("call {id} starts"), || self.executions(id) > 0).await;
    }

    /// Every request of `method` received, with its params.
    pub fn received(&self, method: &str) -> Vec<Value> {
        self.lock()
            .received
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// Lets held call `id` finish.
    pub fn release(&self, id: &str) {
        // `send_replace`: the value must stick even while no call waits on it.
        self.gate(id).send_replace(true);
    }

    fn gate(&self, id: &str) -> watch::Sender<bool> {
        gate(&mut self.lock(), id)
    }

    /// Sends `tool.output` for a call on every connection.
    pub fn output(&self, call_id: &str, text: &str) {
        for peer in &self.lock().peers {
            peer.notify::<ToolOutput>(ToolOutputParams {
                call_id: CallId::new(call_id),
                text: text.to_owned(),
            });
        }
    }

    /// Drops every connection (as a lost link would).
    pub fn drop_connections(&self) {
        for peer in self.lock().peers.drain(..) {
            peer.close();
        }
    }

    /// A daemon restart: a new epoch, every connection dropped, running calls lost.
    pub fn restart(&self) {
        let mut st = self.lock();
        st.epoch = Epoch::generate();
        let started: Vec<String> = st.started.drain().collect();
        let running: Vec<String> = started
            .into_iter()
            .filter(|id| !st.journal.contains_key(id))
            .collect();
        st.lost.extend(running);
        for peer in st.peers.drain(..) {
            peer.close();
        }
    }
}

fn gate(st: &mut State, id: &str) -> watch::Sender<bool> {
    st.gates
        .entry(id.to_owned())
        .or_insert_with(|| watch::channel(false).0)
        .clone()
}

async fn handle(state: &Mutex<State>, method: &str, params: Value) -> Result<Value, RpcError> {
    if method == "tool.run" {
        state
            .lock()
            .unwrap()
            .received
            .push((method.to_owned(), params.clone()));
        let id = params["call_id"].as_str().unwrap_or_default().to_owned();
        let held = params["op"]["command"]
            .as_str()
            .is_some_and(|c| c.starts_with("hold"));
        return run(state, id, params["op"]["op"].clone(), held).await;
    }
    let mut st = state.lock().unwrap();
    st.received.push((method.to_owned(), params.clone()));
    match method {
        "hello" => {
            st.hellos += 1;
            Ok(serde_json::to_value(HostHelloResult {
                host_version: DAEMON_VERSION.to_owned(),
                protocol_version: HOST_PROTOCOL_VERSION,
                daemon_id: st.daemon_id,
                epoch: st.epoch,
                capabilities: vec![Capability::SHELLS, Capability::JOBS],
            })
            .unwrap())
        }
        "tool.attach" => {
            let id = params["call_id"].as_str().unwrap_or_default();
            st.journal.get(id).cloned().ok_or_else(|| {
                RpcError::new(
                    ErrorData::NotFound {
                        resource: controlroom_protocol::error::Resource::Call,
                        id: id.to_owned(),
                    },
                    "no such call",
                )
            })
        }
        "fs.list" => Ok(json!({"path": params["path"], "entries": []})),
        "doctor.report" => Err(RpcError::new(ErrorData::Unsupported, "fake host")),
        _ => Ok(json!({})),
    }
}

/// `tool.run`: once per call id; a repeat attaches or returns the journaled result.
async fn run(state: &Mutex<State>, id: String, op: Value, held: bool) -> Result<Value, RpcError> {
    let mut gate = {
        let mut st = state.lock().unwrap();
        if let Some(done) = st.journal.get(&id) {
            return Ok(done.clone());
        }
        if st.lost.contains(&id) {
            return Err(RpcError::new(
                ErrorData::Lost { may_have_run: true },
                "the host daemon restarted while this call ran",
            ));
        }
        if st.started.insert(id.clone()) {
            *st.executions.entry(id.clone()).or_default() += 1;
        }
        held.then(|| gate(&mut st, &id).subscribe())
    };
    if let Some(gate) = &mut gate {
        let _ = gate.wait_for(|open| *open).await;
    }
    let mut st = state.lock().unwrap();
    if st.lost.contains(&id) {
        return Err(RpcError::new(
            ErrorData::Lost { may_have_run: true },
            "the host daemon restarted while this call ran",
        ));
    }
    let result =
        json!({"op": op, "call_id": id, "ran": st.executions.get(&id).copied().unwrap_or(0)});
    Ok(st.journal.entry(id).or_insert(result).clone())
}
