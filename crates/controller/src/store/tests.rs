use std::sync::{Arc, Barrier};

use controlroom_protocol::UnixMillis;
use controlroom_protocol::host::{BootStartMethod, CheckStatus, DoctorCheck, DoctorReport};
use controlroom_protocol::ids::{DeviceId, KeyId, MachineId, SessionId};
use controlroom_protocol::session::SandboxLevel;
use controlroom_protocol::ui::{
    AuthMode, ContactScope, Priority, ProfileChange, ProfileScope, Task, TaskStatus,
};

use super::*;

fn store() -> (Store, OwnerId) {
    let store = Store::open_in_memory().unwrap();
    let owner = OwnerId::local();
    store.ensure_owner(&owner).unwrap();
    (store, owner)
}

fn key(name: &str, kind: KeyKind) -> NewKey {
    NewKey {
        id: KeyId::generate(),
        name: name.into(),
        kind,
        generated: true,
        public_key: format!("ssh-ed25519 AAAA{name}"),
        fingerprint: format!("SHA256:{name}"),
        sealed_private_key: vec![1, 2, 3],
    }
}

fn machine(store: &Store, owner: &OwnerId, name: &str, port: u16) -> MachineRow {
    store
        .owner(owner)
        .machines()
        .insert(NewMachineRow {
            name: name.into(),
            host: "devbox.example.com".into(),
            port,
            username: "alice".into(),
            key_id: None,
            auth_key_id: None,
            host_key: "ssh-ed25519 AAAAhost".into(),
        })
        .unwrap()
}

fn doctor(version: &str) -> DoctorReport {
    let check = DoctorCheck {
        status: CheckStatus::Ok,
        value: None,
        detail: String::new(),
    };
    DoctorReport {
        daemon_version: version.into(),
        user: Some("alice".into()),
        persistence: check.clone(),
        boot_start: BootStartMethod::Lazy,
        boot_start_check: check.clone(),
        nono: check,
        landlock_abi: None,
        agents_config: None,
        agents_config_error: None,
        agents: vec![],
        checked_at: UnixMillis(1),
    }
}

#[test]
fn a_database_with_another_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("controller.sqlite");
    Store::open(&path).unwrap();
    // Reopening the same version works.
    Store::open(&path).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    let err = Store::open(&path).err().unwrap();
    assert!(err.to_string().contains("delete the database"), "{err}");
}

#[test]
fn controller_id_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("controller.sqlite");
    let first = Store::open(&path).unwrap().controller_id().unwrap();
    assert_eq!(Store::open(&path).unwrap().controller_id().unwrap(), first);
}

#[test]
fn rows_are_scoped_to_their_owner() {
    let (store, alice) = store();
    let bob = OwnerId::new("bob");
    store.ensure_owner(&bob).unwrap();
    let m = machine(&store, &alice, "devbox", 22);
    store
        .owner(&alice)
        .projects()
        .insert("app", "#fff")
        .unwrap();
    assert!(store.owner(&bob).machines().get(m.id).unwrap().is_none());
    assert!(store.owner(&bob).machines().list().unwrap().is_empty());
    assert!(!store.owner(&bob).machines().set_name(m.id, "x").unwrap());
    assert!(!store.owner(&bob).machines().delete(m.id).unwrap());
    assert!(store.owner(&bob).projects().list().unwrap().is_empty());
    // The same host can be added once per owner.
    machine(&store, &bob, "devbox", 22);
    assert_eq!(store.owner_ids().unwrap().len(), 2);
}

#[test]
fn duplicate_machines_and_names_conflict() {
    let (store, owner) = store();
    machine(&store, &owner, "devbox", 22);
    let repo = store.owner(&owner);
    let dup = repo.machines().insert(NewMachineRow {
        name: "other".into(),
        host: "devbox.example.com".into(),
        port: 22,
        username: "alice".into(),
        key_id: None,
        auth_key_id: None,
        host_key: "k".into(),
    });
    assert!(matches!(dup, Err(StoreError::Conflict(_))), "{dup:?}");
    let second = machine(&store, &owner, "second", 2222);
    assert!(matches!(
        repo.machines().set_name(second.id, "devbox"),
        Err(StoreError::Conflict(_))
    ));
    assert!(
        repo.machines()
            .find("devbox.example.com", 2222, "alice")
            .unwrap()
            .is_some()
    );
}

