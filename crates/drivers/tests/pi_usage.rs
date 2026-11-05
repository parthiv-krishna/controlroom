//! Usage reports from pi's RPC events: every model call pi bills (replies, tools' own model
//! calls, compactions, branch summaries, cache warm-ups) is reported once, as pi's
//! `getSessionStats` counts it.

mod support;

use controlroom_drivers::{Driver, DriverEvent, PiDriver};
use controlroom_protocol::timeline::TokenUsage;
use serde_json::{Value, json};
use support::out_line;

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64, cost: f64) -> Value {
    json!({
        "input": input, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write,
        "totalTokens": input + output + cache_read + cache_write,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": cost}
    })
}

/// The usage events of folding `lines` (pi stdout) in order.
fn reports(lines: &[Value]) -> Vec<(TokenUsage, Option<f64>, bool)> {
    let mut driver = PiDriver::new();
    let mut reports = Vec::new();
    for (seq, line) in (1..).zip(lines) {
        for event in driver.fold(&out_line(seq, &line.to_string())).events {
            if let DriverEvent::Usage {
                tokens,
                cost,
                reply,
            } = event
            {
                reports.push((tokens, cost, reply));
            }
        }
    }
    reports
}

fn tokens(input: u64, output: u64, cache_read: u64, cache_write: u64) -> TokenUsage {
    TokenUsage {
        input,
        output,
        cache_read,
        cache_write,
    }
}

#[test]
fn replies_tool_results_compactions_and_appended_entries_report_usage() {
    let reply = json!({"type": "message_end", "message": {
        "role": "assistant", "content": [], "stopReason": "stop", "usage": usage(10, 5, 90, 0, 0.5)
    }});
    let tool_result = json!({"type": "message_end", "message": {
        "role": "toolResult", "toolCallId": "c1", "toolName": "t", "content": [],
        "isError": false, "usage": usage(7, 3, 0, 0, 0.25)
    }});
    let plain_tool_result = json!({"type": "message_end", "message": {
        "role": "toolResult", "toolCallId": "c2", "toolName": "t", "content": [], "isError": false
    }});
    let compaction = json!({"type": "compaction_end", "reason": "manual", "aborted": false,
        "result": {"tokensBefore": 100, "estimatedTokensAfter": 20, "usage": usage(100, 12, 0, 0, 0.125)}
    });
    let warm = json!({"type": "entry_appended", "entry": {
        "type": "usage", "id": "e1", "parentId": null, "kind": "cache_warm",
        "provider": "p", "model": "m", "usage": usage(0, 1, 80, 0, 0.01)
    }});
    let branch = json!({"type": "entry_appended", "entry": {
        "type": "branch_summary", "id": "e2", "summary": "s", "usage": usage(50, 6, 0, 0, 0.02)
    }});
    let custom =
        json!({"type": "entry_appended", "entry": {"type": "custom", "id": "e3", "data": {}}});
    assert_eq!(
        reports(&[
            reply,
            tool_result,
            plain_tool_result,
            compaction,
            warm,
            branch,
            custom
        ]),
        vec![
            (tokens(10, 5, 90, 0), Some(0.5), true),
            (tokens(7, 3, 0, 0), Some(0.25), false),
            (tokens(100, 12, 0, 0), Some(0.125), false),
            (tokens(0, 1, 80, 0), Some(0.01), false),
            (tokens(50, 6, 0, 0), Some(0.02), false),
        ]
    );
}

#[test]
fn a_reply_without_tokens_is_reported_but_makes_no_usage_item() {
    let mut driver = PiDriver::new();
    let line = json!({"type": "message_end", "message": {
        "role": "assistant", "content": [], "stopReason": "aborted", "usage": usage(0, 0, 0, 0, 0.0)
    }});
    let out = driver.fold(&out_line(1, &line.to_string()));
    assert_eq!(
        out.events,
        vec![DriverEvent::Usage {
            tokens: TokenUsage::default(),
            cost: Some(0.0),
            reply: true
        }]
    );
    assert!(out.ops.is_empty());
}
