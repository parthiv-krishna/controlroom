//! pi's blocking extension dialogs (`select`, `confirm`, `input`, `editor`) as neutral
//! `dialog` requests, and neutral responses as `extension_ui_response` fields.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::RequestId;
use controlroom_protocol::session::{
    OptionStyle, Question, QuestionAnswer, Request, RequestAnswer, RequestKind, RequestOption,
    RequestState, Response,
};
use serde_json::{Map, Value};

use super::wire::{InLine, UiRequest};
use crate::EncodeError;

const YES: &str = "yes";
const NO: &str = "no";
const CANCELLED: &str = "Cancelled";

/// Which dialog pi opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogKind {
    Select,
    Confirm,
    Input,
    Editor,
}

impl DialogKind {
    /// The dialog kind of an `extension_ui_request` method; `None` for fire-and-forget methods.
    pub(crate) fn of(method: &str) -> Option<Self> {
        match method {
            "select" => Some(Self::Select),
            "confirm" => Some(Self::Confirm),
            "input" => Some(Self::Input),
            "editor" => Some(Self::Editor),
            _ => None,
        }
    }

    /// The single question's id for text dialogs.
    fn question_id(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            _ => "input",
        }
    }
}

/// An open dialog: what the fold remembers to encode and record answers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dialog {
    /// pi's dialog id, echoed in `extension_ui_response`.
    pub pi_id: String,
    pub kind: DialogKind,
    /// The select options (option ids are their indexes).
    pub options: Vec<String>,
    /// The record ts after which pi gave up waiting, if the dialog has a timeout.
    pub deadline: Option<i64>,
    /// Whether it was raised during a run (such dialogs are moot once the run settles).
    pub in_run: bool,
}

/// The neutral request for a dialog.
pub(crate) fn request(id: RequestId, kind: DialogKind, ui: &UiRequest, at: UnixMillis) -> Request {
    let title = ui.title.clone().unwrap_or_default();
    let (detail, options, questions) = match kind {
        DialogKind::Select => (
            None,
            ui.options
                .iter()
                .enumerate()
                .map(|(index, label)| neutral_option(index.to_string(), label))
                .collect(),
            Vec::new(),
        ),
        DialogKind::Confirm => (
            ui.message.clone(),
            vec![
                neutral_option(YES.into(), "Yes"),
                neutral_option(NO.into(), "No"),
            ],
            Vec::new(),
        ),
        DialogKind::Input | DialogKind::Editor => {
            let hint = if kind == DialogKind::Input {
                &ui.placeholder
            } else {
                &ui.prefill
            };
            (
                hint.clone().filter(|h| !h.is_empty()),
                Vec::new(),
                vec![Question {
                    id: kind.question_id().into(),
                    text: title.clone(),
                    options: Vec::new(),
                    multi: false,
                    free_text: true,
                }],
            )
        }
    };
    Request {
        id,
        kind: RequestKind::Dialog,
        blocking: true,
        state: RequestState::Pending,
        title,
        detail,
        body: None,
        options,
        questions,
        answer: None,
        origin_child: None,
        created_at: at,
    }
}

fn neutral_option(id: String, label: &str) -> RequestOption {
    RequestOption {
        id,
        label: label.to_owned(),
        style: OptionStyle::Neutral,
    }
}

/// The `extension_ui_response` fields (besides `type`, `id` and the tag) answering a dialog.
pub(crate) fn response_fields(
    dialog: &Dialog,
    response: &Response,
) -> Result<Map<String, Value>, EncodeError> {
    let mut fields = Map::new();
    let invalid = || EncodeError::Invalid(format!("{response:?} does not answer this dialog"));
    match (dialog.kind, response) {
        (_, Response::Dismiss) => {
            fields.insert("cancelled".into(), Value::Bool(true));
        }
        (DialogKind::Select, Response::Choice { option_id, .. }) => {
            let index: usize = option_id.parse().map_err(|_| invalid())?;
            let value = dialog.options.get(index).ok_or_else(invalid)?;
            fields.insert("value".into(), Value::String(value.clone()));
        }
        (DialogKind::Confirm, Response::Choice { option_id, .. }) => {
            let confirmed = match option_id.as_str() {
                YES => true,
                NO => false,
                _ => return Err(invalid()),
            };
            fields.insert("confirmed".into(), Value::Bool(confirmed));
        }
        (DialogKind::Input | DialogKind::Editor, Response::Answers { answers }) => {
            let answer = answers.first().ok_or_else(invalid)?;
            let text = answer
                .text
                .clone()
                .or_else(|| answer.selected.first().cloned())
                .unwrap_or_default();
            fields.insert("value".into(), Value::String(text));
        }
        _ => return Err(invalid()),
    }
    Ok(fields)
}

