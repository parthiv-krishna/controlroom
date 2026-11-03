//! The shared todo list (spec §8): edited by the agent (through its extension) and the user
//! (`todo.*`), every change attributed to its author.
//!
//! - Each change upserts the turn's `todo` timeline item, a snapshot of the list: one item per
//!   turn (`todo-<last turn end seq>`), so a run's edits read as one snapshot.
//! - User edits reach the agent as a short notice, a non-waking injection delivered into its
//!   next turn; consecutive edits extend the notice while it waits.
//! - Closing an entry (done, blocked, skipped) counts as auto-continue progress.

use controlroom_protocol::RpcError;
use controlroom_protocol::daemon::{
    TodoAddParams, TodoRemoveParams, TodoReorderParams, TodoUpdateParams,
};
use controlroom_protocol::ids::{ItemId, SessionId, TodoId};
use controlroom_protocol::session::{
    Author, ContentPart, SessionState, Todo, TodoRollup, TodoStatus,
};
use controlroom_protocol::timeline::{InjectOrigin, ItemBody};
use controlroom_protocol::{Resource, UnixMillis};

use crate::auto;
use crate::core::Core;
use crate::errors::{invalid_params, invalid_state, not_found};
use crate::model::{Injection, SessionData};

/// Longest todo text.
const MAX_TEXT: usize = 2000;
/// First line of a todo notice.
const NOTICE_HEAD: &str = "[controlroom] The user changed your todo list:";

/// One edit of the list.
pub(crate) enum Edit {
    Add {
        texts: Vec<String>,
        index: Option<u32>,
    },
    Update {
        todo_id: TodoId,
        text: Option<String>,
        status: Option<TodoStatus>,
        reason: Option<String>,
    },
    Remove {
        todo_id: TodoId,
    },
    Reorder {
        todo_id: TodoId,
        to_index: u32,
    },
}

impl Edit {
    pub(crate) fn add(p: &TodoAddParams) -> Self {
        Edit::Add {
            texts: p.texts.clone(),
            index: p.index,
        }
    }

    pub(crate) fn update(p: &TodoUpdateParams) -> Self {
        Edit::Update {
            todo_id: p.todo_id,
            text: p.text.clone(),
            status: p.status,
            reason: p.reason.clone(),
        }
    }

    pub(crate) fn remove(p: &TodoRemoveParams) -> Self {
        Edit::Remove { todo_id: p.todo_id }
    }

    pub(crate) fn reorder(p: &TodoReorderParams) -> Self {
        Edit::Reorder {
            todo_id: p.todo_id,
            to_index: p.to_index,
        }
    }
}

impl Core {
    /// Applies an edit by `author`; returns the entries it added or changed.
    pub(crate) fn edit_todos(
        &mut self,
        id: SessionId,
        author: Author,
        edit: Edit,
    ) -> Result<Vec<Todo>, RpcError> {
        let now = self.clock.now();
        let session = self.session_mut(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "editing the todo list"));
        }
        let data = &mut session.data;
        let (changed, note) = apply(data, &author, edit, now)?;
        let by_user = matches!(author, Author::User { .. });
        if by_user && let Some(note) = note {
            self.notify_agent(id, &note, now)?;
        }
        self.snapshot_todos(id)?;
        Ok(changed)
    }

    /// Adds `note` to the waiting todo notice, or queues a new one.
    fn notify_agent(&mut self, id: SessionId, note: &str, now: UnixMillis) -> Result<(), RpcError> {
        let data = &mut self.session_mut(id)?.data;
        let waiting = data
            .injections
            .iter_mut()
            .find(|i| i.origin == InjectOrigin::Todo && i.delivery.is_none());
        if let Some(Injection { content, .. }) = waiting
            && let Some(ContentPart::Text { text }) = content.first_mut()
        {
            text.push_str("\n- ");
            text.push_str(note);
            return Ok(());
        }
        let text = format!("{NOTICE_HEAD}\n- {note}");
        let injection = Injection::new(InjectOrigin::Todo, "todo edits".into(), text, false, now);
        self.inject(id, injection)
    }

    /// Upserts the turn's todo snapshot item.
    fn snapshot_todos(&mut self, id: SessionId) -> Result<(), RpcError> {
        let data = &self.session(id)?.data;
        let item = ItemId::new(format!("todo-{}", data.last_turn_end_seq.0));
        let body = ItemBody::Todo {
            items: data.todos.clone(),
        };
        self.put_item(id, item, body)
    }
}

