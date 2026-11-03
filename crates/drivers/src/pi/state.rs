//! The fold's bookkeeping besides the timeline items.

use std::collections::BTreeMap;

use controlroom_protocol::ids::RequestId;
use controlroom_protocol::session::ContentPart;
use controlroom_protocol::timeline::RunTrigger;
use controlroom_protocol::{Seq, UnixMillis};
use serde_json::Value;

use super::dialogs::Dialog;
use super::items::At;
use crate::LineTag;

/// State that lives as long as one pi process; reset when it exits.
#[derive(Debug, Clone, Default)]
pub(crate) struct Process {
    /// A `meta` record (or any record) started it and no `exit` record ended it yet.
    pub alive: bool,
    /// Lines written to pi that await a response, by their RPC id.
    pub sent: BTreeMap<String, Sent>,
    /// Accepted prompts, steers and follow-ups whose user message pi hasn't echoed yet.
    pub echoes: Vec<Echo>,
    /// The line whose `prompt` response said `started`, for the next `agent_start`.
    pub cause: Option<Cause>,
    /// The active run (`agent_start` to `agent_settled`).
    pub run: Option<Run>,
    /// The assistant message being streamed.
    pub message: Option<Streamed>,
    /// Tool calls by pi's call id, from their first block or execution event to their end.
    pub tools: BTreeMap<String, Tool>,
    /// Open dialogs, in the order they were raised.
    pub dialogs: Vec<OpenDialog>,
    /// The open retry item.
    pub retry: Option<String>,
    /// The open compaction item.
    pub compaction: Option<String>,
    /// `abort` lines written and not answered yet.
    pub aborts: usize,
}

/// A line written to pi, awaiting its response.
#[derive(Debug, Clone)]
pub(crate) struct Sent {
    /// The record that wrote it.
    pub at: At,
    pub command: SentCommand,
}

/// What a sent line was, as far as its response matters.
#[derive(Debug, Clone)]
pub(crate) enum SentCommand {
    /// `prompt`, `steer` or `follow_up`: the user message shown once pi accepts it.
    User {
        content: Vec<ContentPart>,
        /// A `prompt` written while no run was active: it starts one unless pi refuses it
        /// (or an extension command handles it).
        starts_run: bool,
    },
    /// `abort`.
    Abort,
    /// The RPC `bash` command and its tool item.
    Bash { item: String },
    /// Anything else.
    Other,
}

/// An accepted user line waiting for pi to deliver it.
#[derive(Debug, Clone)]
pub(crate) struct Echo {
    /// Its user message item.
    pub item: String,
    /// When it was written.
    pub ts: UnixMillis,
    /// Its content.
    pub content: Vec<ContentPart>,
    /// Its text, compared with pi's user message.
    pub text: String,
    /// It started a run, so its echo is the run's first user message even if pi rewrote it
    /// (e.g. a prompt template).
    pub started: bool,
}

/// What starts the next run: the line whose response said `started`. If it is tagged, its
/// `Accepted` waits for `agent_start` (the tag stays unanswered until then).
#[derive(Debug, Clone)]
pub(crate) struct Cause {
    /// The tag of the line, when it carried one.
    pub tag: Option<LineTag>,
    /// The run item's trigger.
    pub trigger: RunTrigger,
}

/// The active run.
#[derive(Debug, Clone)]
pub(crate) struct Run {
    /// Its timeline item.
    pub item: String,
    /// The last assistant message's `stopReason` and `errorMessage`.
    pub last_stop: Option<(String, Option<String>)>,
    /// `auto_retry_end.finalError`, when retries ran out.
    pub retry_error: Option<String>,
}

/// The assistant message being streamed: its start record and blocks by content index.
#[derive(Debug, Clone)]
pub(crate) struct Streamed {
    pub seq: Seq,
    pub blocks: BTreeMap<usize, StreamedBlock>,
}

/// One streamed content block.
#[derive(Debug, Clone)]
pub(crate) enum StreamedBlock {
    /// A text or thinking item.
    Text { item: String },
    /// A tool call (see [`Process::tools`]).
    Tool { call_id: String },
}

/// A tool call the fold tracks.
#[derive(Debug, Clone)]
pub(crate) struct Tool {
    pub item: String,
    pub name: String,
    pub args: Value,
    /// Between `tool_execution_start` and `tool_execution_end`.
    pub running: bool,
}

/// An open dialog request.
#[derive(Debug, Clone)]
pub(crate) struct OpenDialog {
    pub request: RequestId,
    pub dialog: Dialog,
}
