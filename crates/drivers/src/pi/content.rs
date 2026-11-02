//! Neutral [`ContentPart`]s to and from pi's message text and images.

use controlroom_protocol::session::ContentPart;
use serde_json::{Value, json};

use super::wire::{Part, UserContent};

/// pi's `message` text and `images` for a prompt, steer or follow-up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PiPrompt {
    /// The text: text parts and resource references, one per line.
    pub message: String,
    /// `ImageContent` objects.
    pub images: Vec<Value>,
}

impl PiPrompt {
    /// Converts neutral content. pi has no resource parts, so a resource becomes its URI on
    /// its own line; unknown parts are dropped.
    pub(crate) fn from_parts(parts: &[ContentPart]) -> Self {
        let mut lines: Vec<&str> = Vec::new();
        let mut images = Vec::new();
        for part in parts {
            match part {
                ContentPart::Text { text } => lines.push(text),
                ContentPart::Resource { uri, .. } => lines.push(uri),
                ContentPart::Image { mime_type, data } => {
                    images.push(json!({"type": "image", "data": data, "mimeType": mime_type}));
                }
                ContentPart::Unknown => {}
            }
        }
        Self {
            message: lines.join("\n"),
            images,
        }
    }

    /// Whether there is nothing to send.
    pub(crate) fn is_empty(&self) -> bool {
        self.message.is_empty() && self.images.is_empty()
    }
}

/// The neutral content of a line written to pi (`message` plus `images`).
pub(crate) fn from_line(message: &str, images: &[Part]) -> Vec<ContentPart> {
    let mut content = Vec::new();
    if !message.is_empty() {
        content.push(ContentPart::text(message));
    }
    content.extend(images.iter().filter_map(image_part));
    content
}

/// The neutral content of a user or custom message pi reported.
pub(crate) fn from_user(content: &UserContent) -> Vec<ContentPart> {
    match content {
        UserContent::Text(text) => vec![ContentPart::text(text.as_str())],
        UserContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(ContentPart::text(text.as_str())),
                image => image_part(image),
            })
            .collect(),
    }
}

/// The text of a user message, for matching it with the line that sent it.
pub(crate) fn text_of(content: &[ContentPart]) -> String {
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    texts.join("\n")
}

fn image_part(part: &Part) -> Option<ContentPart> {
    match part {
        Part::Image { data, mime_type } => Some(ContentPart::Image {
            mime_type: mime_type.clone(),
            data: data.clone(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_parts_round_trip_through_a_line() {
        let parts = vec![
            ContentPart::text("Look at this"),
            ContentPart::Resource {
                uri: "file:///home/alice/project/notes.txt".into(),
                name: None,
                mime_type: None,
            },
            ContentPart::Image {
                mime_type: "image/png".into(),
                data: "aGk=".into(),
            },
            ContentPart::Unknown,
        ];
        let prompt = PiPrompt::from_parts(&parts);
        assert_eq!(
            prompt.message,
            "Look at this\nfile:///home/alice/project/notes.txt"
        );
        assert_eq!(
            prompt.images,
            vec![json!({"type":"image","data":"aGk=","mimeType":"image/png"})]
        );
        let images: Vec<Part> = serde_json::from_value(Value::Array(prompt.images)).unwrap();
        assert_eq!(
            from_line(&prompt.message, &images),
            vec![
                ContentPart::text("Look at this\nfile:///home/alice/project/notes.txt"),
                ContentPart::Image {
                    mime_type: "image/png".into(),
                    data: "aGk=".into()
                },
            ]
        );
        assert!(PiPrompt::from_parts(&[ContentPart::Unknown]).is_empty());
    }
}