/// The prototype's lost update: a daemon-reported write and a UI settings write raced, each
/// rewriting the whole row. Writes are column-targeted now, so any interleaving keeps both.
#[test]
fn concurrent_column_writes_never_lose_each_other() {
    let (store, owner) = store();
    let m = machine(&store, &owner, "devbox", 22);
    const ROUNDS: usize = 200;
    let barrier = Arc::new(Barrier::new(4));
    let writers: Vec<_> = (0..4)
        .map(|writer| {
            let (store, owner, barrier) = (store.clone(), owner.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                let machines = store.owner(&owner);
                let machines = machines.machines();
                for round in 0..ROUNDS {
                    match writer {
                        0 => machines.set_name(m.id, &format!("name-{round}")).map(drop),
                        1 => machines.set_grace(m.id, round as u32).map(drop),
                        2 => machines.set_auto_upgrade(m.id, round % 2 == 1).map(drop),
                        _ => machines
                            .set_doctor(m.id, &doctor(&format!("0.0.{round}")))
                            .map(drop),
                    }
                    .unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    let last = ROUNDS - 1;
    let row = store.owner(&owner).machines().get(m.id).unwrap().unwrap();
    assert_eq!(row.name, format!("name-{last}"));
    assert_eq!(row.grace_s, last as u32);
    assert!(row.auto_upgrade);
    assert_eq!(row.doctor.unwrap().daemon_version, format!("0.0.{last}"));
    // Untouched columns kept their values.
    assert_eq!(row.host_key.as_deref(), Some("ssh-ed25519 AAAAhost"));
}

#[test]
fn rotation_retires_and_prunes_only_unreferenced_keys() {
    let (store, owner) = store();
    let repo = store.owner(&owner);
    let old = repo
        .keys()
        .insert(key("old", KeyKind::Controlroom))
        .unwrap();
    let on_old = machine(&store, &owner, "a", 22);
    let fresh = key("new", KeyKind::Controlroom);
    let new_id = fresh.id;
    repo.keys().rotate_controlroom(old.id, fresh).unwrap();
    assert_eq!(repo.keys().controlroom().unwrap().unwrap().id, new_id);
    // The machine that never recorded its key was pinned to the old one, so it survives.
    let row = repo.machines().get(on_old.id).unwrap().unwrap();
    assert_eq!(row.auth_key_id, Some(old.id));
    assert_eq!(repo.keys().prune_retired().unwrap(), 0);
    assert!(
        repo.keys()
            .get(old.id)
            .unwrap()
            .unwrap()
            .retired_at
            .is_some()
    );
    // Once it migrated, the old key goes.
    repo.machines()
        .set_auth_key(on_old.id, Some(new_id))
        .unwrap();
    assert_eq!(repo.keys().prune_retired().unwrap(), 1);
    assert!(repo.keys().get(old.id).unwrap().is_none());
    // The current key is never pruned.
    repo.machines().set_auth_key(on_old.id, None).unwrap();
    assert_eq!(repo.keys().prune_retired().unwrap(), 0);
}

#[test]
fn additional_keys_in_use_cannot_be_deleted() {
    let (store, owner) = store();
    let repo = store.owner(&owner);
    let extra = repo
        .keys()
        .insert(key("extra", KeyKind::Additional))
        .unwrap();
    let m = machine(&store, &owner, "a", 22);
    repo.machines().set_key(m.id, Some(extra.id)).unwrap();
    assert!(repo.keys().delete(extra.id).is_err());
    repo.machines().set_key(m.id, None).unwrap();
    assert!(repo.keys().delete(extra.id).unwrap());
}

#[test]
fn tasks_with_checklist_projects_and_dangling_links() {
    let (store, owner) = store();
    let repo = store.owner(&owner);
    let p1 = repo.projects().insert("one", "#111").unwrap();
    let p2 = repo.projects().insert("two", "#222").unwrap();
    let (s1, s2) = (SessionId::generate(), SessionId::generate());
    let task = repo
        .tasks()
        .insert(tasks::NewTask {
            title: "ship".into(),
            status: TaskStatus::Backlog,
            order: None,
            priority: Priority::P1,
            project_ids: vec![p2.id, p1.id],
            notes: String::new(),
            checklist: vec!["a".into(), "c".into()],
            links: vec![s1, s2],
        })
        .unwrap();
    assert_eq!(task.project_ids, [p2.id, p1.id]);
    assert_eq!(task.links, [s1, s2]);
    let texts = |t: &Task| {
        t.checklist
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(&task), ["a", "c"]);

    // Insert between, tick one entry, remove another: only those rows change.
    assert!(repo.tasks().checklist_add(task.id, "b", Some(1)).unwrap());
    let task = repo.tasks().get(task.id).unwrap().unwrap();
    assert_eq!(texts(&task), ["a", "b", "c"]);
    let b = task.checklist[1].id;
    assert!(
        repo.tasks()
            .checklist_update(task.id, b, None, Some(true))
            .unwrap()
    );
    assert!(
        repo.tasks()
            .checklist_remove(task.id, task.checklist[0].id)
            .unwrap()
    );
    let task = repo.tasks().get(task.id).unwrap().unwrap();
    assert_eq!(texts(&task), ["b", "c"]);
    assert!(task.checklist[0].done);

    // Deleting a project unassigns it; links survive anything but the task's deletion.
    assert!(repo.projects().delete(p2.id).unwrap());
    let task = repo.tasks().get(task.id).unwrap().unwrap();
    assert_eq!(task.project_ids, [p1.id]);
    assert_eq!(repo.tasks().linked_to(s1).unwrap(), [task.id]);
    // Moving only from the given columns.
    assert!(
        repo.tasks()
            .move_if_in(
                task.id,
                &[TaskStatus::Backlog, TaskStatus::Todo],
                TaskStatus::Doing
            )
            .unwrap()
    );
    assert!(
        !repo
            .tasks()
            .move_if_in(task.id, &[TaskStatus::Backlog], TaskStatus::Doing)
            .unwrap()
    );
    assert_eq!(
        repo.tasks().get(task.id).unwrap().unwrap().status,
        TaskStatus::Doing
    );
    assert!(repo.tasks().delete(task.id).unwrap());
    assert!(repo.tasks().linked_to(s2).unwrap().is_empty());
}

#[test]
fn devices_sessions_and_push_cascade_on_removal() {
    let (store, owner) = store();
    let repo = store.owner(&owner);
    let phone = DeviceId::new("phone");
    let laptop = DeviceId::new("laptop");
    repo.devices().register(&phone, "Phone").unwrap();
    // Registering again keeps the server's label.
    assert_eq!(
        repo.devices().register(&phone, "Other").unwrap().label,
        "Phone"
    );
    repo.devices().register(&laptop, "Laptop").unwrap();
    let session = |hash: &str, device: &DeviceId| NewAuthSession {
        token_hash: hash.into(),
        mode: AuthMode::Oidc,
        subject: "alice".into(),
        groups: vec!["staff".into()],
        name: None,
        email: None,
        device_id: Some(device.clone()),
        csrf_token: "csrf".into(),
        expires_at: UnixMillis(i64::MAX),
    };
    repo.auth_sessions().create(session("h1", &phone)).unwrap();
    repo.auth_sessions().create(session("h2", &laptop)).unwrap();
    assert!(
        repo.push()
            .set(&PushSubscriptionRow {
                device_id: phone.clone(),
                endpoint: "https://push.example.com/1".into(),
                p256dh: "p".into(),
                auth: "a".into(),
            })
            .unwrap()
    );
    assert!(repo.devices().get(&phone).unwrap().unwrap().push);
    let delivery = Delivery {
        device_id: &phone,
        session_id: SessionId::generate(),
        event_key: "approval:r1",
    };
    assert!(repo.push().claim(delivery).unwrap());
    assert!(!repo.push().claim(delivery).unwrap(), "deduplicated");
    repo.push().release(delivery).unwrap();
    assert!(
        repo.push().claim(delivery).unwrap(),
        "released after a failure"
    );

    assert!(repo.devices().remove(&phone).unwrap());
    let lookup = store.auth_sessions();
    assert!(lookup.get("h1", UnixMillis(0)).unwrap().is_none());
    assert!(lookup.get("h2", UnixMillis(0)).unwrap().is_some());
    assert!(repo.push().get(&phone).unwrap().is_none());
    // An unknown device can neither subscribe nor be claimed.
    assert!(!repo.push().claim(delivery).unwrap());
}

#[test]
fn auth_sessions_slide_and_expire() {
    let (store, owner) = store();
    let repo = store.owner(&owner);
    repo.auth_sessions()
        .create(NewAuthSession {
            token_hash: "h".into(),
            mode: AuthMode::DevToken,
            subject: "dev".into(),
            groups: vec![],
            name: None,
            email: None,
            device_id: None,
            csrf_token: "c".into(),
            expires_at: UnixMillis(100),
        })
        .unwrap();
    let lookup = store.auth_sessions();
    let touched = lookup
        .touch("h", UnixMillis(50), UnixMillis(500))
        .unwrap()
        .unwrap();
    assert_eq!(touched.expires_at, UnixMillis(500));
    assert_eq!(touched.mode, AuthMode::DevToken);
    assert!(
        lookup
            .touch("h", UnixMillis(600), UnixMillis(900))
            .unwrap()
            .is_none()
    );
    assert_eq!(lookup.purge_expired(UnixMillis(600)).unwrap(), 1);
}

#[test]
fn sandbox_profiles_and_grants() {
    let (store, owner) = store();
    let repo = store.owner(&owner);
    let m = MachineId::generate();
    let scope = ProfileScope::Machine { machine_id: m };
    repo.sandbox_profiles()
        .change(
            &scope,
            &ProfileChange::SetLevel {
                level: SandboxLevel::Off,
            },
            None,
        )
        .unwrap();
    repo.sandbox_profiles()
        .change(
            &scope,
            &ProfileChange::AddDomain {
                domain: "registry.example.com".into(),
            },
            None,
        )
        .unwrap();
    let profiles = repo.sandbox_profiles().list().unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].scope, scope);
    assert_eq!(profiles[0].level, Some(SandboxLevel::Off));
    assert_eq!(profiles[0].domains, ["registry.example.com"]);
    // A profile without settings disappears.
    repo.sandbox_profiles()
        .change(&scope, &ProfileChange::ClearLevel, None)
        .unwrap();
    repo.sandbox_profiles()
        .change(
            &scope,
            &ProfileChange::RemoveDomain {
                domain: "registry.example.com".into(),
            },
            None,
        )
        .unwrap();
    assert!(repo.sandbox_profiles().list().unwrap().is_empty());
    // The never list is stored with the profile.
    let never = ProfileChange::AddNever {
        path: "~/.bash_history".into(),
    };
    repo.sandbox_profiles()
        .change(&scope, &never, None)
        .unwrap();
    assert_eq!(
        repo.sandbox_profiles().list().unwrap()[0].never,
        ["~/.bash_history"]
    );

    let a = SessionId::generate();
    let grant = repo
        .grants()
        .insert(a, a, ContactScope::Pair, None)
        .unwrap();
    assert_eq!(repo.grants().list().unwrap(), std::slice::from_ref(&grant));
    assert!(repo.grants().delete(grant.id).unwrap());
    assert!(!repo.grants().delete(grant.id).unwrap());
}

/// Lists in creation order must not depend on id order: ids made in the same millisecond
/// aren't ordered, and the feed's project membership follows this order (a flaky e2e test).
#[test]
fn projects_created_in_the_same_millisecond_keep_their_insertion_order() {
    let (store, owner) = store();
    // Same created_at, ids deliberately descending.
    let ids = [
        "01J00000000000000000000009",
        "01J00000000000000000000008",
        "01J00000000000000000000007",
    ];
    for (i, id) in ids.iter().enumerate() {
        store
            .lock()
            .execute(
                "INSERT INTO projects (id, owner_id, name, color, created_at)
                 VALUES (?1, ?2, ?3, '#000000', 1)",
                rusqlite::params![id, owner.as_str(), format!("p{i}")],
            )
            .unwrap();
    }
    let names: Vec<String> = store
        .owner(&owner)
        .projects()
        .list()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["p0", "p1", "p2"]);
}

/// A version-5 database (before the never list) is upgraded in place, keeping its rows.
#[test]
fn a_version_5_database_is_upgraded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("controller.sqlite");
    let id = Store::open(&path).unwrap().controller_id().unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("ALTER TABLE sandbox_profiles DROP COLUMN never; PRAGMA user_version = 5;")
        .unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.controller_id().unwrap(), id);
    let owner = OwnerId::new("alice");
    store.ensure_owner(&owner).unwrap();
    let scope = ProfileScope::Machine {
        machine_id: MachineId::generate(),
    };
    let never = ProfileChange::AddNever {
        path: "/srv/secret".into(),
    };
    store
        .owner(&owner)
        .sandbox_profiles()
        .change(&scope, &never, None)
        .unwrap();
}