/// The recorded answer of an `extension_ui_response` line (the inverse of
/// [`response_fields`]).
pub(crate) fn answer(dialog: &Dialog, line: &InLine, at: UnixMillis) -> RequestAnswer {
    let (response, label) = if line.cancelled == Some(true) {
        (Response::Dismiss, CANCELLED.to_owned())
    } else {
        match dialog.kind {
            DialogKind::Select => {
                let value = line.value.clone().unwrap_or_default();
                let option_id = dialog
                    .options
                    .iter()
                    .position(|option| *option == value)
                    .map_or_else(|| value.clone(), |index| index.to_string());
                (
                    Response::Choice {
                        option_id,
                        reason: None,
                    },
                    value,
                )
            }
            DialogKind::Confirm => {
                let confirmed = line.confirmed.unwrap_or(false);
                let (id, label) = if confirmed { (YES, "Yes") } else { (NO, "No") };
                (
                    Response::Choice {
                        option_id: id.into(),
                        reason: None,
                    },
                    label.into(),
                )
            }
            DialogKind::Input | DialogKind::Editor => {
                let text = line.value.clone().unwrap_or_default();
                (
                    Response::Answers {
                        answers: vec![QuestionAnswer {
                            question_id: dialog.kind.question_id().into(),
                            selected: Vec::new(),
                            text: Some(text.clone()),
                        }],
                    },
                    text,
                )
            }
        }
    };
    RequestAnswer {
        response,
        label,
        by: None,
        at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(kind: DialogKind, options: &[&str]) -> Dialog {
        Dialog {
            pi_id: "d1".into(),
            kind,
            options: options.iter().map(|o| o.to_string()).collect(),
            deadline: None,
            in_run: false,
        }
    }

    fn line(json: &str) -> InLine {
        serde_json::from_str(json).unwrap()
    }

    fn choice(id: &str) -> Response {
        Response::Choice {
            option_id: id.into(),
            reason: None,
        }
    }

    #[test]
    fn responses_encode_and_decode_symmetrically() {
        let select = dialog(DialogKind::Select, &["red", "green"]);
        let fields = response_fields(&select, &choice("1")).unwrap();
        assert_eq!(Value::Object(fields), serde_json::json!({"value": "green"}));
        let answered = answer(&select, &line(r#"{"value":"green"}"#), UnixMillis(5));
        assert_eq!(answered.response, choice("1"));
        assert_eq!(answered.label, "green");

        let confirm = dialog(DialogKind::Confirm, &[]);
        let fields = response_fields(&confirm, &choice(NO)).unwrap();
        assert_eq!(
            Value::Object(fields),
            serde_json::json!({"confirmed": false})
        );
        assert_eq!(
            answer(&confirm, &line(r#"{"confirmed":true}"#), UnixMillis(1)).label,
            "Yes"
        );

        let input = dialog(DialogKind::Input, &[]);
        let answers = Response::Answers {
            answers: vec![QuestionAnswer {
                question_id: "input".into(),
                selected: vec![],
                text: Some("alice".into()),
            }],
        };
        let fields = response_fields(&input, &answers).unwrap();
        assert_eq!(Value::Object(fields), serde_json::json!({"value": "alice"}));
        assert_eq!(
            answer(&input, &line(r#"{"value":"alice"}"#), UnixMillis(1)).response,
            answers
        );

        let fields = response_fields(&input, &Response::Dismiss).unwrap();
        assert_eq!(
            Value::Object(fields),
            serde_json::json!({"cancelled": true})
        );
        let cancelled = answer(&input, &line(r#"{"cancelled":true}"#), UnixMillis(1));
        assert_eq!(cancelled.response, Response::Dismiss);
    }

    #[test]
    fn mismatched_responses_are_invalid() {
        let select = dialog(DialogKind::Select, &["a"]);
        assert!(response_fields(&select, &choice("1")).is_err());
        assert!(response_fields(&select, &choice("x")).is_err());
        assert!(response_fields(&dialog(DialogKind::Confirm, &[]), &choice("maybe")).is_err());
        let empty = Response::Answers { answers: vec![] };
        assert!(response_fields(&dialog(DialogKind::Editor, &[]), &empty).is_err());
        assert!(response_fields(&select, &Response::Unknown).is_err());
    }
}
