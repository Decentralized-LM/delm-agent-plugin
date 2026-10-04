//! Correlate the MCP socket with native Codex tool invocations. The socket
//! identifies a worker, but only the native event supplies its turn/revision.
use super::Worker;
use crate::worker_tools::Call;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 32;
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(2);

struct Invocation {
    thread: String,
    turn: String,
    revision: u64,
    tool: String,
    arguments: Value,
    active: bool,
    used: bool,
}

#[derive(Default)]
pub(super) struct Calls {
    native: HashMap<(usize, String), Invocation>,
    pending: Vec<(Instant, Call)>,
}

impl Calls {
    pub fn observe(
        &mut self,
        index: usize,
        worker: &Worker,
        params: &Value,
        sequence: Option<u64>,
        started: bool,
    ) -> Result<()> {
        let item = &params["item"];
        if item["type"] != "mcpToolCall"
            || item["server"] != format!("delm_coordination_{}", index + 1)
        {
            return Ok(());
        }
        ensure!(
            params["threadId"].as_str() == Some(&worker.thread),
            "Native MCP event belongs to another worker thread"
        );
        let id = item["id"]
            .as_str()
            .context("Native MCP call identity is missing")?;
        let key = (index, id.to_owned());
        if !started {
            if let Some(invocation) = self.native.get_mut(&key) {
                invocation.active = false;
                invocation.arguments = Value::Null;
            }
            return Ok(());
        }
        let turn = params["turnId"]
            .as_str()
            .context("Native MCP turn is missing")?;
        if worker.turn.as_deref() != Some(turn) {
            return Ok(());
        }
        let sequence = sequence.context("Native MCP start sequence is missing")?;
        let revision = worker
            .revision_fences
            .iter()
            .rev()
            .find(|(boundary, _)| sequence > *boundary)
            .map(|(_, revision)| *revision)
            .context("Native MCP call has no request revision")?;
        self.native.entry(key).or_insert(Invocation {
            thread: worker.thread.clone(),
            turn: turn.into(),
            revision,
            tool: item["tool"]
                .as_str()
                .context("Native MCP tool name is missing")?
                .into(),
            arguments: item["arguments"].clone(),
            active: true,
            used: false,
        });
        Ok(())
    }

    fn authorize(&mut self, workers: &[Worker; 2], call: &Call) -> Result<bool> {
        let worker = workers.get(call.worker).context("Unknown MCP worker")?;
        let id = call
            .call_id
            .as_ref()
            .context("Native MCP callId metadata is required")?;
        let thread = call
            .thread_id
            .as_ref()
            .context("Native MCP threadId metadata is required")?;
        let turn = call
            .turn_id
            .as_ref()
            .context("Native MCP turn_id metadata is required")?;
        ensure!(
            thread == &worker.thread,
            "MCP request belongs to another worker thread"
        );
        ensure!(
            worker.turn.as_ref() == Some(turn),
            "MCP request belongs to an inactive or retired turn"
        );
        let Some(invocation) = self.native.get_mut(&(call.worker, id.clone())) else {
            return Ok(false);
        };
        ensure!(
            invocation.active && !invocation.used,
            "Native MCP invocation already ended or was used"
        );
        ensure!(
            invocation.thread == *thread && invocation.turn == *turn,
            "MCP request belongs to a retired turn"
        );
        ensure!(
            invocation.revision == worker.revision,
            "MCP request belongs to an earlier user revision"
        );
        ensure!(
            invocation.tool == call.tool && invocation.arguments == call.arguments,
            "MCP request differs from its native invocation"
        );
        invocation.used = true;
        invocation.arguments = Value::Null;
        Ok(true)
    }

