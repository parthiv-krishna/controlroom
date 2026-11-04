//! The framed terminal channel: `controlroomd pty attach <shell id> --framed` on its own SSH exec
//! channel, one per attached terminal (spec §22). Bulk bytes never ride the host API.
//!
//! Both directions are a stream of frames: a kind byte, the payload length (`u32`, big-endian,
//! at most [`MAX_PAYLOAD`]) and the payload. A peer skips frames of a kind it doesn't know.
//!
//! **Controller → host** (stdin), [`ToHost`]:
//! - `input` (`0x01`): bytes typed into the terminal (any attached client may type);
//! - `resize` (`0x02`): `cols` and `rows`, each a big-endian `u16`.
//!
//! **Host → controller** (stdout), [`FromHost`]:
//! - `attached` (`0x81`, JSON [`Attached`]): first frame. Which run of the shell's process this
//!   is (`instance`, a new one after the shell was reopened) and the transcript position the
//!   replay starts at;
//! - `output` (`0x82`): the transcript position (`u64`, big-endian) of a chunk of terminal
//!   output, then its bytes. Positions increase; a chunk may split escape sequences and UTF-8
//!   characters;
//! - `foreground` (`0x83`): the name of the terminal's foreground program (UTF-8), empty at the
//!   shell's prompt; sent when it changes.
//!
//! **Replay and resume.** Without `--since-seq` the host replays recent output first. With
//! `--since-seq <seq> --instance <instance>` it continues at `seq` if the shell still runs as
//! `instance` and the gap is small; otherwise it replays recent output, and `attached` says
//! where that starts (the client then clears its screen).
//!
//! **End.** The host closes stdout when the shell's process ends and exits 0. A non-zero exit
//! means it couldn't attach (no running shell with that id; the reason is on stderr).

use serde::{Deserialize, Serialize};

/// The largest frame payload.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// The frame header: kind and payload length.
const HEADER: usize = 5;

const INPUT: u8 = 0x01;
const RESIZE: u8 = 0x02;
const ATTACHED: u8 = 0x81;
const OUTPUT: u8 = 0x82;
const FOREGROUND: u8 = 0x83;

/// A frame from the controller to the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToHost {
    /// Bytes to type into the terminal.
    Input(Vec<u8>),
    /// A new terminal size.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// A frame kind this build doesn't know (skipped).
    Unknown(u8),
}

/// A frame from the host to the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromHost {
    /// The stream started.
    Attached(Attached),
    /// Terminal output.
    Output {
        /// Its transcript position.
        seq: u64,
        /// The bytes.
        data: Vec<u8>,
    },
    /// The terminal's foreground program; `None` at the shell's prompt.
    Foreground(Option<String>),
    /// A frame kind this build doesn't know (skipped).
    Unknown(u8),
}

/// The first frame of a framed attach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    /// This run of the shell's process (its supervisor id); a reopened shell has a new one.
    pub instance: String,
    /// The transcript position the replay starts at.
    pub from_seq: u64,
}

/// A malformed frame stream.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// A frame announced more than [`MAX_PAYLOAD`] bytes.
    #[error("terminal frame of {0} bytes is too large")]
    TooLarge(usize),
    /// A known frame kind with a payload that doesn't fit it.
    #[error("malformed terminal frame of kind {0:#04x}")]
    Malformed(u8),
}

/// A frame that can be encoded and decoded.
pub trait Frame: Sized {
    /// The frame's bytes: header and payload. Panics if the payload exceeds [`MAX_PAYLOAD`]
    /// (callers chunk input and output below it).
    fn encode(&self) -> Vec<u8>;
    /// Decodes a frame of `kind` from its payload.
    fn decode(kind: u8, payload: &[u8]) -> Result<Self, FrameError>;
}

