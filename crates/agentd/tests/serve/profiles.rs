//! Profiles over the real sockets: `profiles.report` is the controller's and persists across a
//! restart; a session's agent dir has only its profile's providers, even at level off (the
//! server sandbox is always on); `session.move` relaunches the agent with the new profile's.

use serde_json::{Value, json};

use crate::harness::*;

/// The server's pi config: two providers, `mock` and `paid`.
fn server_models(env: &Env) {
    let agent = env.state().join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    let provider = json!({"baseUrl": "http://127.0.0.1:9/v1", "api": "openai-completions",
                          "apiKey": "sk-test-key", "models": [{"id": "m"}]});
    std::fs::write(
        agent.join("models.json"),
        json!({"providers": {"mock": provider, "paid": provider}}).to_string(),
    )
    .unwrap();
}

/// The providers in the session's agent dir.
fn session_providers(env: &Env, id: &str) -> Vec<String> {
    let path = env.state().join(format!("sessions/{id}/agent/models.json"));
    let models: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    models["providers"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

/// How many times the session's agent started.
fn starts(env: &Env, id: &str) -> usize {
    env.records(id)
        .iter()
        .filter(|r| matches!(r.body, controlroom_transcript::Body::Meta(_)))
        .count()
}

fn create_in(env: &Env, client: &Client, profile: &str) -> String {
    let created = client.call(
        "session.create",
        json!({"op_id": op(), "workspaces": env.workspaces(), "sandbox_level": "off",
               "profile_id": profile}),
    );
    assert_eq!(created["session"]["profile_id"], profile);
    created["session"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn reported_policies_shape_the_agent_dir_and_survive_a_restart() {
    let mut env = Env::new();
    env.agent(texts(&["Hello."]));
    server_models(&env);
    env.start();
    let (garden, book) = (
        ulid::Ulid::generate().to_string(),
        ulid::Ulid::generate().to_string(),
    );
    let report = json!({"profiles": [
        {"profile_id": garden, "models": {"kind": "only", "accounts": ["mock"]}},
        {"profile_id": book, "models": {"kind": "any"}},
    ]});
    let client = env.client();
    client.call_err("profiles.report", report.clone(), "unsupported");
    env.controller().call("profiles.report", report);
    env.controller().set_profiles(&[&garden, &book]);
    client.watch_sessions();

    // Level off: pi's agent dir is still the session's own, with the profile's providers only.
    let id = create_in(&env, &client, &garden);
    client.wait_idle(&id);
    assert_eq!(session_providers(&env, &id), ["mock"]);

    // Once profiles are known, a session needs one.
    client.call_err(
        "session.create",
        json!({"op_id": op(), "workspaces": env.workspaces()}),
        "invalid_params",
    );

    // After a restart, before the controller reports again, the stored report applies.
    env.kill_daemon();
    env.start();
    env.controller().set_profiles(&[&garden, &book]);
    let client = env.client();
    client.watch_sessions();
    let id = create_in(&env, &client, &garden);
    client.wait_idle(&id);
    assert_eq!(session_providers(&env, &id), ["mock"]);

    // A move relaunches the agent, resumed, with the target profile's providers.
    let mark = client.turn_mark(&id);
    client.prompt(&id, "hi");
    client.wait_turn(&id, mark);
    let moved = json!({"op_id": op(), "id": id, "profile_id": book});
    client.call_err("session.move", moved.clone(), "unsupported");
    env.controller().call("session.move", moved);
    wait_until("the relaunch", || starts(&env, &id) == 2);
    client.wait_session(&id, "the resumed agent", |s| {
        s["profile_id"] == book.as_str() && s["agent_live"] == true
    });
    assert_eq!(session_providers(&env, &id), ["mock", "paid"]);
}
