//! A credential added after the agent started: the agent keeps its launch's credential routes,
//! so its model requests fail at the connection level ("Connection error.", as when nono's proxy
//! has no route for the provider), the summary says the credentials are stale, and
//! `session.restart` relaunches it with the key added in Settings › Models: the next request
//! gets through.

use controlroom_controller::test_support::op;
use controlroom_e2e::world::World;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_picks_up_a_key_added_after_the_launch() {
    let mut world = World::start().await;
    world.agentd.scenario(&json!({
        "models": [{"id": "mock-model", "name": "Mock", "provider": "anthropic"}],
        "max_retries": 0,
        "needs_credentials": true,
    }));
    let devbox = world.devbox.id.clone();
    let id = world
        .create_session(&[(&devbox, "~/work")], json!({}))
        .await;

    // No key yet: the request fails at the connection level, and nothing changed since the
    // launch.
    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "hello").await;
    let failed = world
        .wait_session(&id, "the failed turn", |s| {
            s["last_turn_end_seq"].as_u64() > Some(mark) && s["state"] != "working"
        })
        .await;
    assert_eq!(failed["last_error"], "Connection error.", "{failed}");
    assert_eq!(failed["stale_credentials"], false);

    // The key arrives through Settings › Models: the running agent's credentials are stale.
    world
        .ui
        .ok(
            "models.set_key",
            json!({"provider": "anthropic", "key": "sk-ant-api03-fake-0000"}),
        )
        .await;
    world
        .wait_session(&id, "stale credentials", |s| s["stale_credentials"] == true)
        .await;

    // The restart's launch has the key: the conversation resumes and the request gets through.
    let launched = world.session(&id)["updated_at"].clone();
    world
        .ui
        .ok("session.restart", json!({"op_id": op(), "id": id}))
        .await;
    world
        .wait_session(&id, "the restarted agent", |s| {
            s["agent_live"] == true
                && s["stale_credentials"] == false
                && s["updated_at"] != launched
        })
        .await;
    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "hello again").await;
    let done = world
        .wait_session(&id, "the second turn", |s| {
            s["last_turn_end_seq"].as_u64() > Some(mark) && s["state"] != "working"
        })
        .await;
    assert_eq!(done["last_outcome"], "completed", "{done}");
    assert_eq!(done["stale_credentials"], false);
}