fn frame(kind: u8, parts: &[&[u8]]) -> Vec<u8> {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    assert!(len <= MAX_PAYLOAD, "terminal frame payload of {len} bytes");
    let mut out = Vec::with_capacity(HEADER + len);
    out.push(kind);
    out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

impl Frame for ToHost {
    fn encode(&self) -> Vec<u8> {
        match self {
            ToHost::Input(data) => frame(INPUT, &[data]),
            ToHost::Resize { cols, rows } => {
                frame(RESIZE, &[&cols.to_be_bytes(), &rows.to_be_bytes()])
            }
            ToHost::Unknown(kind) => frame(*kind, &[]),
        }
    }

    fn decode(kind: u8, payload: &[u8]) -> Result<Self, FrameError> {
        Ok(match kind {
            INPUT => ToHost::Input(payload.to_vec()),
            RESIZE => match payload {
                [c0, c1, r0, r1] => ToHost::Resize {
                    cols: u16::from_be_bytes([*c0, *c1]),
                    rows: u16::from_be_bytes([*r0, *r1]),
                },
                _ => return Err(FrameError::Malformed(kind)),
            },
            other => ToHost::Unknown(other),
        })
    }
}

impl Frame for FromHost {
    fn encode(&self) -> Vec<u8> {
        match self {
            FromHost::Attached(attached) => {
                let json = serde_json::to_vec(attached).unwrap_or_default();
                frame(ATTACHED, &[&json])
            }
            FromHost::Output { seq, data } => frame(OUTPUT, &[&seq.to_be_bytes(), data]),
            FromHost::Foreground(name) => {
                frame(FOREGROUND, &[name.as_deref().unwrap_or("").as_bytes()])
            }
            FromHost::Unknown(kind) => frame(*kind, &[]),
        }
    }

    fn decode(kind: u8, payload: &[u8]) -> Result<Self, FrameError> {
        Ok(match kind {
            ATTACHED => FromHost::Attached(
                serde_json::from_slice(payload).map_err(|_| FrameError::Malformed(kind))?,
            ),
            OUTPUT => {
                let (seq, data) = payload
                    .split_first_chunk::<8>()
                    .ok_or(FrameError::Malformed(kind))?;
                FromHost::Output {
                    seq: u64::from_be_bytes(*seq),
                    data: data.to_vec(),
                }
            }
            FOREGROUND => {
                let name = std::str::from_utf8(payload).map_err(|_| FrameError::Malformed(kind))?;
                FromHost::Foreground((!name.is_empty()).then(|| name.to_owned()))
            }
            other => FromHost::Unknown(other),
        })
    }
}

/// Splits a byte stream into frames.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    /// Appends bytes read from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete frame, if one is buffered.
    pub fn next_frame<F: Frame>(&mut self) -> Result<Option<F>, FrameError> {
        let Some((&kind, rest)) = self.buf.split_first() else {
            return Ok(None);
        };
        let Some(len) = rest.first_chunk::<4>() else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(*len) as usize;
        if len > MAX_PAYLOAD {
            return Err(FrameError::TooLarge(len));
        }
        if self.buf.len() < HEADER + len {
            return Ok(None);
        }
        let frame = F::decode(kind, &self.buf[HEADER..HEADER + len]);
        self.buf.drain(..HEADER + len);
        frame.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<F: Frame + PartialEq + std::fmt::Debug>(frames: &[F]) {
        let bytes: Vec<u8> = frames.iter().flat_map(Frame::encode).collect();
        // Every split point: frames come out whole and in order.
        for split in 0..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut out = Vec::new();
            for part in [&bytes[..split], &bytes[split..]] {
                decoder.push(part);
                while let Some(frame) = decoder.next_frame::<F>().unwrap() {
                    out.push(frame);
                }
            }
            assert_eq!(out, frames, "split at {split}");
        }
    }

    #[test]
    fn frames_round_trip_across_any_split() {
        round_trip(&[
            ToHost::Input(b"ls\r".to_vec()),
            ToHost::Resize { cols: 300, rows: 2 },
            ToHost::Input(Vec::new()),
        ]);
        round_trip(&[
            FromHost::Attached(Attached {
                instance: "01JAM000000000000000000001".into(),
                from_seq: 7,
            }),
            FromHost::Output {
                seq: 1 << 40,
                data: b"\x1b[2mhi\xff".to_vec(),
            },
            FromHost::Foreground(Some("vim".into())),
            FromHost::Foreground(None),
        ]);
    }

    #[test]
    fn the_wire_format_is_kind_length_payload() {
        assert_eq!(
            ToHost::Resize { cols: 80, rows: 24 }.encode(),
            [2, 0, 0, 0, 4, 0, 80, 0, 24]
        );
        assert_eq!(
            FromHost::Output {
                seq: 3,
                data: b"x".to_vec()
            }
            .encode(),
            [0x82, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 3, b'x']
        );
    }

    #[test]
    fn unknown_kinds_are_skipped_and_bad_frames_rejected() {
        let mut decoder = Decoder::default();
        decoder.push(&[0x7f, 0, 0, 0, 2, 1, 2]);
        decoder.push(&ToHost::Input(b"a".to_vec()).encode());
        assert_eq!(
            decoder.next_frame::<ToHost>(),
            Ok(Some(ToHost::Unknown(0x7f)))
        );
        assert_eq!(
            decoder.next_frame::<ToHost>(),
            Ok(Some(ToHost::Input(b"a".to_vec())))
        );
        assert_eq!(decoder.next_frame::<ToHost>(), Ok(None));

        let mut decoder = Decoder::default();
        decoder.push(&[RESIZE, 0, 0, 0, 1, 9]);
        assert_eq!(
            decoder.next_frame::<ToHost>(),
            Err(FrameError::Malformed(RESIZE))
        );

        let mut decoder = Decoder::default();
        decoder.push(&[INPUT, 0xff, 0xff, 0xff, 0xff]);
        assert!(matches!(
            decoder.next_frame::<ToHost>(),
            Err(FrameError::TooLarge(_))
        ));
    }
}
