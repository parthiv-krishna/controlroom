//! The agent API's side of the runtime: what the session's own extension (`@controlroom/pi`)
//! may do, through the daemon's per-session agent socket. Every mutating call is deduplicated
//! by the pi tool call id (`call_id`), so a call retried after a reconnect applies once.

use controlroom_protocol::RpcError;
use controlroom_protocol::agent::{
    AgentState, AgentTodoAddParams, AgentTodoUpdateParams, AgentTodos, NotifyParams,
    QuestionAskParams, QuestionAskResult, QuestionWithdrawParams, QuestionWithdrawResult,
};
use controlroom_protocol::ids::{ItemId, OpId, SessionId};
use controlroom_protocol::session::{Author, SessionState};
use controlroom_protocol::timeline::ItemBody;
use tokio::sync::watch;

use crate::core::Core;
use crate::errors::{invalid_params, invalid_state};
use crate::runtime::Runtime;
use crate::todos::Edit;

/// Longest notice text.
const MAX_NOTICE: usize = 4000;

/// The op id of an agent call: scoped to the session, so call ids can't collide across agents.
pub(crate) fn op_id(id: SessionId, call_id: &str) -> OpId {
    OpId::new(format!("agent:{id}:{call_id}"))
}

impl Runtime {
    /// The session's agent state now, and its changes.
    pub async fn agent_state(
        &self,
        id: SessionId,
    ) -> Result<watch::Receiver<Option<AgentState>>, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| {
            let session = core.session(id)?;
            let rx = session.agent.subscribe();
            if rx.borrow().is_none() {
                session.agent.send_replace(Some(session.agent_state()));
            }
            Ok(rx)
        })
    }

    /// `question.ask`.
    pub async fn agent_ask(
        &self,
        id: SessionId,
        params: QuestionAskParams,
    ) -> Result<QuestionAskResult, RpcError> {
        let op = op_id(id, &params.call_id);
        self.command("agent.question.ask", &op, &params, |core| {
            core.ask(id, &params)
        })
        .await
    }

    /// `question.withdraw`.
    pub async fn agent_withdraw(
        &self,
        id: SessionId,
        params: QuestionWithdrawParams,
    ) -> Result<QuestionWithdrawResult, RpcError> {
        let op = op_id(id, &params.call_id);
        self.command("agent.question.withdraw", &op, &params, |core| {
            core.withdraw(id, &params)
        })
        .await
    }

    /// `todo.list`.
    pub async fn agent_todos(&self, id: SessionId) -> Result<AgentTodos, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| {
            Ok(AgentTodos {
                todos: core.session(id)?.data.todos.clone(),
            })
        })
    }

    /// The agent's `todo.add`.
    pub async fn agent_todo_add(
        &self,
        id: SessionId,
        params: AgentTodoAddParams,
    ) -> Result<AgentTodos, RpcError> {
        let op = op_id(id, &params.call_id);
        let edit = || Edit::Add {
            texts: params.texts.clone(),
            index: None,
        };
        self.command("agent.todo.add", &op, &params, |core| {
            core.agent_edit(id, edit())
        })
        .await
    }

    /// The agent's `todo.update`.
    pub async fn agent_todo_update(
        &self,
        id: SessionId,
        params: AgentTodoUpdateParams,
    ) -> Result<AgentTodos, RpcError> {
        let op = op_id(id, &params.call_id);
        let edit = Edit::Update {
            todo_id: params.todo_id,
            text: params.text.clone(),
            status: params.status,
            reason: params.reason.clone(),
        };
        self.command("agent.todo.update", &op, &params, |core| {
            core.agent_edit(id, edit)
        })
        .await
    }

    /// `notify`: a notice in the session's timeline.
    pub async fn agent_notify(&self, id: SessionId, params: NotifyParams) -> Result<(), RpcError> {
        let op = op_id(id, &params.call_id);
        self.command("agent.notify", &op, &params, |core| {
            core.agent_notice(id, &params)
        })
        .await
    }
}

impl Core {
    fn agent_edit(&mut self, id: SessionId, edit: Edit) -> Result<AgentTodos, RpcError> {
        self.edit_todos(id, Author::Agent, edit)?;
        self.reconcile(id);
        Ok(AgentTodos {
            todos: self.session(id)?.data.todos.clone(),
        })
    }

    fn agent_notice(&mut self, id: SessionId, params: &NotifyParams) -> Result<(), RpcError> {
        let text = params.text.trim();
        if text.is_empty() || text.len() > MAX_NOTICE {
            return Err(invalid_params(format!(
                "a notice has 1 to {MAX_NOTICE} bytes"
            )));
        }
        if self.session(id)?.data.closed() {
            return Err(invalid_state(SessionState::Closed, "notify"));
        }
        let item = ItemId::new(format!("notice-{}", ulid::Ulid::generate()));
        let body = ItemBody::Notice {
            level: params.level,
            text: text.to_owned(),
        };
        self.put_item(id, item, body)
    }
}
