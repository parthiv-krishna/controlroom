//! Sessions as the UI sees them: created, prompted and watched through the UI API.

use std::collections::BTreeMap;

use controlroom_controller::test_support::op;
use serde_json::{Value, json};

use crate::world::World;

/// A session's timeline as one subscription mirrors it (items by id).
pub struct Timeline {
    /// The subscription's id.
    pub subscription: Value,
    /// Items by id.
    pub items: BTreeMap<String, Value>,
}

impl Timeline {
    fn apply(&mut self, ops: &Value) {
        for op in ops.as_array().into_iter().flatten() {
            match op["op"].as_str() {
                Some("upsert") => {
                    let item = op["item"].clone();
                    self.items
                        .insert(item["id"].as_str().unwrap().to_owned(), item);
                }
                Some("append_text") => {
                    let Some(item) = self.items.get_mut(op["id"].as_str().unwrap()) else {
                        continue;
                    };
                    let text = op["text"].as_str().unwrap_or_default();
                    let target = match item["kind"].as_str() {
                        Some("tool_call") => &mut item["output"]["text"],
                        Some("thinking") => &mut item["text"],
                        _ => continue,
                    };
                    *target = json!(format!("{}{text}", target.as_str().unwrap_or_default()));
                }
                _ => {}
            }
        }
    }

    /// Items of `kind`, in timeline order.
    pub fn of_kind(&self, kind: &str) -> Vec<&Value> {
        let mut items: Vec<&Value> = self.items.values().filter(|i| i["kind"] == kind).collect();
        items.sort_by_key(|i| i["seq_from"].as_u64());
        items
    }

    /// The tool call `id`, if it is in the timeline.
    pub fn tool(&self, id: &str) -> Option<&Value> {
        self.items
            .values()
            .find(|i| i["kind"] == "tool_call" && i["tool_call_id"] == id)
    }

    /// Whether tool call `id` has ended (a result or an error).
    pub fn tool_done(&self, id: &str) -> bool {
        self.tool(id)
            .is_some_and(|t| matches!(t["status"].as_str(), Some("completed" | "failed")))
    }

    /// The ended tool call `id`'s output text parsed as JSON (the agent API's result, or its
    /// error object when the tool failed).
    pub fn tool_json(&self, id: &str) -> Value {
        let tool = self.tool(id).unwrap_or_else(|| panic!("no tool call {id}"));
        let text = tool["output"]["text"].as_str().unwrap_or_default();
        serde_json::from_str(text).unwrap_or_else(|_| panic!("tool {id}: not JSON: {tool}"))
    }

    /// Whether tool call `id` ended as an error.
    pub fn tool_failed(&self, id: &str) -> bool {
        self.tool(id).is_some_and(|t| t["status"] == "failed")
    }

    /// The runs started by an injection of `origin` (e.g. `job`, `tool_late`).
    pub fn injected_runs(&self, origin: &str) -> Vec<&Value> {
        self.of_kind("run")
            .into_iter()
            .filter(|r| r["trigger"]["kind"] == "inject" && r["trigger"]["origin"] == origin)
            .collect()
    }

    /// The texts of user messages (prompts and injections as the agent got them).
    pub fn user_texts(&self) -> Vec<String> {
        self.of_kind("message")
            .into_iter()
            .filter(|m| m["role"] == "user")
            .map(message_text)
            .collect()
    }

    /// The texts of notices (messages with role `notice` and `notice` items).
    pub fn notices(&self) -> Vec<String> {
        self.items
            .values()
            .filter(|i| i["kind"] == "notice" || (i["kind"] == "message" && i["role"] == "notice"))
            .map(|i| {
                i["text"]
                    .as_str()
                    .map_or_else(|| message_text(i), str::to_owned)
            })
            .collect()
    }
}