/// Applies `edit`; returns the changed entries and, for the agent, a one-line description.
fn apply(
    data: &mut SessionData,
    author: &Author,
    edit: Edit,
    now: UnixMillis,
) -> Result<(Vec<Todo>, Option<String>), RpcError> {
    let todos = &mut data.todos;
    let stamp = |todo: &mut Todo| {
        todo.updated_by = author.clone();
        todo.updated_at = now;
    };
    match edit {
        Edit::Add { texts, index } => {
            let texts: Vec<String> = texts.iter().map(|t| t.trim().to_owned()).collect();
            if texts.is_empty() || texts.iter().any(|t| t.is_empty()) {
                return Err(invalid_params("todo texts must not be empty"));
            }
            check_len(&texts)?;
            let new: Vec<Todo> = texts
                .into_iter()
                .map(|text| Todo {
                    id: TodoId::generate(),
                    text,
                    status: TodoStatus::Pending,
                    reason: None,
                    updated_by: author.clone(),
                    updated_at: now,
                })
                .collect();
            let at = index.map_or(todos.len(), |i| clamp(i, todos.len()));
            todos.splice(at..at, new.iter().cloned());
            let note = new
                .iter()
                .map(|t| format!("added \"{}\"", t.text))
                .collect::<Vec<_>>()
                .join("; ");
            Ok((new, Some(note)))
        }
        Edit::Update {
            todo_id,
            text,
            status,
            reason,
        } => {
            let pos = position(todos, todo_id)?;
            let todo = &mut todos[pos];
            let mut notes = Vec::new();
            if let Some(text) = text {
                let text = text.trim().to_owned();
                if text.is_empty() {
                    return Err(invalid_params("todo text must not be empty"));
                }
                check_len(std::slice::from_ref(&text))?;
                if text != todo.text {
                    notes.push(format!("renamed \"{}\" to \"{text}\"", todo.text));
                    todo.text = text;
                }
            }
            if let Some(status) = status {
                if status == TodoStatus::Unknown {
                    return Err(invalid_params("unknown todo status"));
                }
                if status != todo.status {
                    notes.push(format!("marked \"{}\" {}", todo.text, status_word(status)));
                    todo.status = status;
                    auto::todo_status_changed(&mut data.auto, status);
                }
            }
            if let Some(reason) = reason {
                let reason = Some(reason.trim().to_owned()).filter(|r| !r.is_empty());
                if reason != todo.reason {
                    if let Some(r) = &reason {
                        notes.push(format!("noted on \"{}\": {r}", todo.text));
                    }
                    todo.reason = reason;
                }
            }
            if !matches!(todo.status, TodoStatus::Blocked | TodoStatus::Skipped) {
                todo.reason = None;
            }
            stamp(todo);
            let note = (!notes.is_empty()).then(|| notes.join("; "));
            Ok((vec![todo.clone()], note))
        }
        Edit::Remove { todo_id } => {
            let pos = position(todos, todo_id)?;
            let removed = todos.remove(pos);
            Ok((Vec::new(), Some(format!("removed \"{}\"", removed.text))))
        }
        Edit::Reorder { todo_id, to_index } => {
            let pos = position(todos, todo_id)?;
            let mut todo = todos.remove(pos);
            stamp(&mut todo);
            let to = clamp(to_index, todos.len());
            todos.insert(to, todo.clone());
            let note = (to != pos).then(|| format!("moved \"{}\" to #{}", todo.text, to + 1));
            Ok((vec![todo], note))
        }
    }
}

fn position(todos: &[Todo], id: TodoId) -> Result<usize, RpcError> {
    todos
        .iter()
        .position(|t| t.id == id)
        .ok_or_else(|| not_found(Resource::Todo, id))
}

fn clamp(index: u32, len: usize) -> usize {
    usize::try_from(index).unwrap_or(usize::MAX).min(len)
}

fn check_len(texts: &[String]) -> Result<(), RpcError> {
    if texts.iter().any(|t| t.len() > MAX_TEXT) {
        return Err(invalid_params(format!(
            "todo texts are limited to {MAX_TEXT} bytes"
        )));
    }
    Ok(())
}

fn status_word(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in progress",
        TodoStatus::Done => "done",
        TodoStatus::Blocked => "blocked",
        TodoStatus::Skipped => "skipped",
        TodoStatus::Unknown => "unknown",
    }
}

/// The roll-up for the summary; `None` while the list is empty.
pub(crate) fn rollup(data: &SessionData) -> Option<TodoRollup> {
    if data.todos.is_empty() {
        return None;
    }
    let counted = data.todos.iter().filter(|t| t.status != TodoStatus::Skipped);
    let (done, total) = counted.fold((0u32, 0u32), |(done, total), t| {
        (done + u32::from(t.status == TodoStatus::Done), total + 1)
    });
    Some(TodoRollup {
        done,
        total,
        auto_continue: data.auto.status(),
    })
}

/// The list as text for the agent: `- [status] text (id)`.
pub(crate) fn render_list(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "The todo list is empty.".into();
    }
    let mut out = String::from("Todo list:");
    for (n, t) in todos.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. [{}] {} (id {})",
            n + 1,
            status_word(t.status),
            t.text,
            t.id
        ));
        if let Some(reason) = &t.reason {
            out.push_str(&format!(" ({reason})"));
        }
    }
    out
}

impl crate::runtime::Runtime {
    /// `todo.add`, by the user.
    pub async fn todo_add(
        &self,
        params: TodoAddParams,
    ) -> Result<controlroom_protocol::daemon::TodoAddResult, RpcError> {
        self.command("todo.add", &params.op_id, &params, |core| {
            let todos = core.edit_todos(params.id, user(&params.by), Edit::add(&params))?;
            Ok(controlroom_protocol::daemon::TodoAddResult { todos })
        })
        .await
    }

    /// `todo.update`, by the user.
    pub async fn todo_update(&self, params: TodoUpdateParams) -> Result<Todo, RpcError> {
        self.command("todo.update", &params.op_id, &params, |core| {
            let changed = core.edit_todos(params.id, user(&params.by), Edit::update(&params))?;
            changed
                .into_iter()
                .next()
                .ok_or_else(|| not_found(Resource::Todo, params.todo_id))
        })
        .await
    }

    /// `todo.remove`, by the user.
    pub async fn todo_remove(&self, params: TodoRemoveParams) -> Result<(), RpcError> {
        self.command("todo.remove", &params.op_id, &params, |core| {
            core.edit_todos(params.id, user(&params.by), Edit::remove(&params))
                .map(drop)
        })
        .await
    }

    /// `todo.reorder`, by the user.
    pub async fn todo_reorder(&self, params: TodoReorderParams) -> Result<(), RpcError> {
        self.command("todo.reorder", &params.op_id, &params, |core| {
            core.edit_todos(params.id, user(&params.by), Edit::reorder(&params))
                .map(drop)
        })
        .await
    }
}

fn user(by: &Option<controlroom_protocol::session::DeviceRef>) -> Author {
    Author::User { device: by.clone() }
}
