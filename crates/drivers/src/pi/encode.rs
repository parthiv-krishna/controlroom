//! Intents as pi RPC lines, each tagged in its `id` (or `crTag`, see the `tag` module).

use controlroom_protocol::session::ContentPart;
use serde_json::{Map, Value, json};

use super::content::PiPrompt;
use super::{PiDriver, dialogs, tag};
use crate::{EncodeError, Intent, LineTag};

impl PiDriver {
    pub(super) fn encode_intent(
        &self,
        intent: &Intent,
        tag: &LineTag,
    ) -> Result<Vec<String>, EncodeError> {
        let run_active = self.view.run_active;
        let commands = match intent {
            Intent::Prompt { content } => {
                if run_active {
                    return Err(EncodeError::InvalidState("a run is active".into()));
                }
                vec![user_command("prompt", content)?]
            }
            Intent::Steer { content } | Intent::FollowUp { content } => {
                // An idle agent gets a `Prompt`, so the runtime knows a run is about to start.
                if !run_active {
                    return Err(EncodeError::InvalidState("no run is active".into()));
                }
                // A `prompt` with a `streamingBehavior` queues into the run, and starts the
                // next run if this one settles before pi reads the line, where a plain
                // `steer`/`follow_up` would wait in pi's queue for the next prompt (fixtures
                // README obs. 8).
                let behavior = if matches!(intent, Intent::Steer { .. }) {
                    "steer"
                } else {
                    "followUp"
                };
                let mut command = user_command("prompt", content)?;
                command["streamingBehavior"] = json!(behavior);
                vec![command]
            }
            Intent::Inject { content, wake, .. } => {
                // While a run is active it steers into it. A waking item goes as a `prompt`
                // with `streamingBehavior: "steer"`, like `Steer`; a non-waking one as a plain
                // `steer`, which waits in pi's queue for the next prompt if the run settles
                // before pi reads the line.
                let command = match (run_active, wake) {
                    (false, _) => user_command("prompt", content)?,
                    (true, false) => user_command("steer", content)?,
                    (true, true) => {
                        let mut command = user_command("prompt", content)?;
                        command["streamingBehavior"] = json!("steer");
                        command
                    }
                };
                vec![command]
            }
            Intent::Cancel => self.cancel(),
            Intent::Answer { request, response } => {
                let open = self
                    .process
                    .dialogs
                    .iter()
                    .find(|open| open.request == *request)
                    .ok_or_else(|| {
                        EncodeError::Invalid(format!("request {request} is not open"))
                    })?;
                let fields = dialogs::response_fields(&open.dialog, response)?;
                vec![ui_response(&open.dialog.pi_id, fields)]
            }
            Intent::SetOption { id, value } => vec![self.options.command(id, value)?],
            Intent::NewSession { parent } => {
                if run_active {
                    return Err(EncodeError::InvalidState("a run is active".into()));
                }
                let mut command = json!({"type": "new_session"});
                if let Some(parent) = parent {
                    command["parentSession"] = json!(self.session_file(parent));
                }
                vec![command]
            }
            Intent::Refresh => vec![
                json!({"type": "get_state"}),
                json!({"type": "get_available_models"}),
                json!({"type": "get_available_thinking_levels"}),
            ],
        };
        commands
            .into_iter()
            .enumerate()
            .map(|(part, command)| {
                let part = u16::try_from(part)
                    .map_err(|_| EncodeError::Unsupported("too many lines".into()))?;
                Ok(tagged(command, &tag::encode(tag, part)))
            })
            .collect()
    }

    /// pi links a parent session by its file: the file of the current native session when
    /// `parent` is its id, else `parent` as given (already a path).
    fn session_file<'a>(&'a self, parent: &'a str) -> &'a str {
        match &self.view.native {
            Some(native) if native.id == parent => native.path.as_deref().unwrap_or(parent),
            _ => parent,
        }
    }

    /// Cancels every open dialog, then aborts. pi's `abort` waits until the dialogs are
    /// answered (obs. 2), so answering first can't hang even if pi read lines one at a time.
    fn cancel(&self) -> Vec<Value> {
        let mut commands: Vec<Value> = self
            .process
            .dialogs
            .iter()
            .map(|open| {
                let mut fields = Map::new();
                fields.insert("cancelled".into(), Value::Bool(true));
                ui_response(&open.dialog.pi_id, fields)
            })
            .collect();
        commands.push(json!({"type": "abort"}));
        commands
    }
}

/// A `prompt`, `steer` or `follow_up` command.
fn user_command(kind: &str, content: &[ContentPart]) -> Result<Value, EncodeError> {
    let prompt = PiPrompt::from_parts(content);
    if prompt.is_empty() {
        return Err(EncodeError::Invalid("empty content".into()));
    }
    let mut command = json!({"type": kind, "message": prompt.message});
    if !prompt.images.is_empty() {
        command["images"] = Value::Array(prompt.images);
    }
    Ok(command)
}

/// An `extension_ui_response` for pi's dialog `pi_id`; the tag goes into `crTag`.
fn ui_response(pi_id: &str, fields: Map<String, Value>) -> Value {
    let mut command = Map::new();
    command.insert("type".into(), json!("extension_ui_response"));
    command.insert("id".into(), json!(pi_id));
    command.extend(fields);
    Value::Object(command)
}

/// Serializes a command with its tag: in `crTag` when the `id` is pi's, else in `id`.
fn tagged(mut command: Value, tag: &str) -> String {
    if let Value::Object(map) = &mut command {
        let field = if map.contains_key("id") {
            "crTag"
        } else {
            "id"
        };
        map.insert(field.into(), json!(tag));
    }
    command.to_string()
}
