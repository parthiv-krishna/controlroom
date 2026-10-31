//! Deterministic scrubbing of raw recordings before they are committed.
//!
//! Replacements are textual and applied to each stdio line's `raw` text (and
//! to the paths in `meta` records), so everything that needs no change stays
//! byte-exact (pi's number formatting, key order). Every replacement is free
//! of quotes and backslashes, so JSON lines stay valid JSON. The rules:
//!
//! - the recording temp dir (learned from the `meta` record's labels) becomes
//!   `/home/alice/...` (`work` → `project`, `agent` → `.pi/agent`), also in
//!   pi's encoded session dir names (`--home-alice-project--`);
//! - the mock model port becomes 4000; `meta` pids become 0;
//! - Nix store hashes are zeroed; the real user name, home and host name
//!   become `alice`, `/home/alice` and `devbox`;
//! - UUIDs (session ids, dialog ids) and 8-hex entry ids become sequential
//!   placeholders, consistent across all transcripts of one scenario;
//! - unix-ms timestamps (`ts`, `"timestamp":<ms>`) and ISO timestamps
//!   (including session file names) are rebased so the scenario starts at
//!   2023-11-14T22:13:20Z, keeping relative timing.

use std::collections::HashMap;
use std::sync::LazyLock;

use controlroom_transcript::{Body, Line, Meta, Record};
use regex::{Captures, Regex};

use crate::fixture::{LABEL_BASE_URL, LABEL_ROOT};

/// Fixed start of every sanitized scenario (unix ms).
pub const EPOCH_MS: u64 = 1_700_000_000_000;
/// Mock model port after sanitizing.
pub const PORT: u16 = 4000;
/// Where the temp dir's `work/` (pi's cwd) ends up.
pub const PROJECT: &str = "/home/alice/project";

static NIX_STORE: LazyLock<Regex> = LazyLock::new(|| re(r"/nix/store/[0-9a-z]{32}-"));
static LEFTOVER_ROOT: LazyLock<Regex> = LazyLock::new(|| re(r"cr-fixture-[A-Za-z0-9]{6}"));
static UUID: LazyLock<Regex> =
    LazyLock::new(|| re(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"));
static ENTRY_ID: LazyLock<Regex> = LazyLock::new(|| {
    re(r#""(id|parentId|targetId|firstKeptEntryId|leafId|since|entryId)":"([0-9a-f]{8})""#)
});
static MS_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| re(r#""timestamp":(\d{13})\b"#));
static ISO_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(\d{4})-(\d{2})-(\d{2})T(\d{2})([:-])(\d{2})([:-])(\d{2})([.-])(\d{3})Z")
});

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

/// Machine-specific words to scrub besides the recording environment.
#[derive(Debug, Clone, Default)]
pub struct Sanitizer {
    /// `(real, placeholder)` pairs replaced verbatim, longest first.
    pub words: Vec<(String, String)>,
}

impl Sanitizer {
    /// Scrub this machine's home dir, user name and host name.
    pub fn for_this_machine() -> Self {
        let mut words = Vec::new();
        if let Ok(home) = std::env::var("HOME")
            && home.len() > 1
        {
            words.push((home, "/home/alice".to_owned()));
        }
        if let Ok(user) = std::env::var("USER")
            && user.len() > 2
        {
            words.push((user, "alice".to_owned()));
        }
        if let Ok(host) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
            let host = host.trim().to_owned();
            if host.len() > 2 {
                words.push((host, "devbox".to_owned()));
            }
        }
        words.sort_by_key(|(word, _)| std::cmp::Reverse(word.len()));
        Self { words }
    }

    /// Sanitize all transcripts of one scenario with shared id maps.
    pub fn scenario(&self, transcripts: &[Vec<Record>]) -> Vec<Vec<Record>> {
        let mut pass = Pass::new(self, transcripts);
        transcripts
            .iter()
            .map(|records| records.iter().map(|r| pass.record(r)).collect())
            .collect()
    }
}

/// State of one sanitizing pass over a scenario.
struct Pass<'a> {
    sanitizer: &'a Sanitizer,
    /// `(literal, replacement)` pairs from the `env` meta records.
    env: Vec<(String, String)>,
    base_ms: u64,
    uuids: HashMap<String, String>,
    entry_ids: HashMap<String, String>,
}

impl<'a> Pass<'a> {
    fn new(sanitizer: &'a Sanitizer, transcripts: &[Vec<Record>]) -> Self {
        let mut env = Vec::new();
        for record in transcripts.iter().flatten() {
            let Body::Meta(meta) = &record.body else {
                continue;
            };
            if let Some(root) = meta.labels.get(LABEL_ROOT) {
                env.extend(root_replacements(root));
            }
            if let Some(host) = meta
                .labels
                .get(LABEL_BASE_URL)
                .and_then(|url| url.strip_prefix("http://"))
                .and_then(|rest| rest.split('/').next())
            {
                env.push((host.to_owned(), format!("127.0.0.1:{PORT}")));
            }
        }
        env.sort_by_key(|(literal, _)| std::cmp::Reverse(literal.len()));
        env.dedup();
        let base_ms = transcripts
            .iter()
            .flatten()
            .map(|r| r.ts)
            .min()
            .unwrap_or(EPOCH_MS);
        Self {
            sanitizer,
            env,
            base_ms,
            uuids: HashMap::new(),
            entry_ids: HashMap::new(),
        }
    }

