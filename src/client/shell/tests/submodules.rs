use super::*;

#[test]
fn uninitialized_submodule_does_not_send_open_request() {
    // Given a registered checkout that has not been initialized.
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.overlay = Some(ClientShellOverlay::WorktreeOpen(
        ClientWorktreeOpenOverlay {
            submodules: true,
            source_workspace_id: "ws_1".into(),
            entries: vec![ClientWorktreeOpenEntry {
                initialized: false,
                path: "packages/rebuild".into(),
                label: "packages/rebuild".into(),
                branch: None,
                is_linked_worktree: false,
                is_detached: false,
                open_workspace_id: None,
            }],
            selected: 0,
            query: TextEditor::default(),
            search_focused: false,
            error: None,
            opening: false,
        },
    ));
    // When the selected entry is opened.
    let mut outcome = ClientShellInput::default();
    state.submit_worktree_open(&mut outcome);
    // Then initialization is required and no server mutation occurs.
    assert!(outcome.actions.is_empty());
    assert!(
        matches!(state.overlay, Some(ClientShellOverlay::WorktreeOpen(ref open)) if !open.opening && open.error.is_some())
    );
}

#[test]
fn metadata_requires_positive_capability_advertisement() {
    // Given an old endpoint with no advertised metadata API.
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    // When metadata collection runs.
    let actions = state.take_submodule_metadata_actions();
    // Then it does not probe an unsupported server.
    assert!(actions.is_empty());
}

fn metadata_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_endpoint_methods(Some(vec![
        "submodule.contexts".into(),
        "submodule.list".into(),
        "submodule.open".into(),
        "submodule.context.refresh".into(),
    ]));
    state
}

fn context(workspace: &str, parent: Option<&str>) -> crate::api::schema::SubmoduleContextInfo {
    crate::api::schema::SubmoduleContextInfo {
        workspace_id: workspace.into(),
        parent_workspace_id: parent.map(str::to_owned),
        parent_path: "/repo".into(),
        submodule_path: "rebuild".into(),
        checkout_path: "/repo/rebuild".into(),
        repo_key: "child-repo".into(),
        sharing_enabled: true,
        status: "ready".into(),
        messages: Vec::new(),
    }
}

fn metadata_request_id(actions: &[ClientShellAction]) -> String {
    actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.id.clone()),
            _ => None,
        })
        .expect("metadata request")
}

#[test]
fn metadata_coalesces_terminal_revisions_and_refetches_workspace_changes() {
    // Given an outstanding metadata request.
    let mut state = metadata_state();
    let request = metadata_request_id(&state.take_submodule_metadata_actions());
    let mut updated = snapshot();
    updated.revision += 1;
    state.set_snapshot(Box::new(updated.clone()));
    assert!(state.take_submodule_metadata_actions().is_empty());
    state.handle_endpoint_result(
        "boot-1",
        &request,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts { contexts: vec![] }),
    );
    assert!(state.take_submodule_metadata_actions().is_empty());
    // When a workspace is added, independent of terminal revisions.
    let mut child = updated.workspaces[0].clone();
    child.workspace_id = "ws_2".into();
    updated.workspaces.push(child);
    updated.revision += 1;
    state.set_snapshot(Box::new(updated));
    // Then exactly one metadata request is scheduled.
    assert_eq!(state.take_submodule_metadata_actions().len(), 1);
    assert!(state.take_submodule_metadata_actions().is_empty());
}

#[test]
fn metadata_from_previous_connection_is_discarded() {
    // Given a metadata request from a retired connection.
    let mut state = metadata_state();
    let request = metadata_request_id(&state.take_submodule_metadata_actions());
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 2, Box::new(snapshot()));
    // When that request arrives after reconnect, even with the same boot ID.
    state.handle_endpoint_result(
        "boot-1",
        &request,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
            contexts: vec![context("ws_1", None)],
        }),
    );
    // Then no metadata is applied to the new connection.
    assert!(state.endpoints[0].submodules.contexts.is_empty());
}

