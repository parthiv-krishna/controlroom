//! Shared agent state and the helpers every task uses: emitting records,
//! persisting messages, dialogs, gates and side steps.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};

use crate::gates::Gates;
use crate::ids::uuid;
use crate::out::Out;
use crate::scenario::{DialogStep, ModelSpec, Reply, Scenario, Step, UiStep};
use crate::session::Session;
use crate::wire;

/// Queue delivery modes (`set_steering_mode`, `set_follow_up_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    All,
    OneAtATime,
}

impl QueueMode {
    pub fn parse(mode: &str) -> Option<Self> {
        match mode {
            "all" => Some(Self::All),
            "one-at-a-time" => Some(Self::OneAtATime),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::OneAtATime => "one-at-a-time",
        }
    }
}

/// Control of the active run.
pub struct RunControl {
    pub abort: watch::Sender<bool>,
    pub done: watch::Receiver<bool>,
}

/// Mutable state, behind one lock that is never held across an await.
pub struct State {
    pub session: Session,
    pub model: usize,
    pub thinking_level: String,
    pub replies: VecDeque<Reply>,
    pub steering: VecDeque<String>,
    pub follow_up: VecDeque<String>,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    pub auto_compaction: bool,
    pub auto_retry: bool,
    pub compacting: bool,
    pub run: Option<RunControl>,
    pub dialogs: HashMap<String, oneshot::Sender<Value>>,
}

/// The agent: configuration plus shared state.
pub struct Agent {
    pub scenario: Scenario,
    pub gates: Option<Gates>,
    /// The project's sessions directory; `None` with `--no-session`.
    pub sessions_dir: Option<PathBuf>,
    pub cwd: String,
    out: Out,
    state: Mutex<State>,
}

/// The outcome of waiting on something a run can abort.
pub enum Waited<T> {
    Done(T),
    Aborted,
}

impl Agent {
    pub fn new(
        scenario: Scenario,
        gates: Option<Gates>,
        sessions_dir: Option<PathBuf>,
        cwd: String,
        session: Session,
        out: Out,
    ) -> Arc<Self> {
        let model = scenario
            .models
            .iter()
            .position(|m| Some(m.id.as_str()) == session.latest("model_change", "modelId"))
            .unwrap_or(0);
        let thinking_level = session
            .latest("thinking_level_change", "thinkingLevel")
            .unwrap_or(&scenario.thinking_level)
            .to_owned();
        let state = State {
            session,
            model,
            thinking_level,
            replies: scenario.replies.iter().cloned().collect(),
            steering: VecDeque::new(),
            follow_up: VecDeque::new(),
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            auto_compaction: false,
            auto_retry: true,
            compacting: false,
            run: None,
            dialogs: HashMap::new(),
        };
        Arc::new(Self {
            scenario,
            gates,
            sessions_dir,
            cwd,
            out,
            state: Mutex::new(state),
        })
    }

    /// Lock the state (never across an await).
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Write one record to stdout.
    pub fn emit(&self, record: &Value) {
        self.out.emit(record);
    }

    /// The current model.
    pub fn model(&self) -> ModelSpec {
        let index = self.state().model;
        self.scenario.models[index].clone()
    }

    /// The next scripted model reply.
    pub fn next_reply(&self) -> Reply {
        self.state().replies.pop_front().unwrap_or_else(|| {
            self.scenario
                .default_reply
                .clone()
                .unwrap_or_else(Reply::ok)
        })
    }

    /// Append a session entry; on a write failure report it on stderr, as
    /// pi does, and carry on.
    pub fn persist(&self, entry: Value) -> Value {
        match self.state().session.append(entry.clone()) {
            Ok(entry) => entry,
            Err(err) => {
                eprintln!("fake-agent: persisting a session entry failed: {err:#}");
                entry
            }
        }
    }

    /// Persist a message; returns its entry id.
    pub fn persist_message(&self, message: &Value) -> String {
        let entry = self.persist(json!({"type": "message", "message": message}));
        entry["id"].as_str().unwrap_or_default().to_owned()
    }

    /// `message_start` + `message_end` for a complete message, persisted.
    pub fn whole_message(&self, message: &Value) -> String {
        self.emit(&wire::message_event("message_start", message));
        self.emit(&wire::message_event("message_end", message));
        self.persist_message(message)
    }

    /// The current `queue_update`.
    pub fn queue_update(&self) -> Value {
        let state = self.state();
        json!({
            "type": "queue_update",
            "steering": state.steering,
            "followUp": state.follow_up,
        })
    }

    /// Pass a gate; an abort ends the wait early.
    pub async fn gate(&self, name: &str, abort: &mut watch::Receiver<bool>) -> Waited<()> {
        let Some(gates) = &self.gates else {
            eprintln!("fake-agent: gate {name} reached but no gate dir is configured");
            std::process::exit(2);
        };
        abortable(abort, async {
            if let Err(err) = gates.pass(name).await {
                eprintln!("fake-agent: gate {name}: {err:#}");
            }
        })
        .await
    }

