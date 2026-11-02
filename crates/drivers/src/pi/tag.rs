//! The compact, reversible encoding of a [`LineTag`] in pi's RPC `id` field.
//!
//! `cr:<kind>:<part>:<key>` with kind `op`, `q` or `in`. The part comes before the key so a key
//! may contain colons (op ids such as `sched:1700000000000`, internal keys such as
//! `continue:run-5`). Ids without the `cr:` prefix belong to someone else (a debug client, the
//! fixture recorder, or pi's own dialog ids) and decode to `None`.

use controlroom_protocol::ids::{OpId, QueueId};

use crate::LineTag;

const PREFIX: &str = "cr:";

/// Encodes a tag with the given part.
pub(crate) fn encode(tag: &LineTag, part: u16) -> String {
    let (kind, key) = match tag {
        LineTag::Op { op_id, .. } => ("op", op_id.as_str().to_owned()),
        LineTag::Queue { queue_id, .. } => ("q", queue_id.to_string()),
        LineTag::Internal { key, .. } => ("in", key.clone()),
    };
    format!("{PREFIX}{kind}:{part}:{key}")
}

/// Decodes an id written by [`encode`]; `None` for anything else.
pub(crate) fn decode(id: &str) -> Option<LineTag> {
    let rest = id.strip_prefix(PREFIX)?;
    let mut fields = rest.splitn(3, ':');
    let kind = fields.next()?;
    let part: u16 = fields.next()?.parse().ok()?;
    let key = fields.next()?;
    if key.is_empty() {
        return None;
    }
    match kind {
        "op" => Some(LineTag::Op {
            op_id: OpId::new(key),
            part,
        }),
        "q" => Some(LineTag::Queue {
            queue_id: key.parse::<QueueId>().ok()?,
            part,
        }),
        "in" => Some(LineTag::Internal {
            key: key.to_owned(),
            part,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_kind_with_colons_in_keys() {
        let queue_id = QueueId::generate();
        for tag in [
            LineTag::Op {
                op_id: OpId::new("sched:1700000000000"),
                part: 0,
            },
            LineTag::Queue { queue_id, part: 3 },
            LineTag::Internal {
                key: "continue:run-5".into(),
                part: 65535,
            },
        ] {
            let (LineTag::Op { part, .. }
            | LineTag::Queue { part, .. }
            | LineTag::Internal { part, .. }) = tag;
            let encoded = encode(&tag, part);
            assert!(encoded.starts_with("cr:"), "{encoded}");
            assert_eq!(decode(&encoded), Some(tag));
        }
        assert_eq!(
            encode(&LineTag::Queue { queue_id, part: 1 }, 1),
            format!("cr:q:1:{queue_id}")
        );
    }

    #[test]
    fn foreign_and_malformed_ids_decode_to_none() {
        for id in [
            "1",
            "00000000-0000-4000-8000-000000000002",
            "cr:",
            "cr:op",
            "cr:op:x:key",
            "cr:op:0:",
            "cr:op:70000:key",
            "cr:zz:0:key",
            "cr:q:0:not-a-ulid",
        ] {
            assert_eq!(decode(id), None, "{id}");
        }
    }
}