    fn record(&mut self, record: &Record) -> Record {
        let mut line = |line: &Line| Line {
            raw: self.text(&line.raw),
            ..line.clone()
        };
        let body = match &record.body {
            Body::In(l) => Body::In(line(l)),
            Body::Out(l) => Body::Out(line(l)),
            Body::Stderr(l) => Body::Stderr(line(l)),
            Body::Meta(meta) => Body::Meta(Box::new(self.meta(meta))),
            other @ (Body::Exit(_) | Body::Unknown(_)) => other.clone(),
        };
        Record {
            seq: record.seq,
            ts: self.rebase(record.ts),
            body,
        }
    }

    /// Scrub the paths in a `meta` record and zero the (run-specific) pids.
    fn meta(&mut self, meta: &Meta) -> Meta {
        Meta {
            argv: meta.argv.iter().map(|arg| self.text(arg)).collect(),
            cwd: self.text(&meta.cwd),
            labels: meta
                .labels
                .iter()
                .map(|(key, value)| (key.clone(), self.text(value)))
                .collect(),
            child_pid: 0,
            supervisor_pid: 0,
            ..meta.clone()
        }
    }

    fn rebase(&self, ms: u64) -> u64 {
        EPOCH_MS + ms.saturating_sub(self.base_ms)
    }

    fn text(&mut self, raw: &str) -> String {
        let mut out = raw.to_owned();
        for (literal, replacement) in self.env.iter().chain(&self.sanitizer.words) {
            if out.contains(literal.as_str()) {
                out = out.replace(literal.as_str(), replacement);
            }
        }
        out = LEFTOVER_ROOT.replace_all(&out, "cr-fixture").into_owned();
        out = NIX_STORE
            .replace_all(&out, "/nix/store/00000000000000000000000000000000-")
            .into_owned();
        out = UUID
            .replace_all(&out, |c: &Captures| {
                let next = self.uuids.len() + 1;
                self.uuids
                    .entry(c[0].to_owned())
                    .or_insert_with(|| format!("00000000-0000-4000-8000-{next:012}"))
                    .clone()
            })
            .into_owned();
        out = ENTRY_ID
            .replace_all(&out, |c: &Captures| {
                let next = self.entry_ids.len() + 1;
                let id = self
                    .entry_ids
                    .entry(c[2].to_owned())
                    .or_insert_with(|| format!("e{next:07x}"));
                format!(r#""{}":"{id}""#, &c[1])
            })
            .into_owned();
        out = MS_TIMESTAMP
            .replace_all(&out, |c: &Captures| {
                let ms: u64 = c[1].parse().unwrap_or(0);
                format!(r#""timestamp":{}"#, self.rebase(ms))
            })
            .into_owned();
        ISO_TIMESTAMP
            .replace_all(&out, |c: &Captures| self.iso(c))
            .into_owned()
    }

    /// Rebase an ISO timestamp, keeping its separators.
    fn iso(&self, c: &Captures) -> String {
        let num = |i: usize| c[i].parse::<i64>().unwrap_or(0);
        let days = days_from_civil(num(1), num(2), num(3));
        let ms = (((days * 24 + num(4)) * 60 + num(6)) * 60 + num(8)) * 1000 + num(10);
        let rebased = self.rebase(u64::try_from(ms).unwrap_or(0));
        let (secs, millis) = (rebased / 1000, rebased % 1000);
        let (y, m, d) = civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0));
        let rem = secs % 86_400;
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}{}{:02}{}{:02}{}{millis:03}Z",
            rem / 3600,
            &c[5],
            rem / 60 % 60,
            &c[7],
            rem % 60,
            &c[9],
        )
    }
}

