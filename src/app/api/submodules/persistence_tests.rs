use crate::submodule::tests::Fixture;

#[tokio::test]
async fn submodule_context_snapshot_round_trip_preserves_identity_and_old_defaults() {
    let fixture = Fixture::new();
    let mut state = crate::app::state::AppState::test_with_adversarial_identity_state();
    state.assert_invariants_for_test();
    let context = crate::submodule::select(&fixture.parent, "nested/child module").unwrap();
    state.workspaces[0].submodule_context = Some(context.clone());
    let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    let snapshot = crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &runtimes,
        state.active,
        state.selected,
    );
    let json = serde_json::to_string(&snapshot).unwrap();
    let snapshot: crate::persist::SessionSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(
        snapshot.workspaces[0].submodule_context,
        Some(context.clone())
    );
    let (tx, _rx) = tokio::sync::mpsc::channel(128);
    let (workspaces, terminals, runtimes) = crate::persist::restore(
        &snapshot,
        None,
        24,
        80,
        0,
        "__missing_shell__",
        crate::config::ShellModeConfig::NonLogin,
        false,
        tx,
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
    );
    assert_eq!(workspaces[0].submodule_context, Some(context));
    let captured = crate::persist::capture(
        &workspaces,
        &terminals,
        &runtimes.into(),
        snapshot.active,
        snapshot.selected,
    );
    assert_eq!(captured.workspaces[0].id, snapshot.workspaces[0].id);
    assert_eq!(
        captured.workspaces[0].public_tab_numbers,
        snapshot.workspaces[0].public_tab_numbers
    );
    let old: crate::persist::SessionSnapshot = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/session/current-herdr-session.json"
    ))
    .unwrap();
    assert!(old
        .workspaces
        .iter()
        .all(|ws| ws.submodule_context.is_none()));
}
