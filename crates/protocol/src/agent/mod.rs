//! The agent API: `@controlroom/pi` (the extension inside the agent) ↔ `controlroomd`.
//!
//! Newline-delimited JSON-RPC 2.0 over a **per-session unix socket**, `<state>/run/<id>.agent.sock`.
//! The daemon serves one socket per session, and the session's sandbox profile grants only that
//! socket, so an agent can only reach its own session. The extension finds the socket and a
//! per-launch token in its environment (`CONTROLROOM_AGENT_SOCKET`, `CONTROLROOM_AGENT_TOKEN`,
//! from the launch env file) and authenticates with the token in `hello`.
//!
//! **Connection lifecycle:**
//! 1. The extension calls `hello {protocol_version, token}`. A wrong token is
//!    `unauthenticated`, a major mismatch `incompatible_version`, any other method first
//!    `hello_required`.
//! 2. Right after the `hello` result the daemon sends a `state` notification, and another one
//!    whenever the todo list, the open questions or auto-continue change. The extension keeps
//!    the latest one; it never needs to poll.
//! 3. Mutating calls carry the pi tool call's id as `call_id`, an idempotency key: a call
//!    retried after a reconnect returns the original result instead of applying twice.
//!
//! Tools only cover what the agent initiates (spec §9). Answers to questions, user edits of the
//! todo list and auto-continue reach the agent as injected prompts through the agent's own RPC,
//! never through this socket.
//!
//! **Why notifications on the socket** rather than `/controlroom <verb>` commands written to
//! pi's stdin: commands on stdin are transcript lines that need a ready agent, follow the
//! runtime's in-doubt rules and make pi wait for the handler before it answers; a state
//! snapshot on the extension's own connection needs none of that, and a reconnect simply gets
//! the current state again.

mod types;

pub use types::*;

use crate::jsonrpc::Ack;

/// Marker types for every agent API method; see [`crate::jsonrpc::Method`].
pub mod methods {
    use super::*;

    rpc_methods! {
        /// A typed agent API request.
        pub enum AgentRequest;

        /// Handshake with the session's token; must be the first call.
        Hello = "hello" (AgentHelloParams) -> AgentHelloResult;
        /// Ask the user non-blocking questions (`ask_user`).
        QuestionAsk = "question.ask" (QuestionAskParams) -> QuestionAskResult;
        /// Withdraw an open question.
        QuestionWithdraw = "question.withdraw" (QuestionWithdrawParams) -> QuestionWithdrawResult;
        /// The todo list.
        TodoList = "todo.list" (crate::jsonrpc::NoParams) -> AgentTodos;
        /// Add todo entries.
        TodoAdd = "todo.add" (AgentTodoAddParams) -> AgentTodos;
        /// Change a todo entry (complete, block, rename, ...).
        TodoUpdate = "todo.update" (AgentTodoUpdateParams) -> AgentTodos;
        /// Tell the user something without asking (`notify_user`).
        Notify = "notify" (NotifyParams) -> Ack;
    }
}

/// Marker types for every agent API notification; see [`crate::jsonrpc::NotificationMethod`].
pub mod notifications {
    use super::*;

    rpc_notifications! {
        /// A typed daemon → extension notification.
        pub enum AgentNotification;

        /// The session's todo list, open questions and auto-continue, sent after `hello` and
        /// on every change.
        State = "state" (AgentState);
    }
}

pub use methods::AgentRequest;
pub use notifications::AgentNotification;
