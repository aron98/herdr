use super::tests::{app, complete, TestStateDir};
use super::*;
use crate::api::schema::{SubmoduleOpenParams, SuccessResponse};
use crate::submodule::tests::{git, Fixture};

#[tokio::test]
async fn submodule_external_worktree_open_prepares_context_before_response() {
    let fixture = Fixture::new();
    let _state_dir = TestStateDir::new(&fixture.root);
    let skill = fixture.parent.join(".agents/skills/open-marker");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "open skill").unwrap();
    let mut app = app();
    let ResponseResult::SubmoduleOpened { workspace_id } = complete(
        &mut app,
        Method::SubmoduleOpen(SubmoduleOpenParams {
            workspace_id: None,
            cwd: Some(fixture.parent.display().to_string()),
            path: "nested/child module".into(),
            focus: false,
            share_skills: true,
        }),
    ) else {
        panic!("opened");
    };
    assert!(app.parse_workspace_id(&workspace_id).is_some());
    let nested_cwd = fixture.child.join("nested cwd");
    std::fs::create_dir(&nested_cwd).unwrap();
    let checkout = fixture.root.join("existing outside");
    git(
        &fixture.child,
        &["worktree", "add", "--detach", checkout.to_str().unwrap()],
    );
    let (tx, rx) = std::sync::mpsc::channel();
    app.handle_deferred_worktree_api_request(
        Request {
            id: "open-external".into(),
            method: Method::WorktreeOpen(crate::api::schema::WorktreeOpenParams {
                cwd: Some(nested_cwd.display().to_string()),
                path: Some(checkout.display().to_string()),
                ..Default::default()
            }),
        },
        tx,
        false,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let response = loop {
        if let Ok(response) = rx.try_recv() {
            break response;
        }
        if let Ok(event) = app.event_rx.try_recv() {
            app.handle_internal_event(event);
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let ResponseResult::WorktreeOpened { workspace, .. } =
        serde_json::from_str::<SuccessResponse>(&response)
            .unwrap()
            .result
    else {
        panic!("opened: {response}");
    };
    let idx = app.parse_workspace_id(&workspace.workspace_id).unwrap();
    let context = app.state.workspaces[idx]
        .submodule_context
        .as_ref()
        .unwrap();
    assert_eq!(context.parent_path, fixture.parent);
    assert_eq!(context.status, "ready");
    assert_eq!(
        std::fs::read_to_string(checkout.join(".agents/skills/open-marker/SKILL.md")).unwrap(),
        "open skill"
    );
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn submodule_restore_refreshes_more_contexts_than_worker_slots() {
    let fixture = Fixture::new();
    let _state_dir = TestStateDir::new(&fixture.root);
    let mut app = app();
    let mut context = crate::submodule::select(&fixture.parent, "nested/child module").unwrap();
    context.sharing_enabled = false;
    app.state.workspaces = (0..12)
        .map(|_| {
            let mut ws = crate::workspace::Workspace::test_new("restored");
            ws.submodule_context = Some(context.clone());
            ws
        })
        .collect();
    app.refresh_restored_submodules();
    assert!(app
        .submodule_contexts()
        .iter()
        .all(|c| c.status == "pending"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app
        .submodule_contexts()
        .iter()
        .any(|c| c.status == "pending")
    {
        if let Ok(event) = app.event_rx.try_recv() {
            app.handle_internal_event(event);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "restore did not refresh every context"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app
        .submodule_contexts()
        .iter()
        .all(|c| c.status == "disabled"));
}

#[tokio::test]
async fn submodule_mutation_lease_rejects_overlap_without_blocking_discovery() {
    let fixture = Fixture::new();
    let _state_dir = TestStateDir::new(&fixture.root);
    let mut app = app();
    let permit = crate::submodule::preparation_slot().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let params = SubmoduleOpenParams {
        workspace_id: None,
        cwd: Some(fixture.parent.display().to_string()),
        path: "nested/child module".into(),
        focus: false,
        share_skills: true,
    };
    app.start_submodule_request(
        Request {
            id: "overlap".into(),
            method: Method::SubmoduleOpen(params.clone()),
        },
        tx,
        false,
    );
    let response = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
    assert!(error.error.message.contains("operation is pending"));
    assert!(matches!(
        complete(
            &mut app,
            Method::SubmoduleList(crate::api::schema::SubmoduleListParams {
                workspace_id: None,
                cwd: params.cwd.clone()
            })
        ),
        ResponseResult::SubmoduleList { .. }
    ));
    drop(permit);
    assert!(matches!(
        complete(&mut app, Method::SubmoduleOpen(params)),
        ResponseResult::SubmoduleOpened { .. }
    ));
    assert!(
        crate::submodule::preparation_slot().is_ok(),
        "completion must release mutation lease"
    );
}