#[test]
fn background_metadata_stays_on_its_endpoint_with_colliding_boot_and_workspace_ids() {
    // Given two endpoints with identical server-local identities.
    let mut state = metadata_state();
    let profile = crate::client::endpoint::SavedSshEndpoint {
        id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    state.cache_endpoint_snapshot(&remote, Box::new(snapshot()));
    state.set_endpoint_methods_for(&remote, Some(vec!["submodule.contexts".into()]));
    let actions = state.take_submodule_metadata_actions();
    let request = actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint {
                endpoint_id,
                request,
                ..
            } if endpoint_id == &remote => Some(request.id.clone()),
            _ => None,
        })
        .unwrap();
    // When the inactive endpoint responds.
    state.handle_endpoint_result(
        "boot-1",
        &request,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
            contexts: vec![context("ws_1", None)],
        }),
    );
    // Then only that endpoint receives the relationship.
    assert!(state.endpoints[0].submodules.contexts.is_empty());
    assert_eq!(state.endpoints[1].submodules.contexts.len(), 1);
    assert!(state.overlay.is_none());
}

fn hierarchy_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let template = snapshot.workspaces[0].clone();
    snapshot.workspaces = ["parent", "worktree", "child"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let mut ws = template.clone();
            ws.workspace_id = id.into();
            ws.label = id.into();
            ws.number = index + 1;
            ws.focused = id == "parent";
            ws.worktree = Some(ClientShellWorktree {
                key: if id == "parent" {
                    "parent-repo"
                } else {
                    "child-repo"
                }
                .into(),
                label: id.into(),
                is_linked_worktree: id == "worktree",
            });
            ws
        })
        .collect();
    snapshot.focused_workspace_id = Some("parent".into());
    snapshot
}

#[test]
fn hierarchy_orders_external_worktree_beneath_submodule() {
    // Given shuffled workspace order and an explicitly associated external worktree.
    let snapshot = hierarchy_snapshot();
    let hierarchy = super::super::hierarchy::WorkspaceHierarchy::build(
        &snapshot,
        &[
            context("child", Some("parent")),
            context("worktree", Some("parent")),
        ],
    );
    // When projecting the sidebar.
    let entries = hierarchy.entries(&snapshot, &HashSet::new());
    // Then parent, submodule, and worktree occupy three nesting levels.
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                snapshot.workspaces[entry.index].workspace_id.as_str(),
                entry.depth,
                entry.submodule
            ))
            .collect::<Vec<_>>(),
        vec![
            ("parent", 0, false),
            ("child", 1, true),
            ("worktree", 2, false)
        ]
    );
}

#[test]
fn collapsed_parent_keeps_path_to_focused_descendant_visible() {
    // Given focus on a nested worktree beneath a collapsed pipeline.
    let mut snapshot = hierarchy_snapshot();
    snapshot
        .workspaces
        .iter_mut()
        .for_each(|ws| ws.focused = ws.workspace_id == "worktree");
    let hierarchy = super::super::hierarchy::WorkspaceHierarchy::build(
        &snapshot,
        &[
            context("child", Some("parent")),
            context("worktree", Some("parent")),
        ],
    );
    let collapsed = HashSet::from(["parent-repo".to_owned()]);
    // When the hierarchy is projected.
    let entries = hierarchy.entries(&snapshot, &collapsed);
    // Then every ancestor needed to understand the focus remains visible.
    assert_eq!(
        entries
            .iter()
            .map(|entry| snapshot.workspaces[entry.index].workspace_id.as_str())
            .collect::<Vec<_>>(),
        vec!["parent", "child", "worktree"]
    );
}