/// A message item's text parts, joined.
fn message_text(message: &Value) -> String {
    message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

impl World {
    /// Creates a session (sandbox off: the gate has no nono) on `workspaces`
    /// (`(machine id, path)`, the first is primary), with `extra` create params; returns its
    /// id once it is idle.
    pub async fn create_session(&mut self, workspaces: &[(&str, &str)], extra: Value) -> String {
        let id = self
            .ui
            .ok("session.create", self.create_params(workspaces, extra))
            .await["session"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        self.wait_session(&id, "idle", |s| {
            s["state"] == "idle" && s["queue"]["len"] == 0
        })
        .await;
        id
    }

    /// The `session.create` params for [`World::create_session`].
    pub fn create_params(&self, workspaces: &[(&str, &str)], extra: Value) -> Value {
        let workspaces: Vec<Value> = workspaces
            .iter()
            .map(|(machine, path)| json!({"machine_id": machine, "path": path}))
            .collect();
        let mut params = json!({"op_id": op(), "workspaces": workspaces, "sandbox_level": "off"});
        for (k, v) in extra.as_object().into_iter().flatten() {
            params[k] = v.clone();
        }
        params
    }

    /// The session's summary in the state feed (`null` if absent).
    pub fn session(&self, id: &str) -> Value {
        self.ui.mirror.session(id)
    }

    /// Waits until the session's summary satisfies `pred`; returns it.
    pub async fn wait_session(
        &mut self,
        id: &str,
        what: &str,
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        self.ui
            .wait(&format!("session {what}"), |m| pred(&m.session(id)))
            .await;
        self.session(id)
    }

    /// Waits until the session shows its workspace on `machine` as connected or not: the agent
    /// daemon has seen the machine's link change.
    pub async fn wait_availability(&mut self, id: &str, machine: &str, connected: bool) {
        let what = if connected {
            "reachable"
        } else {
            "unreachable"
        };
        self.wait_session(id, &format!("sees {machine} {what}"), |s| {
            s["workspaces"].as_array().into_iter().flatten().any(|w| {
                w["machine_id"] == machine
                    && (w["availability"]["connectivity"]["state"] == "connected") == connected
            })
        })
        .await;
    }

    /// Sends a prompt and waits until the run it starts (and anything it triggers) ended and
    /// the session is idle again.
    pub async fn prompt(&mut self, id: &str, text: &str) {
        let mark = self.turn_mark(id);
        self.send_prompt(id, text).await;
        self.wait_turn(id, mark).await;
    }

    /// Sends a prompt without waiting for the run.
    pub async fn send_prompt(&mut self, id: &str, text: &str) {
        self.ui
            .ok(
                "session.prompt",
                json!({"op_id": op(), "id": id,
                       "content": [{"kind": "text", "text": text}]}),
            )
            .await;
    }

    /// The session's `last_turn_end_seq` as the feed shows it.
    pub fn turn_mark(&self, id: &str) -> u64 {
        self.session(id)["last_turn_end_seq"].as_u64().unwrap_or(0)
    }

    /// Waits until a run ended after `mark` and the session is idle with an empty queue.
    pub async fn wait_turn(&mut self, id: &str, mark: u64) -> Value {
        self.wait_session(id, "a turn end", |s| {
            s["state"] == "idle"
                && s["queue"]["len"] == 0
                && s["last_turn_end_seq"].as_u64() > Some(mark)
        })
        .await
    }

    /// Subscribes to the session's timeline from the start.
    pub async fn timeline(&mut self, id: &str) -> Timeline {
        let result = self
            .ui
            .ok(
                "session.subscribe",
                json!({"id": id, "since_seq": 0, "timeline": true}),
            )
            .await;
        let mut timeline = Timeline {
            subscription: result["subscription_id"].clone(),
            items: BTreeMap::new(),
        };
        timeline.apply(&result["ops"]);
        self.drain(&mut timeline);
        timeline
    }

    /// Applies the timeline's queued `session.events`.
    fn drain(&mut self, timeline: &mut Timeline) {
        let sub = timeline.subscription.clone();
        let (mine, rest): (Vec<Value>, Vec<Value>) = std::mem::take(&mut self.ui.notifications)
            .into_iter()
            .partition(|n| {
                n["method"] == "session.events" && n["params"]["subscription_id"] == sub
            });
        self.ui.notifications = rest;
        for events in mine {
            timeline.apply(&events["params"]["ops"]);
        }
    }

    /// Waits until the timeline satisfies `pred`.
    pub async fn until(
        &mut self,
        timeline: &mut Timeline,
        what: &str,
        pred: impl Fn(&Timeline) -> bool,
    ) {
        loop {
            self.drain(timeline);
            if pred(timeline) {
                return;
            }
            let sub = timeline.subscription.clone();
            let events = self
                .ui
                .notification(what, |n| {
                    n["method"] == "session.events" && n["params"]["subscription_id"] == sub
                })
                .await;
            timeline.apply(&events["params"]["ops"]);
        }
    }

    /// Ends a timeline subscription.
    pub async fn unsubscribe(&mut self, timeline: Timeline) {
        self.ui
            .ok(
                "unsubscribe",
                json!({"subscription_id": timeline.subscription}),
            )
            .await;
    }
}
