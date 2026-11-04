//! A session with two workspaces on two machines: the agent reads, edits, writes, searches and
//! runs commands on both hosts with `machine:path`; named shells keep their state; the session
//! is Unassigned until a task links it.

use controlroom_e2e::script::{bash, edit, host_call, say, scenario, tool};
use controlroom_e2e::world::World;
use serde_json::json;
use sha2::{Digest, Sha256};

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workspaces_on_two_machines() {
    let mut world = World::start().await;
    let api = world.devbox.dir("src/api");
    std::fs::create_dir_all(api.join("src")).unwrap();
    let main_rs = "fn main() {\n    println!(\"hi\");\n}\n";
    std::fs::write(api.join("src/main.rs"), main_rs).unwrap();
    std::fs::write(world.devbox.home().join("src/AGENTS.md"), "Be brief.\n").unwrap();
    std::fs::write(api.join("AGENTS.md"), "Run the tests.\n").unwrap();
    let web = world.buildbox.dir("work/web");
    std::fs::write(web.join("package.json"), "{}\n").unwrap();

    let new_main = "fn main() {\n    println!(\"hello\");\n}\n";
    world.agentd.scenario(&scenario(vec![
        tool("c_info", "info", &[("session.info", json!({}))]),
        host_call(
            "c_read",
            "read",
            "devbox",
            json!({"op": "read", "path": "src/main.rs"}),
        ),
        edit(
            "c_edit",
            "devbox",
            "~/src/api/src/main.rs",
            new_main,
            &sha256(main_rs),
        ),
        host_call(
            "c_write",
            "write",
            "buildbox",
            json!({"op": "write", "path": "notes.md", "content": "notes\n"}),
        ),
        host_call(
            "c_grep",
            "grep",
            "devbox",
            json!({"op": "grep", "pattern": "println", "path": "src"}),
        ),
        host_call("c_ls", "ls", "buildbox", json!({"op": "ls"})),
        bash(
            "c_sh1",
            "devbox",
            "devbox",
            "cd src && export GREETING=hi && greet() { echo \"hello $1\"; }",
        ),
        bash(
            "c_sh2",
            "devbox",
            "devbox",
            "pwd; echo $GREETING; greet there",
        ),
        tool(
            "c_new",
            "shell_new",
            &[(
                "shell.open",
                json!({"name": "devbox-2", "machine": "devbox"}),
            )],
        ),
        bash("c_sh3", "devbox", "devbox-2", "pwd; echo \"[$GREETING]\""),
        bash("c_sh4", "buildbox", "buildbox", "cd /tmp && export X=7"),
        bash("c_sh5", "buildbox", "buildbox", "pwd; echo $X; exit 3"),
        bash("c_sh6", "buildbox", "buildbox", "pwd; echo \"[$X]\""),
        tool("c_list", "shell_list", &[("shell.list", json!({}))]),
        say("Done."),
    ]));

    // A project and a task; the session starts Unassigned.
    let project = world
        .ui
        .ok(
            "projects.create",
            json!({"name": "api", "color": "#3366ff"}),
        )
        .await;
    let task = world
        .ui
        .ok(
            "tasks.create",
            json!({"title": "ship it", "project_ids": [project["id"]]}),
        )
        .await;
    let (devbox, buildbox) = (world.devbox.id.clone(), world.buildbox.id.clone());
    let id = world
        .create_session(
            &[(&devbox, "~/src/api"), (&buildbox, "~/work/web")],
            json!({}),
        )
        .await;
    let summary = world.session(&id);
    assert_eq!(summary["project_ids"], json!([]), "Unassigned: {summary}");
    let roots: Vec<_> = summary["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["machine_id"].clone(),
                w["root"].clone(),
                w["primary"].clone(),
            )
        })
        .collect();
    assert_eq!(
        roots,
        [
            (json!(devbox), json!(api.to_str().unwrap()), json!(true)),
            (json!(buildbox), json!(web.to_str().unwrap()), json!(false)),
        ]
    );
    let shells: Vec<_> = summary["shells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(shells, [json!("devbox"), json!("buildbox")]);

    // Linking the task puts the session in its project.
    world
        .ui
        .ok(
            "tasks.link",
            json!({"task_id": task["id"], "session_id": id}),
        )
        .await;
    world
        .wait_session(&id, "in the project", |s| {
            s["project_ids"] == json!([project["id"]])
        })
        .await;

    world.prompt(&id, "work on both machines").await;
    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "every tool call ended", |t| {
            t.tool_done("c_list")
        })
        .await;
    for call in [
        "c_info", "c_read", "c_edit", "c_write", "c_grep", "c_ls", "c_sh1", "c_sh2", "c_new",
        "c_sh3", "c_sh4", "c_sh6", "c_list",
    ] {
        assert!(
            !timeline.tool_failed(call),
            "{call}: {}",
            timeline.tool(call).unwrap()
        );
    }

    // session.info: both workspaces with their instruction files.
    let info = timeline.tool_json("c_info");
    let machines: Vec<_> = info["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["machine"].clone(), w["shell"].clone()))
        .collect();
    assert_eq!(
        machines,
        [
            (json!("devbox"), json!("devbox")),
            (json!("buildbox"), json!("buildbox"))
        ]
    );
    let context: Vec<String> = info["workspaces"][0]["context_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["content"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(context, ["Be brief.\n", "Run the tests.\n"]);

    // Files: read relative to the workspace root, the edit's compare-and-swap write, a write
    // on the other machine, grep and ls.
    let read = timeline.tool_json("c_read");
    assert_eq!(
        read["result"]["content"]["lines"][1],
        "    println!(\"hi\");"
    );
    assert_eq!(read["link_drops"], 0);
    assert_eq!(
        std::fs::read_to_string(api.join("src/main.rs")).unwrap(),
        new_main
    );
    assert_eq!(
        timeline.tool_json("c_edit")["result"]["sha256"],
        sha256(new_main)
    );
    assert_eq!(
        std::fs::read_to_string(web.join("notes.md")).unwrap(),
        "notes\n"
    );
    let grep = timeline.tool_json("c_grep");
    assert_eq!(grep["result"]["matches"], 1, "{grep}");
    assert_eq!(
        grep["result"]["lines"][0]["text"],
        "    println!(\"hello\");"
    );
    let ls = timeline.tool_json("c_ls");
    let mut names: Vec<_> = ls["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    assert!(
        names.contains(&"notes.md".to_owned()) && names.contains(&"package.json".to_owned()),
        "{names:?}"
    );

    // Shells keep their state; a second shell on the same workspace is independent.
    let sh2 = &timeline.tool_json("c_sh2")["result"];
    assert_eq!(
        sh2["output"],
        format!("{}\nhi\nhello there", api.join("src").display())
    );
    assert_eq!(sh2["cwd"], api.join("src").to_str().unwrap());
    let sh3 = &timeline.tool_json("c_sh3")["result"];
    assert_eq!(sh3["output"], format!("{}\n[]", api.display()));
    let sh5 = &timeline.tool_json("c_sh5")["result"];
    assert_eq!(
        (sh5["output"].clone(), sh5["exit_code"].clone()),
        (json!("/tmp\n7"), json!(3))
    );
    // `exit` ends the shell: the next command runs in a fresh shell in the same directory.
    let sh6 = &timeline.tool_json("c_sh6")["result"];
    assert_eq!(sh6["output"], "/tmp\n[]");
    assert!(
        sh6["shell_reset"]
            .as_str()
            .is_some_and(|r| r.contains("status 3")),
        "{sh6}"
    );
    let listed: Vec<_> = timeline.tool_json("c_list")["shells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["machine"].clone(), s["name"].clone()))
        .collect();
    assert_eq!(
        listed,
        [
            (json!("devbox"), json!("devbox")),
            (json!("buildbox"), json!("buildbox")),
            (json!("devbox"), json!("devbox-2")),
        ]
    );
}