#[test]
fn parent_close_promotes_submodule_and_reopen_restores_grouping() {
    // Given retained context after the parent workspace closes.
    let complete = hierarchy_snapshot();
    let contexts = [
        context("child", Some("parent")),
        context("worktree", Some("parent")),
    ];
    let mut closed = complete.clone();
    closed.workspaces.retain(|ws| ws.workspace_id != "parent");
    let hierarchy = super::super::hierarchy::WorkspaceHierarchy::build(&closed, &contexts);
    assert_eq!(
        hierarchy
            .entries(&closed, &HashSet::new())
            .iter()
            .map(|entry| entry.depth)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    // When the same parent checkout is restored in the snapshot and metadata.
    let hierarchy = super::super::hierarchy::WorkspaceHierarchy::build(&complete, &contexts);
    // Then its surviving child and external worktree regroup.
    assert_eq!(
        hierarchy
            .entries(&complete, &HashSet::new())
            .iter()
            .map(|entry| entry.depth)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn initialized_submodule_picker_search_and_enter_open_registered_path() {
    // Given a picker containing two initialized registrations.
    let mut state = metadata_state();
    let mut outcome = ClientShellInput::default();
    state.handle_submodule_result(
        PendingEndpointKind::PrepareSubmoduleOpen {
            workspace_id: "ws_1".into(),
        },
        Ok(crate::api::schema::ResponseResult::SubmoduleList {
            parent_path: "/repo".into(),
            submodules: ["other", "packages/rebuild"]
                .into_iter()
                .map(|path| crate::api::schema::SubmoduleInfo {
                    path: path.into(),
                    checkout_path: format!("/repo/{path}"),
                    initialized: true,
                })
                .collect(),
        }),
        &mut outcome,
    );
    if let Some(ClientShellOverlay::WorktreeOpen(open)) = state.overlay.as_mut() {
        open.search_focused = true;
    }
    state.insert_worktree_overlay_text("rebuild");
    // When Enter is handled through the picker key path.
    let mut outcome = ClientShellInput::default();
    state.route_worktree_overlay_key(
        &crate::input::TerminalKey::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut outcome,
    );
    // Then the selected registration is opened with sharing and focus enabled.
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(&request.method, crate::api::schema::Method::SubmoduleOpen(params) if params.path == "packages/rebuild" && params.share_skills && params.focus))));
}

#[test]
fn parent_close_requests_only_parent_workspace() {
    // Given a pipeline workspace with a surviving submodule.
    let mut state = metadata_state();
    state.config.confirm_close = false;
    state.endpoints[0].submodules.contexts = vec![context("child", Some("ws_1"))];
    state.set_endpoint_methods(Some(vec![
        "submodule.contexts".into(),
        "workspace.close".into(),
    ]));
    // When Close is requested even with group intent.
    let mut outcome = ClientShellInput::default();
    state.request_workspace_close("ws_1".into(), Some(true), &mut outcome);
    // Then the server is asked to close only the parent.
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(&request.method, crate::api::schema::Method::WorkspaceClose(params) if params.workspace_id == "ws_1" && !params.close_group))));
}

#[test]
fn unsupported_endpoint_omits_submodule_action() {
    // Given a server advertising worktree support only.
    let mut state = metadata_state();
    state.set_endpoint_methods(Some(vec!["worktree.list".into()]));
    // When the workspace context menu is opened.
    state.open_workspace_context_menu("ws_1".into(), 1, 1);
    // Then only the optional unsupported action is omitted.
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("context menu")
    };
    assert!(!menu
        .items()
        .iter()
        .any(|item| item.action == ClientContextMenuAction::OpenSubmodule));
    assert!(menu
        .items()
        .iter()
        .any(|item| item.action == ClientContextMenuAction::OpenWorktree));
}

#[test]
fn stale_metadata_cannot_restore_enabled_state_after_disable() {
    // Given a metadata request predating an explicit disable operation.
    let mut state = metadata_state();
    let initial = metadata_request_id(&state.take_submodule_metadata_actions());
    state.handle_endpoint_result(
        "boot-1",
        &initial,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
            contexts: vec![context("ws_1", None)],
        }),
    );
    state.invalidate_submodule_metadata();
    let request = metadata_request_id(&state.take_submodule_metadata_actions());
    let mut outcome = ClientShellInput::default();
    state.refresh_submodule_context("ws_1".into(), true, &mut outcome);
    let mutation = metadata_request_id(&outcome.actions);
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
        if matches!(&request.method, crate::api::schema::Method::SubmoduleContextRefresh(params)
            if params.enabled == Some(false))
    )));
    let mut disabled = context("ws_1", None);
    disabled.sharing_enabled = false;
    disabled.status = "disabled".into();
    state.handle_endpoint_result(
        "boot-1",
        &mutation,
        Ok(crate::api::schema::ResponseResult::SubmoduleContext { context: disabled }),
    );
    // When the older response reports sharing as enabled.
    state.handle_endpoint_result(
        "boot-1",
        &request,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
            contexts: vec![context("ws_1", None)],
        }),
    );
    // Then the explicit mutation remains authoritative and a refresh is queued.
    assert!(!state.submodule_context("ws_1").unwrap().sharing_enabled);
    assert_eq!(state.take_submodule_metadata_actions().len(), 1);
}