/// Literal replacements for the recording temp dir `root`.
fn root_replacements(root: &str) -> Vec<(String, String)> {
    let encoded = root.trim_start_matches('/').replace('/', "-");
    vec![
        (format!("{root}/work"), PROJECT.to_owned()),
        (format!("{root}/agent"), "/home/alice/.pi/agent".to_owned()),
        (
            format!("{root}/ext"),
            "/home/alice/.pi/fixture-ext".to_owned(),
        ),
        (format!("{root}/home"), "/home/alice".to_owned()),
        (root.to_owned(), "/home/alice/.cr-fixture".to_owned()),
        (format!("{encoded}-work"), "home-alice-project".to_owned()),
    ]
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of `days` since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::recorder_meta;

    fn rec(seq: u64, ts: u64, body: fn(Line) -> Body, raw: &str) -> Record {
        Record {
            seq,
            ts,
            body: body(Line::new(raw)),
        }
    }

    fn raw(record: &Record) -> &str {
        record.raw().unwrap_or_default()
    }

    fn sanitizer() -> Sanitizer {
        Sanitizer {
            words: vec![
                ("/home/realuser".into(), "/home/alice".into()),
                ("realuser".into(), "alice".into()),
                ("realhost".into(), "devbox".into()),
            ],
        }
    }

    fn env_meta() -> Record {
        let mut meta = recorder_meta("s", "/tmp/cr-fixture-AbC123", "http://127.0.0.1:41234/v1");
        meta.argv = vec![
            "pi".into(),
            "-e".into(),
            "/tmp/cr-fixture-AbC123/ext/x.ts".into(),
        ];
        meta.cwd = "/tmp/cr-fixture-AbC123/work".into();
        meta.child_pid = 42;
        meta.supervisor_pid = 41;
        Record {
            seq: 1,
            ts: 1_800_000_000_000,
            body: Body::Meta(Box::new(meta)),
        }
    }

    #[test]
    fn scrubs_environment_paths_and_ports() {
        let line = r#"{"sessionFile":"/tmp/cr-fixture-AbC123/agent/sessions/--tmp-cr-fixture-AbC123-work--/x.jsonl","cwd":"/tmp/cr-fixture-AbC123/work","baseUrl":"http://127.0.0.1:41234/v1","p":"/nix/store/0123456789abcdefghijklmnopqrstuv-pi/x","who":"realuser@realhost in /home/realuser"}"#;
        let out =
            sanitizer().scenario(&[vec![env_meta(), rec(2, 1_800_000_000_005, Body::Out, line)]]);
        assert_eq!(
            raw(&out[0][1]),
            r#"{"sessionFile":"/home/alice/.pi/agent/sessions/--home-alice-project--/x.jsonl","cwd":"/home/alice/project","baseUrl":"http://127.0.0.1:4000/v1","p":"/nix/store/00000000000000000000000000000000-pi/x","who":"alice@devbox in /home/alice"}"#
        );
        let Body::Meta(meta) = &out[0][0].body else {
            panic!("not meta");
        };
        assert_eq!(meta.argv, ["pi", "-e", "/home/alice/.pi/fixture-ext/x.ts"]);
        assert_eq!(meta.cwd, "/home/alice/project");
        assert_eq!(meta.labels[LABEL_ROOT], "/home/alice/.cr-fixture");
        assert_eq!(meta.labels[LABEL_BASE_URL], "http://127.0.0.1:4000/v1");
        assert_eq!((meta.child_pid, meta.supervisor_pid), (0, 0));
        assert_eq!(out[0][0].ts, EPOCH_MS);
        assert_eq!(out[0][1].ts, EPOCH_MS + 5);
    }

    #[test]
    fn untouched_lines_stay_byte_exact() {
        let line = r#"{"type":"message_update","usage":{"cost":{"input":0.00009999999999999999}}}"#;
        let out = sanitizer().scenario(&[vec![rec(1, 1, Body::Out, line)]]);
        assert_eq!(raw(&out[0][0]), line);
    }

    #[test]
    fn ids_map_consistently_across_transcripts() {
        let a = r#"{"sessionId":"01a19363-7b62-7513-8bc3-f06e3e6de70c","entries":[{"id":"ab12cd34","parentId":null},{"id":"ffff0000","parentId":"ab12cd34"}]}"#;
        let b = r#"{"type":"get_entries","since":"ffff0000","x":"01a19363-7b62-7513-8bc3-f06e3e6de70c"}"#;
        let out =
            sanitizer().scenario(&[vec![rec(1, 1, Body::Out, a)], vec![rec(1, 2, Body::In, b)]]);
        assert_eq!(
            raw(&out[0][0]),
            r#"{"sessionId":"00000000-0000-4000-8000-000000000001","entries":[{"id":"e0000001","parentId":null},{"id":"e0000002","parentId":"e0000001"}]}"#
        );
        assert_eq!(
            raw(&out[1][0]),
            r#"{"type":"get_entries","since":"e0000002","x":"00000000-0000-4000-8000-000000000001"}"#
        );
    }

    #[test]
    fn timestamps_are_rebased() {
        // 2027-01-15T08:00:00.000Z
        let start = 1_800_000_000_000;
        let line = r#"{"timestamp":1800000000250,"t":"2027-01-15T08:00:01.500Z","f":"2027-01-15T08-00-01-500Z_x.jsonl"}"#;
        let out = sanitizer().scenario(&[vec![rec(1, start, Body::Out, line)]]);
        assert_eq!(
            raw(&out[0][0]),
            r#"{"timestamp":1700000000250,"t":"2023-11-14T22:13:21.500Z","f":"2023-11-14T22-13-21-500Z_x.jsonl"}"#
        );
    }

    #[test]
    fn civil_round_trip() {
        for days in [-1, 0, 59, 365, 19_675, 20_000, 50_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(19_675), (2023, 11, 14));
    }
}
