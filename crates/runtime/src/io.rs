//! The async half of the runtime: tasks that talk to the local supervisors (launches,
//! attachments, barriers, recovery) or wait on hosts (`hosts`), and feed what they learn back
//! into the core.
//!
//! Each attachment runs three tasks: a **writer** that writes staged lines in order, a
//! **barrier** that ends the attach-time doubt, and a **reader** that folds the supervisor's
//! record stream. When the stream ends, the reader catches up from the transcript file and
//! detaches, which applies the dead-agent rules.

use std::sync::Arc;

use controlroom_protocol::ids::{Seq, SessionId};
use controlroom_transcript::Record;
use tokio::sync::mpsc;

use crate::core::Task;
use crate::ingest::Source;
use crate::ports::SupervisorConn;
use crate::runtime::Inner;

/// Lines buffered per attachment before staging fails over to a barrier.
const WRITER_CAPACITY: usize = 256;
/// Records folded per lock acquisition while re-folding a transcript.
const REFOLD_BATCH: usize = 256;

/// Starts the tasks a transaction asked for.
pub(crate) fn spawn_tasks(inner: &Arc<Inner>, tasks: Vec<Task>) {
    for task in tasks {
        let inner2 = inner.clone();
        match task {
            Task::Launch(id) => inner.spawn(launch(inner2, id)),
            Task::Terminate(conn, grace) => inner.spawn(async move {
                if let Err(e) = conn.terminate(grace).await {
                    tracing::warn!("terminating an agent: {e}");
                }
            }),
            Task::Reload(id) => inner.spawn(reload(inner2, id)),
            Task::Remove(id, conn) => inner.spawn(async move {
                if let Some(conn) = conn
                    && let Err(e) = conn.terminate(std::time::Duration::ZERO).await
                {
                    tracing::warn!("session {id}: stopping its agent: {e}");
                }
                if let Err(e) = inner2.host.remove(id).await {
                    tracing::warn!("session {id}: removing its files: {e}");
                }
            }),
            Task::Barrier {
                id,
                generation,
                conn,
            } => inner.spawn(barrier(inner2, id, generation, conn)),
            Task::Sync(machine) => inner.spawn(crate::hosts::connected(inner2, machine)),
            Task::Attach(id, late) => inner.spawn(crate::hosts::attach(inner2, id, late)),
            Task::OpenShells(id) => inner.spawn(crate::hosts::open_shells(inner2, id)),
            Task::Refresh(id) => inner.spawn(crate::hosts::refresh_session(inner2, id)),
        };
    }
}

/// Launches the session's agent and attaches to its supervisor.
async fn launch(inner: Arc<Inner>, id: SessionId) {
    let Ok(Some(launch)) = inner.transact(|core| core.prepare_launch(id)) else {
        return;
    };
    let agents = inner.agents.clone();
    let spec = tokio::task::spawn_blocking(move || agents.spawn_spec(&launch)).await;
    let result = match spec {
        Ok(Ok(spec)) => match inner.host.launch(id, spec).await {
            Ok(()) => inner.host.connect(id).await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        },
        Ok(Err(e)) => Err(e.message),
        Err(e) => Err(e.to_string()),
    };
    match result {
        Ok(Some(conn)) => start_attachment(&inner, id, conn),
        Ok(None) => fail(&inner, id, "the supervisor exited at once".into()),
        Err(e) => fail(&inner, id, e),
    }
}

fn fail(inner: &Arc<Inner>, id: SessionId, error: String) {
    let _ = inner.transact(|core| core.launch_failed(id, error));
}

