//! `extension_ui_request`: dialogs become blocking requests, everything else a notice.

use controlroom_protocol::ids::RequestId;
use controlroom_protocol::session::RequestState;
use controlroom_protocol::timeline::{ItemBody, NoticeLevel};

use super::PiDriver;
use super::dialogs::{self, Dialog, DialogKind};
use super::items::{At, Life, item_id};
use super::state::OpenDialog;
use super::wire::{InLine, UiRequest};
use crate::{DriverEvent, FoldOut, LineTag};

impl PiDriver {
    /// `extension_ui_request`.
    pub(super) fn on_ui_request(&mut self, at: At, ui: UiRequest, out: &mut FoldOut) {
        if let Some(kind) = DialogKind::of(&ui.method) {
            self.open_dialog(at, kind, ui, out);
            return;
        }
        let text = match ui.method.as_str() {
            "notify" => {
                let level = match ui.notify_type.as_deref() {
                    Some("warning") => NoticeLevel::Warning,
                    Some("error") => NoticeLevel::Error,
                    _ => NoticeLevel::Info,
                };
                let text = ui.message.unwrap_or_default();
                self.notice(at, level, text, out);
                return;
            }
            "setStatus" => {
                let key = ui.status_key.unwrap_or_default();
                match ui.status_text {
                    Some(text) => format!("{key}: {text}"),
                    None => format!("{key}: cleared"),
                }
            }
            "setWidget" => {
                let key = ui.widget_key.unwrap_or_default();
                match ui.widget_lines {
                    Some(lines) => format!("{key}:\n{}", lines.join("\n")),
                    None => format!("{key}: cleared"),
                }
            }
            "setTitle" => format!("Title: {}", ui.title.unwrap_or_default()),
            // `set_editor_text` and unknown methods have nothing to show.
            _ => return,
        };
        self.notice(at, NoticeLevel::Info, text, out);
    }

    fn open_dialog(&mut self, at: At, kind: DialogKind, ui: UiRequest, out: &mut FoldOut) {
        let id = item_id("dialog", at.seq, None);
        let request = RequestId::new(id.as_str());
        let neutral = dialogs::request(request.clone(), kind, &ui, at.ts);
        self.items.create(
            &mut out.ops,
            id,
            at,
            at.seq,
            ItemBody::Request {
                request: neutral.clone(),
            },
            Life::Open,
        );
        let deadline = ui.timeout.map(|ms| {
            at.ts
                .0
                .saturating_add(i64::try_from(ms).unwrap_or(i64::MAX))
        });
        self.process.dialogs.push(OpenDialog {
            request: request.clone(),
            dialog: Dialog {
                pi_id: ui.id,
                kind,
                options: ui.options,
                deadline,
                in_run: self.process.run.is_some(),
            },
        });
        self.view.open_requests.push(request);
        out.events
            .push(DriverEvent::RequestOpened(Box::new(neutral)));
    }

    /// An `extension_ui_response` line. Ours (tagged) is accepted if the dialog is still open
    /// and rejected otherwise (pi ignores it silently, obs. 11); anyone else's answer closes
    /// the request as far as the runtime is concerned.
    pub(super) fn on_answer(
        &mut self,
        at: At,
        line: &InLine,
        tag: Option<LineTag>,
        out: &mut FoldOut,
    ) {
        let index = line.id.as_deref().and_then(|id| {
            self.process
                .dialogs
                .iter()
                .position(|open| open.dialog.pi_id == id)
        });
        let Some(index) = index else {
            if let Some(tag) = tag {
                out.events.push(DriverEvent::Rejected {
                    tag,
                    error: "the dialog is no longer open".into(),
                });
            }
            return;
        };
        let open = self.process.dialogs.remove(index);
        let answer = dialogs::answer(&open.dialog, line, at.ts);
        self.items
            .finish(&mut out.ops, open.request.as_str(), at, |body| {
                if let ItemBody::Request { request } = body {
                    request.state = RequestState::Answered;
                    request.answer = Some(answer);
                }
            });
        self.view.open_requests.retain(|id| *id != open.request);
        out.events.push(match tag {
            Some(tag) => DriverEvent::Accepted { tag },
            None => DriverEvent::RequestClosed { id: open.request },
        });
    }

    /// Expires dialogs whose timeout passed by this record's time (pi reports no timeout).
    pub(super) fn expire_dialogs(&mut self, at: At, out: &mut FoldOut) {
        let now = at.ts.0;
        self.close_dialogs(
            at,
            RequestState::Expired,
            |dialog| dialog.deadline.is_some_and(|deadline| deadline <= now),
            out,
        );
    }

    /// Closes the open dialogs matching `which` without an answer.
    pub(super) fn close_dialogs(
        &mut self,
        at: At,
        state: RequestState,
        which: impl Fn(&Dialog) -> bool,
        out: &mut FoldOut,
    ) {
        let (closing, open): (Vec<_>, Vec<_>) = std::mem::take(&mut self.process.dialogs)
            .into_iter()
            .partition(|open| which(&open.dialog));
        self.process.dialogs = open;
        for open in closing {
            self.items
                .finish(&mut out.ops, open.request.as_str(), at, |body| {
                    if let ItemBody::Request { request } = body {
                        request.state = state;
                    }
                });
            self.view.open_requests.retain(|id| *id != open.request);
            out.events
                .push(DriverEvent::RequestClosed { id: open.request });
        }
    }
}