    pub fn enqueue(&mut self, call: Call) {
        if self.pending.len() >= MAX_PENDING {
            reject(call, "Too many pending native MCP invocations".into());
        } else {
            self.pending.push((Instant::now(), call));
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Native stdout and the local socket are separate channels. Wait briefly
    /// for a matching event instead of guessing identity from arrival order.
    pub fn take_ready(&mut self, workers: &[Worker; 2]) -> Vec<Call> {
        let mut ready = Vec::new();
        for (received, call) in std::mem::take(&mut self.pending) {
            match self.authorize(workers, &call) {
                Ok(true) => ready.push(call),
                Ok(false) if received.elapsed() < OBSERVATION_TIMEOUT => {
                    self.pending.push((received, call))
                }
                Ok(false) => reject(
                    call,
                    "Matching native MCP invocation was not observed; retry the tool call".into(),
                ),
                Err(error) => reject(call, error.to_string()),
            }
        }
        ready
    }

    pub fn retire(&mut self, index: usize) {
        self.native.retain(|(worker, _), _| *worker != index);
    }
}

fn reject(call: Call, reason: String) {
    let _ = call
        .reply
        .send(serde_json::json!({"isError":true,"content":[{"type":"text","text":reason}]}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workers() -> [Worker; 2] {
        std::array::from_fn(|index| Worker {
            thread: format!("thread-{index}"),
            turn: Some("turn-1".into()),
            revision: 1,
            revision_fences: vec![(0, 1)],
            ..Default::default()
        })
    }
    fn call() -> Call {
        Call {
            worker: 0,
            call_id: Some("call-1".into()),
            thread_id: Some("thread-0".into()),
            turn_id: Some("turn-1".into()),
            tool: "delm_task_claim".into(),
            arguments: json!({"task_id":1}),
            reply: tokio::sync::oneshot::channel().0,
        }
    }
    fn event() -> Value {
        json!({"threadId":"thread-0","turnId":"turn-1","item":{"type":"mcpToolCall","id":"call-1",
            "server":"delm_coordination_1","tool":"delm_task_claim","arguments":{"task_id":1}}})
    }

    #[test]
    fn socket_waits_for_native_start_and_cannot_replay_or_change_arguments() {
        let workers = workers();
        let mut calls = Calls::default();
        calls.enqueue(call());
        assert!(calls.take_ready(&workers).is_empty());
        assert!(calls.has_pending());
        calls
            .observe(0, &workers[0], &event(), Some(1), true)
            .unwrap();
        let mut altered = call();
        altered.arguments["task_id"] = json!(2);
        assert!(calls.authorize(&workers, &altered).is_err());
        assert_eq!(calls.take_ready(&workers).len(), 1);
        assert!(calls.authorize(&workers, &call()).is_err());
    }

    #[test]
    fn direct_admin_calls_wrong_threads_and_servers_do_not_authorize() {
        let workers = workers();
        let mut calls = Calls::default();
        let mut direct = call();
        direct.call_id = None;
        assert!(calls.authorize(&workers, &direct).is_err());
        let mut other = call();
        other.thread_id = Some("thread-1".into());
        assert!(calls.authorize(&workers, &other).is_err());
        other = call();
        other.turn_id = None;
        assert!(calls.authorize(&workers, &other).is_err());
        let mut wrong_server = event();
        wrong_server["item"]["server"] = json!("delm_coordination_2");
        calls
            .observe(0, &workers[0], &wrong_server, Some(1), true)
            .unwrap();
        assert!(!calls.authorize(&workers, &call()).unwrap());
    }

    #[test]
    fn old_turns_completed_calls_and_pre_update_starts_are_rejected() {
        let mut workers = workers();
        let mut calls = Calls::default();
        calls
            .observe(0, &workers[0], &event(), Some(1), true)
            .unwrap();
        workers[0].turn = Some("turn-2".into());
        assert!(calls.authorize(&workers, &call()).is_err());
        workers[0].turn = Some("turn-1".into());
        calls
            .observe(0, &workers[0], &event(), None, false)
            .unwrap();
        assert!(calls.authorize(&workers, &call()).is_err());
        calls.retire(0);
        workers[0].revision = 2;
        workers[0].revision_fences.push((10, 2));
        calls
            .observe(0, &workers[0], &event(), Some(9), true)
            .unwrap();
        assert!(calls.authorize(&workers, &call()).is_err());
    }

    #[test]
    fn missing_native_event_times_out_with_a_tool_error() {
        let mut calls = Calls::default();
        let mut call = call();
        let (reply, mut result) = tokio::sync::oneshot::channel();
        call.reply = reply;
        calls
            .pending
            .push((Instant::now() - OBSERVATION_TIMEOUT, call));
        assert!(calls.take_ready(&workers()).is_empty());
        assert_eq!(result.try_recv().unwrap()["isError"], true);
        assert!(!calls.has_pending());
    }
}
