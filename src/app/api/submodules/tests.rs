use super::*;
use crate::api::schema::{EmptyParams, SubmoduleOpenParams, SuccessResponse};
use crate::submodule::tests::Fixture;

pub(super) fn app() -> App {
    let (_, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        rx,
        crate::api::EventHub::default(),
    );
    #[cfg(windows)]
    {
        app.state.default_shell = "C:\\Windows\\System32\\cmd.exe".into();
    }
    #[cfg(unix)]
    {
        app.state.default_shell = "/bin/sh".into();
    }
    app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    app
}

pub(super) fn complete(app: &mut App, method: Method) -> ResponseResult {
    let (tx, rx) = std::sync::mpsc::channel();
    app.start_submodule_request(
        Request {
            id: "test-submodule".into(),
            method,
        },
        tx,
        false,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(response) = rx.try_recv() {
            return serde_json::from_str::<SuccessResponse>(&response)
                .unwrap_or_else(|e| panic!("{e}: {response}"))
                .result;
        }
        if let Ok(event) = app.event_rx.try_recv() {
            app.handle_internal_event(event);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "submodule request timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[tokio::test]
async fn submodule_open_deduplicates_and_survives_parent_closure() {
    let fixture = Fixture::new();
    let _state_dir = TestStateDir::new(&fixture.root);
    let mut app = app();
    let parent_idx = app
        .create_workspace_with_options(fixture.parent.clone(), false)
        .unwrap();
    let parent_id = app.public_workspace_id(parent_idx);
    let params = SubmoduleOpenParams {
        workspace_id: Some(parent_id.clone()),
        cwd: None,
        path: "nested/child module".into(),
        focus: false,
        share_skills: false,
    };
    let first = complete(&mut app, Method::SubmoduleOpen(params.clone()));
    let count = app.state.workspaces.len();
    let mut reopening = params;
    reopening.share_skills = true;
    assert_eq!(first, complete(&mut app, Method::SubmoduleOpen(reopening)));
    assert!(!app.submodule_contexts()[0].sharing_enabled);
    assert_eq!(count, app.state.workspaces.len());
    app.state.assert_invariants_for_test();
    let ResponseResult::SubmoduleOpened { workspace_id } = first else {
        panic!("open response");
    };
    let idx = app.parse_workspace_id(&workspace_id).unwrap();
    let retained = app.state.workspaces[idx].submodule_context.clone().unwrap();
    let response = app.handle_api_request(Request {
        id: "close-parent".into(),
        method: Method::WorkspaceClose(crate::api::schema::WorkspaceCloseParams {
            workspace_id: parent_id,
            close_group: false,
        }),
    });
    assert!(!response.contains("\"error\""), "{response}");
    app.state.assert_invariants_for_test();
    let contexts = app.submodule_contexts();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].parent_workspace_id, None);
    assert_eq!(
        contexts[0].parent_path,
        retained.parent_path.display().to_string()
    );
    app.create_workspace_with_options(fixture.parent.clone(), false)
        .unwrap();
    assert!(app.submodule_contexts()[0].parent_workspace_id.is_some());
    let response = app.handle_api_request(Request {
        id: "contexts".into(),
        method: Method::SubmoduleContexts(EmptyParams {}),
    });
    assert!(response.contains("submodule_contexts"));
}

#[tokio::test]
async fn submodule_worktree_create_retains_context_when_source_closes_during_deferred_work() {
    let fixture = Fixture::new();
    let skill = fixture.parent.join(".agents/skills/parent-marker");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "fixture skill").unwrap();
    let _state_dir = TestStateDir::new(&fixture.root);
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
    let checkout = fixture.root.join("external worktree");
    let (tx, rx) = std::sync::mpsc::channel();
    assert!(app.handle_deferred_worktree_api_request(
        Request {
            id: "create".into(),
            method: Method::WorktreeCreate(crate::api::schema::WorktreeCreateParams {
                workspace_id: Some(workspace_id.clone()),
                branch: Some("context-test".into()),
                path: Some(checkout.display().to_string()),
                ..Default::default()
            })
        },
        tx,
        false
    ));
    // Close directly before consuming any completion event, even if Git already finished.
    let closed = app.handle_workspace_close(
        "close".into(),
        crate::api::schema::WorkspaceCloseParams {
            workspace_id: workspace_id.clone(),
            close_group: false,
        },
    );
    assert!(app.parse_workspace_id(&workspace_id).is_none());
    assert!(!closed.contains("\"error\""), "{closed}");
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
    let ResponseResult::WorktreeCreated { workspace, .. } =
        serde_json::from_str::<SuccessResponse>(&response)
            .unwrap()
            .result
    else {
        panic!("created: {response}");
    };
    let idx = app.parse_workspace_id(&workspace.workspace_id).unwrap();
    let context = app.state.workspaces[idx]
        .submodule_context
        .as_ref()
        .unwrap();
    assert_eq!(context.parent_path, fixture.parent);
    assert_eq!(context.submodule_path, "nested/child module");
    assert_eq!(context.status, "ready");
    assert_eq!(
        std::fs::read_to_string(checkout.join(".agents/skills/parent-marker/SKILL.md")).unwrap(),
        "fixture skill"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join(".claude/skills/parent-marker/SKILL.md")).unwrap(),
        "fixture skill"
    );
    app.state.assert_invariants_for_test();
    let refreshed = complete(
        &mut app,
        Method::SubmoduleContextRefresh(crate::api::schema::SubmoduleContextParams {
            workspace_id: workspace.workspace_id,
            enabled: Some(false),
            codex_sources: None,
            claude_sources: None,
        }),
    );
    assert!(matches!(refreshed, ResponseResult::SubmoduleContext { .. }));
    assert!(app
        .submodule_contexts()
        .iter()
        .all(|context| !context.sharing_enabled));
    assert!(!checkout.join(".agents/skills/parent-marker").exists());
    assert!(!fixture.child.join(".agents/skills/parent-marker").exists());
}

pub(super) struct TestStateDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}
impl TestStateDir {
    pub(super) fn new(path: &std::path::Path) -> Self {
        let lock = crate::config::test_config_env_lock().lock().unwrap();
        let previous = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", path.join("state"));
        Self {
            _lock: lock,
            previous,
        }
    }
}
impl Drop for TestStateDir {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}
