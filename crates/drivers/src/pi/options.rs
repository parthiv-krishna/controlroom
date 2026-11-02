//! pi's model and thinking level as neutral select options.

use controlroom_protocol::daemon::OptionValue;
use controlroom_protocol::session::{OptionChoice, OptionControl, SessionOption};
use serde_json::{Value, json};

use super::wire::Model;
use crate::EncodeError;

/// The model option id; values are `<provider>/<model id>`.
pub(crate) const MODEL: &str = "model";
/// The thinking level option id; values are pi's level names.
pub(crate) const THINKING: &str = "thinking";

/// What the fold knows about pi's model and thinking settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Options {
    /// The current model.
    pub model: Option<Model>,
    /// Models pi offers (`get_available_models`).
    pub models: Vec<Model>,
    /// The current thinking level.
    pub thinking: Option<String>,
    /// Levels pi offers for the current model (`get_available_thinking_levels`).
    pub levels: Vec<String>,
}

impl Options {
    /// The neutral options: each one only once its current value is known.
    pub(crate) fn neutral(&self) -> Vec<SessionOption> {
        let mut options = Vec::new();
        if let Some(current) = &self.model {
            let mut choices: Vec<OptionChoice> = self.models.iter().map(model_choice).collect();
            if !self.models.contains(current) {
                choices.insert(0, model_choice(current));
            }
            options.push(SessionOption {
                id: MODEL.into(),
                label: "Model".into(),
                control: OptionControl::Select {
                    value: model_value(current),
                    choices,
                },
            });
        }
        if let Some(level) = &self.thinking {
            let mut levels = self.levels.clone();
            if !levels.contains(level) {
                levels.insert(0, level.clone());
            }
            options.push(SessionOption {
                id: THINKING.into(),
                label: "Thinking".into(),
                control: OptionControl::Select {
                    value: level.clone(),
                    choices: levels
                        .into_iter()
                        .map(|level| OptionChoice {
                            label: level.clone(),
                            value: level,
                        })
                        .collect(),
                },
            });
        }
        options
    }

    /// The context window of a model by provider and id, else of the current model.
    pub(crate) fn context_window(&self, provider: Option<&str>, id: Option<&str>) -> Option<u64> {
        let named = self
            .models
            .iter()
            .chain(&self.model)
            .find(|m| Some(m.provider.as_str()) == provider && Some(m.id.as_str()) == id);
        named.or(self.model.as_ref())?.context_window
    }

    /// The pi command setting an option (without `id`).
    pub(crate) fn command(&self, id: &str, value: &OptionValue) -> Result<Value, EncodeError> {
        let OptionValue::Choice(value) = value else {
            return Err(EncodeError::Invalid(format!("{id} takes a choice")));
        };
        let allowed = self
            .neutral()
            .into_iter()
            .find(|o| o.id == id)
            .and_then(|o| match o.control {
                OptionControl::Select { choices, .. } => Some(choices),
                _ => None,
            });
        let Some(choices) = allowed else {
            return Err(EncodeError::Invalid(format!("unknown option {id:?}")));
        };
        if !choices.iter().any(|choice| choice.value == *value) {
            return Err(EncodeError::Invalid(format!(
                "{value:?} is not a choice of {id}"
            )));
        }
        Ok(match id {
            MODEL => {
                let (provider, model_id) = value
                    .split_once('/')
                    .ok_or_else(|| EncodeError::Invalid(format!("bad model {value:?}")))?;
                json!({"type": "set_model", "provider": provider, "modelId": model_id})
            }
            _ => json!({"type": "set_thinking_level", "level": value}),
        })
    }
}

/// A model's option value.
fn model_value(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

/// A model's display label (its name, else its id).
pub(crate) fn model_label(model: &Model) -> String {
    model.name.clone().unwrap_or_else(|| model.id.clone())
}

fn model_choice(model: &Model) -> OptionChoice {
    OptionChoice {
        value: model_value(model),
        label: model_label(model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> Model {
        Model {
            id: id.into(),
            name: Some(format!("Model {id}")),
            provider: "mock".into(),
            context_window: Some(1000),
        }
    }

    #[test]
    fn options_appear_once_known_and_include_the_current_value() {
        let mut options = Options::default();
        assert!(options.neutral().is_empty());
        options.model = Some(model("a"));
        options.thinking = Some("high".into());
        options.levels = vec!["off".into(), "low".into()];
        let neutral = options.neutral();
        assert_eq!(neutral.len(), 2);
        let OptionControl::Select { value, choices } = &neutral[0].control else {
            panic!("not a select");
        };
        assert_eq!(value, "mock/a");
        assert_eq!(choices.len(), 1);
        let OptionControl::Select { choices, .. } = &neutral[1].control else {
            panic!("not a select");
        };
        let values: Vec<&str> = choices.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, ["high", "off", "low"]);
    }

    #[test]
    fn commands_validate_choices() {
        let options = Options {
            model: Some(model("a")),
            models: vec![model("a"), model("b/c")],
            thinking: Some("off".into()),
            levels: vec!["off".into(), "high".into()],
        };
        let choice = |v: &str| OptionValue::Choice(v.into());
        assert_eq!(
            options.command(MODEL, &choice("mock/b/c")).unwrap(),
            json!({"type": "set_model", "provider": "mock", "modelId": "b/c"})
        );
        assert_eq!(
            options.command(THINKING, &choice("high")).unwrap(),
            json!({"type": "set_thinking_level", "level": "high"})
        );
        assert!(options.command(MODEL, &choice("mock/zz")).is_err());
        assert!(options.command(THINKING, &OptionValue::Bool(true)).is_err());
        assert!(options.command("speed", &choice("x")).is_err());
        assert_eq!(
            options.context_window(Some("mock"), Some("b/c")),
            Some(1000)
        );
    }
}