/// Takes over a supervisor connection and starts its writer, barrier and reader.
pub(crate) fn start_attachment(inner: &Arc<Inner>, id: SessionId, conn: Arc<dyn SupervisorConn>) {
    let (writer, lines) = mpsc::channel(WRITER_CAPACITY);
    let attached = inner.transact(|core| core.attach(id, conn.clone(), writer));
    let Ok(Some(attached)) = attached else {
        // The session is gone (deleted while starting): don't leave its agent behind.
        inner.spawn(async move {
            let _ = conn.terminate(std::time::Duration::ZERO).await;
        });
        return;
    };
    let generation = attached.generation;
    let handles = vec![
        inner.spawn(write_lines(
            inner.clone(),
            id,
            generation,
            conn.clone(),
            lines,
        )),
        inner.spawn(barrier(inner.clone(), id, generation, conn.clone())),
        inner.spawn(read_records(
            inner.clone(),
            id,
            generation,
            conn,
            attached.from,
        )),
    ];
    inner.with_core(|core| match core.sessions.get_mut(&id) {
        Some(session)
            if session.live.attachment.as_ref().map(|a| a.generation) == Some(generation) =>
        {
            session.tasks.extend(handles);
        }
        _ => handles.iter().for_each(tokio::task::AbortHandle::abort),
    });
}

async fn write_lines(
    inner: Arc<Inner>,
    id: SessionId,
    generation: u64,
    conn: Arc<dyn SupervisorConn>,
    mut lines: mpsc::Receiver<String>,
) {
    while let Some(line) = lines.recv().await {
        if let Err(e) = conn.write(line).await {
            tracing::warn!("session {id}: write failed: {e}");
            let _ = inner.transact(|core| core.write_failed(id, generation));
        }
    }
}

async fn barrier(inner: Arc<Inner>, id: SessionId, generation: u64, conn: Arc<dyn SupervisorConn>) {
    match conn.barrier().await {
        Ok(durable) => {
            let _ = inner.transact(|core| core.barrier_done(id, generation, durable));
        }
        // The reader sees the stream end and detaches.
        Err(e) => tracing::warn!("session {id}: barrier failed: {e}"),
    }
}

async fn read_records(
    inner: Arc<Inner>,
    id: SessionId,
    generation: u64,
    conn: Arc<dyn SupervisorConn>,
    from: Seq,
) {
    match conn.read(from).await {
        Ok(mut stream) => {
            while let Some(record) = stream.next().await {
                let _ = inner.transact(|core| core.ingest(id, Source::Stream(generation), &record));
            }
        }
        Err(e) => tracing::warn!("session {id}: reading the supervisor: {e}"),
    }
    drop(conn);
    // The supervisor is gone: whatever it logged last is in the file.
    let from = inner.with_core(|core| Seq(core.head(id).0 + 1));
    refold(&inner, id, from).await;
    let _ = inner.transact(|core| core.detach(id, generation));
}

/// Folds the transcript file from `from` to its end.
async fn refold(inner: &Arc<Inner>, id: SessionId, from: Seq) {
    let mut stream = match inner.host.read_transcript(id, from).await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::warn!("session {id}: reading its transcript: {e}");
            return;
        }
    };
    let mut batch: Vec<Record> = Vec::with_capacity(REFOLD_BATCH);
    loop {
        let next = stream.next().await;
        let end = next.is_none();
        batch.extend(next);
        if end || batch.len() == REFOLD_BATCH {
            let records = std::mem::take(&mut batch);
            let _ = inner.transact(|core| {
                records
                    .iter()
                    .try_for_each(|record| core.ingest(id, Source::File, record))
            });
        }
        if end {
            return;
        }
    }
}

/// Rebuilds a session from its transcript and reattaches its supervisor (startup recovery,
/// or after a failed transaction).
pub(crate) async fn recover_session(inner: Arc<Inner>, id: SessionId) {
    refold(&inner, id, Seq(1)).await;
    match inner.host.connect(id).await {
        Ok(Some(conn)) => start_attachment(&inner, id, conn),
        Ok(None) => {
            let _ = inner.transact(|core| core.no_supervisor(id));
        }
        Err(e) => {
            tracing::warn!("session {id}: connecting to its supervisor: {e}");
            let _ = inner.transact(|core| core.no_supervisor(id));
        }
    }
}

async fn reload(inner: Arc<Inner>, id: SessionId) {
    tracing::warn!("session {id}: rebuilding after a failed transaction");
    if inner
        .with_core(|core| core.reset_session(id))
        .unwrap_or(false)
    {
        recover_session(inner, id).await;
    }
}