#[test]
fn nested_sidebar_hits_and_navigation_follow_collapsed_projection() {
    // Given an expanded parent/submodule/worktree hierarchy.
    let mut state = metadata_state();
    let snapshot = hierarchy_snapshot();
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    let request = metadata_request_id(&state.take_submodule_metadata_actions());
    state.handle_endpoint_result(
        "boot-1",
        &request,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
            contexts: vec![
                context("child", Some("parent")),
                context("worktree", Some("parent")),
            ],
        }),
    );
    state.compose(106, 30).unwrap();
    assert_eq!(
        state
            .hits
            .workspaces
            .iter()
            .map(|hit| hit.workspace_id.as_str())
            .collect::<Vec<_>>(),
        vec!["parent", "child", "worktree"]
    );
    let (toggle, _) = state.hits.workspaces[1].group_toggle.as_ref().unwrap();
    // When the submodule collapse control is clicked.
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x,
        row: toggle.y,
        modifiers: KeyModifiers::NONE,
    })]);
    state.compose(106, 30).unwrap();
    // Then hit testing and keyboard navigation expose the same two visible workspaces.
    let visible = state
        .hits
        .workspaces
        .iter()
        .map(|hit| hit.workspace_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["parent", "child"]);
    let entries = state.navigation_workspace_entries(&snapshot);
    assert_eq!(
        entries
            .iter()
            .map(|entry| snapshot.workspaces[entry.index].workspace_id.as_str())
            .collect::<Vec<_>>(),
        visible
    );
}