    /// Run a side step (anything but model content). Returns `Aborted` if
    /// an abortable wait was interrupted.
    pub async fn side_step(&self, step: &Step, abort: &mut watch::Receiver<bool>) -> Waited<()> {
        match step {
            Step::Gate { name } => return self.gate(name, abort).await,
            Step::Sleep { ms } => {
                return abortable(abort, tokio::time::sleep(Duration::from_millis(*ms))).await;
            }
            Step::Ui(ui) => self.ui(ui),
            Step::Dialog(dialog) => {
                self.dialog(dialog).await;
            }
            Step::AppendEntry { custom_type, data } => self.append_entry(custom_type, data),
            Step::Crash { code } => std::process::exit(*code),
            Step::Text(_) | Step::Thinking(_) | Step::ToolCall(_) => {
                eprintln!("fake-agent: ignoring a content step outside a reply");
            }
        }
        Waited::Done(())
    }

    /// Run the leading steps that never wait (UI requests, entries) right
    /// away; returns how many ran.
    pub fn instant_steps(&self, steps: &[Step]) -> usize {
        let mut ran = 0;
        for step in steps {
            match step {
                Step::Ui(ui) => self.ui(ui),
                Step::AppendEntry { custom_type, data } => self.append_entry(custom_type, data),
                _ => break,
            }
            ran += 1;
        }
        ran
    }

    /// Run side steps in order (session start, commands).
    pub async fn side_steps(&self, steps: &[Step]) {
        let (_keep, mut never) = watch::channel(false);
        for step in steps {
            self.side_step(step, &mut never).await;
        }
    }

    /// `pi.appendEntry`: a persisted `custom` entry plus `entry_appended`.
    pub fn append_entry(&self, custom_type: &str, data: &Value) {
        let entry =
            self.persist(json!({"type": "custom", "customType": custom_type, "data": data}));
        self.emit(&json!({"type": "entry_appended", "entry": entry}));
    }

    /// A fire-and-forget extension UI request.
    pub fn ui(&self, step: &UiStep) {
        let (method, fields) = match step {
            UiStep::Notify {
                message,
                notify_type,
            } => (
                "notify",
                json!({"message": message, "notifyType": notify_type}),
            ),
            UiStep::SetStatus { key, text } => {
                ("setStatus", json!({"statusKey": key, "statusText": text}))
            }
            UiStep::SetWidget { key, lines } => {
                ("setWidget", json!({"widgetKey": key, "widgetLines": lines}))
            }
            UiStep::SetTitle { title } => ("setTitle", json!({"title": title})),
        };
        let fields = drop_nulls(fields);
        self.emit(&wire::ui_request(&uuid(), method, fields));
    }

    /// Show a dialog and wait for its answer (`None` after its timeout).
    /// pi's dialogs ignore aborts, so this wait does too.
    pub async fn dialog(&self, dialog: &DialogStep) -> Option<Value> {
        let id = uuid();
        let mut fields = json!({"title": dialog.title});
        match dialog.method {
            crate::scenario::DialogMethod::Select => fields["options"] = json!(dialog.options),
            crate::scenario::DialogMethod::Confirm => fields["message"] = json!(dialog.message),
            crate::scenario::DialogMethod::Input => {
                fields["placeholder"] = json!(dialog.placeholder);
            }
            crate::scenario::DialogMethod::Editor => fields["prefill"] = json!(dialog.prefill),
        }
        if let Some(timeout) = dialog.timeout_ms {
            fields["timeout"] = timeout.into();
        }
        let (tx, rx) = oneshot::channel();
        self.state().dialogs.insert(id.clone(), tx);
        self.emit(&wire::ui_request(
            &id,
            dialog.method.as_str(),
            drop_nulls(fields),
        ));
        let answer = match dialog.timeout_ms {
            Some(ms) => tokio::time::timeout(Duration::from_millis(ms), rx)
                .await
                .ok()
                .and_then(Result::ok),
            None => rx.await.ok(),
        };
        self.state().dialogs.remove(&id);
        answer
    }
}

/// Await `fut` unless the run is (or gets) aborted.
pub async fn abortable<T>(
    abort: &mut watch::Receiver<bool>,
    fut: impl Future<Output = T>,
) -> Waited<T> {
    if *abort.borrow() {
        return Waited::Aborted;
    }
    tokio::select! {
        value = fut => Waited::Done(value),
        _ = abort.wait_for(|aborted| *aborted) => Waited::Aborted,
    }
}

fn drop_nulls(mut value: Value) -> Value {
    if let Value::Object(map) = &mut value {
        map.retain(|_, v| !v.is_null());
    }
    value
}
