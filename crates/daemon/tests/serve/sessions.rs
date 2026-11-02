//! Sessions: create, prompt, idle; errors; options and context.

use crate::harness::*;

#[test]
fn create_prompt_reply_idle() {
    let env = Env::with(texts(&["Hello there."]));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    let result = client.prompt(&id, "hi");
    assert_eq!(result["outcome"], "sent", "{result}");
    let summary = client.wait_session(&id, "idle after the reply", |s| {
        s["state"] == "idle" && s["preview"].as_str().is_some_and(|p| p.contains("Hello there"))
    });
    assert_eq!(summary["last_outcome"], "completed");
    assert_eq!(env.prompts_sent(&id), ["hi"]);
}