#[test]
#[ignore = "manual fixed-geometry render scaling profile"]
fn submodule_render_scale_profile() {
    for count in [1, 15] {
        for grouped in [false, true] {
            let mut state = metadata_state();
            let mut projection = snapshot();
            let template = projection.workspaces[0].clone();
            let mut contexts = Vec::new();
            for index in 0..count {
                let mut child = template.clone();
                child.workspace_id = format!("child-{index}");
                child.label = format!("rebuild-{index}");
                child.focused = false;
                child.number = index + 2;
                child.worktree = Some(ClientShellWorktree {
                    key: format!("repo-{index}"),
                    label: child.label.clone(),
                    is_linked_worktree: false,
                });
                contexts.push(context(&child.workspace_id, Some("ws_1")));
                let mut pane = projection.panes[0].clone();
                pane.pane_id = format!("pane-child-{index}");
                pane.workspace_id = child.workspace_id.clone();
                pane.focused = false;
                projection.panes.push(pane);
                projection.workspaces.push(child);
            }
            state.set_snapshot(Box::new(projection));
            state.set_pane_surface(surface());
            let request = metadata_request_id(&state.take_submodule_metadata_actions());
            state.handle_endpoint_result(
                "boot-1",
                &request,
                Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
                    contexts: if grouped { contexts } else { Vec::new() },
                }),
            );
            for _ in 0..20 {
                std::hint::black_box(state.compose(160, 48).unwrap());
            }
            let started = std::time::Instant::now();
            for _ in 0..1000 {
                std::hint::black_box(state.compose(160, 48).unwrap());
            }
            eprintln!(
                "submodule-render: populated_children={count} grouped={grouped} viewport=160x48 us/frame={:.2}",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

#[test]
fn context_refresh_survives_switch_to_another_endpoint() {
    // Given a pending context mutation originating on Local.
    let mut state = metadata_state();
    let metadata = metadata_request_id(&state.take_submodule_metadata_actions());
    state.handle_endpoint_result(
        "boot-1",
        &metadata,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
            contexts: vec![context("ws_1", None)],
        }),
    );
    let mut outcome = ClientShellInput::default();
    state.refresh_submodule_context("ws_1".into(), true, &mut outcome);
    let request = metadata_request_id(&outcome.actions);
    let profile = crate::client::endpoint::SavedSshEndpoint {
        id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    state.cache_endpoint_snapshot(&remote, Box::new(snapshot()));
    assert!(state.activate_endpoint_projection(&remote));
    let mut disabled = context("ws_1", None);
    disabled.sharing_enabled = false;
    disabled.status = "disabled".into();
    // When Local finishes while the remote endpoint is active.
    state.handle_endpoint_result(
        "boot-1",
        &request,
        Ok(crate::api::schema::ResponseResult::SubmoduleContext { context: disabled }),
    );
    // Then Local is updated, siblings are scheduled for refresh, and the remote stays untouched.
    assert!(!state.endpoints[0].submodules.contexts[0].sharing_enabled);
    assert!(state.endpoints[1].submodules.contexts.is_empty());
    assert!(state
        .take_submodule_metadata_actions()
        .iter()
        .any(|action| matches!(
            action,
            ClientShellAction::Endpoint {
                endpoint_id: ClientEndpointId::Local,
                ..
            }
        )));
}

#[test]
fn submodule_owner_that_is_a_worktree_has_independent_collapse_key() {
    // Given a submodule owned by one linked pipeline checkout.
    let mut snapshot = hierarchy_snapshot();
    let mut owner = snapshot.workspaces[0].clone();
    owner.workspace_id = "pipeline-worktree".into();
    owner.worktree.as_mut().unwrap().is_linked_worktree = true;
    owner.focused = false;
    snapshot.workspaces.push(owner);
    let hierarchy = super::super::hierarchy::WorkspaceHierarchy::build(
        &snapshot,
        &[context("child", Some("pipeline-worktree"))],
    );
    // When the owner's nested group is collapsed.
    let key = hierarchy.group_key(3).unwrap();
    let entries = hierarchy.entries(&snapshot, &HashSet::from([key.to_owned()]));
    // Then the pipeline root stays expanded while the submodule descendants hide.
    assert_ne!(key, hierarchy.group_key(0).unwrap());
    assert_eq!(
        entries
            .iter()
            .map(|entry| snapshot.workspaces[entry.index].workspace_id.as_str())
            .collect::<Vec<_>>(),
        vec!["parent", "pipeline-worktree"]
    );
}

#[test]
fn collapsed_pipeline_reports_nested_worktree_attention() {
    // Given an idle pipeline with a blocked grandchild.
    let mut snapshot = hierarchy_snapshot();
    snapshot.workspaces[1].agent_status = AgentStatus::Blocked;
    let hierarchy = super::super::hierarchy::WorkspaceHierarchy::build(
        &snapshot,
        &[context("child", Some("parent"))],
    );
    // When the pipeline group is collapsed.
    let status = hierarchy.displayed_status(&snapshot, 0, &HashSet::from(["parent-repo".into()]));
    // Then the collapsed row preserves the descendant's actionable state.
    assert_eq!(status, AgentStatus::Blocked);
}

#[test]
fn metadata_refreshes_external_changes_on_bounded_deadline() {
    // Given a completed metadata request and unchanged workspace IDs.
    let mut state = metadata_state();
    let now = std::time::Instant::now();
    let request = metadata_request_id(&state.take_submodule_metadata_actions_at(now));
    state.handle_endpoint_result(
        "boot-1",
        &request,
        Ok(crate::api::schema::ResponseResult::SubmoduleContexts {
            contexts: vec![context("ws_1", None)],
        }),
    );
    assert!(state
        .take_submodule_metadata_actions_at(now + std::time::Duration::from_secs(4))
        .is_empty());
    // When the bounded refresh deadline is reached.
    let actions = state.take_submodule_metadata_actions_at(now + std::time::Duration::from_secs(5));
    // Then one request observes CLI/restore updates, and repeated ticks coalesce.
    assert_eq!(actions.len(), 1);
    assert!(state
        .take_submodule_metadata_actions_at(now + std::time::Duration::from_secs(5))
        .is_empty());
}

#[test]
fn reopened_parent_after_children_regroups_forward_references() {
    // Given surviving child rows preceding a newly opened parent workspace.
    let mut snapshot = hierarchy_snapshot();
    let mut parent = snapshot.workspaces.remove(0);
    parent.workspace_id = "reopened-parent".into();
    snapshot.workspaces.push(parent);
    // When refreshed metadata attaches the child to that new parent ID.
    let hierarchy = super::super::hierarchy::WorkspaceHierarchy::build(
        &snapshot,
        &[
            context("child", Some("reopened-parent")),
            context("worktree", Some("reopened-parent")),
        ],
    );
    // Then rendering and navigation both place the late parent above its descendants.
    assert_eq!(
        hierarchy
            .entries(&snapshot, &HashSet::new())
            .iter()
            .map(|entry| (
                snapshot.workspaces[entry.index].workspace_id.as_str(),
                entry.depth
            ))
            .collect::<Vec<_>>(),
        vec![("reopened-parent", 0), ("child", 1), ("worktree", 2)]
    );
}
