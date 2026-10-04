use super::*;

#[tokio::test]
async fn submodule_navigation_resolves_live_tab_identity() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("submodule");
    let workspace_id = workspace.id.clone();
    server.app.state.workspaces.push(workspace);
    let index = server.app.state.workspaces.len() - 1;
    let expected_tab = server.app.public_tab_id(index, 0).unwrap();
    let response = serde_json::to_vec(&serde_json::json!({
        "id": "open-child",
        "result": {"type": "submodule_opened", "workspace_id": workspace_id},
    }))
    .unwrap();
    assert_eq!(
        server.deferred_endpoint_navigation_tab_id(&response),
        Some(expected_tab)
    );
    server.app.state.workspaces.remove(index);
    assert_eq!(server.deferred_endpoint_navigation_tab_id(&response), None);
}

#[tokio::test]
async fn submodule_error_response_does_not_supply_a_navigation_target() {
    let server = test_headless_server();
    assert_eq!(
        server.deferred_endpoint_navigation_tab_id(
            br#"{"id":"open-child","error":{"code":"submodule_failed","message":"uninitialized"}}"#
        ),
        None
    );
}
