//! Splits a shell's terminal output into the events of the shell integration: OSC 133
//! `C;<marker>` (an agent command starts), `D;<status>` (a command ended), `A` (a prompt), and
//! OSC 7 (the cwd). Everything between a `C` and the next `D` is that command's output.
//!
//! Bytes arrive in arbitrary chunks, so a sequence may straddle two of them: a possible
//! partial sequence at the end of a chunk is held back until the next one. Output outside a
//! command is discarded (the human's own typing and the prompt).

/// What the parser saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// OSC 133 `C;<marker>`: the agent command with this marker started.
    Started(String),
    /// Output of the running command.
    Output(Vec<u8>),
    /// OSC 133 `D;<status>`: the command (or prompt cycle) ended with this status.
    Ended(Option<i32>),
    /// OSC 7: the shell's directory.
    Cwd(String),
    /// OSC 133 `A`: a prompt is shown; the shell waits for input.
    Prompt,
}

/// Longest OSC sequence considered; longer ones are passed through as output.
const MAX_OSC: usize = 4096;

/// The parser state.
#[derive(Debug, Default)]
pub struct Parser {
    pending: Vec<u8>,
    capturing: bool,
}

impl Parser {
    /// Feeds bytes; returns the events in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.pending.extend_from_slice(bytes);
        let data = std::mem::take(&mut self.pending);
        let mut events = Vec::new();
        let mut output = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let Some(esc) = memchr::memchr(0x1b, &data[i..]).map(|p| i + p) else {
                output.extend_from_slice(&data[i..]);
                i = data.len();
                break;
            };
            output.extend_from_slice(&data[i..esc]);
            match osc_at(&data[esc..]) {
                Osc::Incomplete => {
                    self.pending = data[esc..].to_vec();
                    i = data.len();
                    break;
                }
                Osc::Plain => {
                    output.push(0x1b);
                    i = esc + 1;
                }
                Osc::Complete { body, len } => {
                    i = esc + len;
                    match classify(body) {
                        Some(event) => {
                            self.flush(&mut output, &mut events);
                            match &event {
                                Event::Started(_) => self.capturing = true,
                                Event::Ended(_) => self.capturing = false,
                                _ => {}
                            }
                            events.push(event);
                        }
                        None => output.extend_from_slice(&data[esc..i]),
                    }
                }
            }
        }
        debug_assert!(i == data.len());
        self.flush(&mut output, &mut events);
        events
    }

    fn flush(&self, output: &mut Vec<u8>, events: &mut Vec<Event>) {
        if self.capturing && !output.is_empty() {
            events.push(Event::Output(std::mem::take(output)));
        } else {
            output.clear();
        }
    }
}

enum Osc<'a> {
    /// Not an OSC sequence (or too long to be one of ours): plain output.
    Plain,
    /// Possibly an OSC sequence, not terminated yet.
    Incomplete,
    /// `ESC ] body (BEL | ESC \)`, `len` bytes in all.
    Complete { body: &'a [u8], len: usize },
}

fn osc_at(data: &[u8]) -> Osc<'_> {
    match data.get(1) {
        None => return Osc::Incomplete,
        Some(b']') => {}
        Some(_) => return Osc::Plain,
    }
    let rest = &data[2..];
    for (j, &b) in rest.iter().enumerate().take(MAX_OSC) {
        match b {
            0x07 => {
                return Osc::Complete {
                    body: &rest[..j],
                    len: j + 3,
                };
            }
            0x1b => {
                return match rest.get(j + 1) {
                    Some(b'\\') => Osc::Complete {
                        body: &rest[..j],
                        len: j + 4,
                    },
                    Some(_) => Osc::Plain,
                    None => Osc::Incomplete,
                };
            }
            _ => {}
        }
    }
    if rest.len() < MAX_OSC {
        Osc::Incomplete
    } else {
        Osc::Plain
    }
}

fn classify(body: &[u8]) -> Option<Event> {
    let text = std::str::from_utf8(body).ok()?;
    if let Some(rest) = text.strip_prefix("133;") {
        let mut parts = rest.splitn(2, ';');
        return match parts.next()? {
            "A" => Some(Event::Prompt),
            "C" => Some(Event::Started(parts.next()?.to_owned())),
            "D" => Some(Event::Ended(parts.next().and_then(|s| s.parse().ok()))),
            _ => None,
        };
    }
    let url = text.strip_prefix("7;file://")?;
    let path = &url[url.find('/')?..];
    Some(Event::Cwd(path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<Event> {
        let mut parser = Parser::default();
        let mut events: Vec<Event> = Vec::new();
        for chunk in chunks {
            for event in parser.feed(chunk) {
                // Merge adjacent output for comparison.
                if let (Event::Output(new), Some(Event::Output(last))) = (&event, events.last_mut())
                {
                    last.extend_from_slice(new);
                    continue;
                }
                events.push(event);
            }
        }
        events
    }

    const RUN: &[u8] = b"$  __cr_run m1 /x\r\n\x1b[2mecho hi\x1b[0m\r\n\x1b]133;C;m1\x07hi\r\n\x1b[31mred\x1b[0m\r\n\x1b]133;D;3\x07\x1b]7;file://devbox/home/alice/src\x07\x1b]133;A\x07$ ";

    #[test]
    fn a_command_between_markers() {
        let expected = vec![
            Event::Started("m1".into()),
            Event::Output(b"hi\r\n\x1b[31mred\x1b[0m\r\n".to_vec()),
            Event::Ended(Some(3)),
            Event::Cwd("/home/alice/src".into()),
            Event::Prompt,
        ];
        assert_eq!(all(&[RUN]), expected);
        // Any split of the stream gives the same events.
        for cut in 1..RUN.len() {
            assert_eq!(all(&[&RUN[..cut], &RUN[cut..]]), expected, "cut at {cut}");
        }
    }

    #[test]
    fn st_terminators_and_foreign_sequences() {
        let events = all(&[
            b"\x1b]133;C;m2\x1b\\a\x1b]0;title\x07b\x1b]133;D;0\x1b\\\x1b]7;file:///tmp\x1b\\",
        ]);
        assert_eq!(
            events,
            vec![
                Event::Started("m2".into()),
                Event::Output(b"a\x1b]0;title\x07b".to_vec()),
                Event::Ended(Some(0)),
                Event::Cwd("/tmp".into()),
            ]
        );
        // Output outside a command is dropped; a prompt cycle still reports its status.
        assert_eq!(
            all(&[b"typing\x1b]133;D;130\x07"]),
            vec![Event::Ended(Some(130))]
        );
    }
}
