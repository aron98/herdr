use super::*;
use crate::api::schema::{Method, ResponseResult, SubmoduleContextInfo};

#[derive(Clone, Debug, Default)]
pub(super) struct SubmoduleMetadata {
    pub(super) hierarchy: super::hierarchy::WorkspaceHierarchy,
    pub(super) error: Option<String>,
    pub(super) contexts: Vec<SubmoduleContextInfo>,
    identity: Option<(String, Option<u64>)>,
    workspaces: Vec<String>,
    pending: Option<String>,
    context_requests: HashSet<String>,
    dirty: bool,
    refresh_deadline: Option<std::time::Instant>,
}

impl ClientShellState {
    /// Optional metadata has independent request ownership so background endpoints never
    /// apply a response to the active endpoint's overlay or lose it on a focus switch.
    pub(crate) fn take_submodule_metadata_actions(&mut self) -> Vec<ClientShellAction> {
        self.take_submodule_metadata_actions_at(std::time::Instant::now())
    }

    pub(super) fn take_submodule_metadata_actions_at(
        &mut self,
        now: std::time::Instant,
    ) -> Vec<ClientShellAction> {
        let mut actions = Vec::new();
        for endpoint in &mut self.endpoints {
            if endpoint.status != ClientEndpointStatus::Online
                || !endpoint
                    .methods
                    .as_ref()
                    .is_some_and(|methods| methods.contains("submodule.contexts"))
            {
                continue;
            }
            let Some(snapshot) = endpoint.snapshot.as_deref() else {
                continue;
            };
            let cache = &mut endpoint.submodules;
            let identity_changed = cache.identity.as_ref().is_none_or(|(boot, generation)| {
                boot != &snapshot.boot_id || generation != &endpoint.snapshot_generation
            });
            if identity_changed {
                *cache = SubmoduleMetadata::default();
                cache.identity = Some((snapshot.boot_id.clone(), endpoint.snapshot_generation));
                cache.dirty = true;
            }
            if !cache.workspaces.iter().map(String::as_str).eq(snapshot
                .workspaces
                .iter()
                .map(|ws| ws.workspace_id.as_str()))
            {
                cache.workspaces = snapshot
                    .workspaces
                    .iter()
                    .map(|ws| ws.workspace_id.clone())
                    .collect();
                cache.dirty = true;
            }
            cache.dirty |= cache.pending.is_none()
                && cache
                    .refresh_deadline
                    .is_some_and(|deadline| now >= deadline);
            if !cache.dirty || cache.pending.is_some() {
                continue;
            }
            let id = format!("client-submodules:{}", self.next_request_id);
            self.next_request_id = self.next_request_id.saturating_add(1);
            cache.pending = Some(id.clone());
            cache.dirty = false;
            cache.refresh_deadline = Some(now + std::time::Duration::from_secs(5));
            actions.push(ClientShellAction::Endpoint {
                endpoint_id: endpoint.endpoint_id.clone(),
                boot_id: snapshot.boot_id.clone(),
                request: Box::new(crate::api::schema::Request {
                    id,
                    method: Method::SubmoduleContexts(crate::api::schema::EmptyParams {}),
                }),
            });
        }
        actions
    }

    pub(super) fn cancel_submodule_metadata(&mut self, id: &str) -> bool {
        if let Some(endpoint) = self.endpoints.iter_mut().find(|endpoint| {
            endpoint.submodules.pending.as_deref() == Some(id)
                || endpoint.submodules.context_requests.contains(id)
        }) {
            if endpoint.submodules.pending.as_deref() == Some(id) {
                endpoint.submodules.pending = None;
            }
            endpoint.submodules.context_requests.remove(id);
            endpoint.submodules.error = Some("Repository context request interrupted".into());
            return true;
        }
        false
    }

    pub(crate) fn is_submodule_metadata_request(&self, id: &str) -> bool {
        self.endpoints.iter().any(|endpoint| {
            endpoint.submodules.pending.as_deref() == Some(id)
                || endpoint.submodules.context_requests.contains(id)
        })
    }

    pub(super) fn complete_submodule_metadata(
        &mut self,
        boot_id: &str,
        request_id: &str,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        self.pending_requests.remove(request_id);
        let Some(endpoint) = self.endpoints.iter_mut().find(|endpoint| {
            endpoint.submodules.pending.as_deref() == Some(request_id)
                || endpoint.submodules.context_requests.contains(request_id)
        }) else {
            return false;
        };
        let cache = &mut endpoint.submodules;
        let context_mutation = cache.context_requests.remove(request_id);
        if !context_mutation {
            cache.pending = None;
        }
        let current = endpoint.snapshot.as_deref().is_some_and(|snapshot| {
            snapshot.boot_id == boot_id
                && cache.identity.as_ref().is_some_and(|(boot, generation)| {
                    boot == boot_id && generation == &endpoint.snapshot_generation
                })
        });
        if context_mutation {
            if !current {
                return false;
            }
            cache.dirty = true;
            match result {
                Ok(ResponseResult::SubmoduleContext { context }) => {
                    cache
                        .contexts
                        .retain(|existing| existing.workspace_id != context.workspace_id);
                    cache.contexts.push(context);
                    cache.error = None;
                }
                Err(error) => {
                    cache.error = Some(format!("Parent skill sharing: {}", error.message))
                }
                Ok(_) => cache.error = Some("Unexpected parent skill sharing response".into()),
            }
            return true;
        }
        let topology_matches = endpoint.snapshot.as_deref().is_some_and(|snapshot| {
            cache.workspaces.iter().map(String::as_str).eq(snapshot
                .workspaces
                .iter()
                .map(|ws| ws.workspace_id.as_str()))
        });
        if !current || !topology_matches || cache.dirty {
            cache.dirty = true;
            return false;
        }
        match result {
            Ok(ResponseResult::SubmoduleContexts { contexts }) => {
                cache.error = None;
                cache.contexts = contexts;
                if let Some(snapshot) = endpoint.snapshot.as_deref() {
                    cache.hierarchy =
                        super::hierarchy::WorkspaceHierarchy::build(snapshot, &cache.contexts);
                }
                true
            }
            Ok(_) => {
                cache.error =
                    Some("Repository context unavailable: unexpected server response".into());
                true
            }
            Err(error) => {
                cache.error = Some(format!("Repository context unavailable: {}", error.message));
                for context in &mut cache.contexts {
                    context.status = "unavailable".into();
                    context.messages = vec![error.message.clone()];
                }
                true
            }
        }
    }

    pub(super) fn invalidate_submodule_metadata(&mut self) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        {
            endpoint.submodules.dirty = true;
        }
    }

    pub(super) fn hierarchy_for_endpoint(
        &self,
        endpoint_id: &ClientEndpointId,
    ) -> &super::hierarchy::WorkspaceHierarchy {
        static EMPTY: std::sync::LazyLock<super::hierarchy::WorkspaceHierarchy> =
            std::sync::LazyLock::new(Default::default);
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .map(|endpoint| &endpoint.submodules.hierarchy)
            .unwrap_or(&EMPTY)
    }

    pub(super) fn is_submodule_parent(&self, workspace_id: &str) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .is_some_and(|endpoint| {
                endpoint
                    .submodules
                    .contexts
                    .iter()
                    .any(|context| context.parent_workspace_id.as_deref() == Some(workspace_id))
            })
    }

    pub(super) fn submodule_context(&self, workspace_id: &str) -> Option<&SubmoduleContextInfo> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)?
            .submodules
            .contexts
            .iter()
            .find(|context| context.workspace_id == workspace_id)
    }

    pub(super) fn begin_submodule_open(
        &mut self,
        workspace_id: String,
        outcome: &mut ClientShellInput,
    ) {
        self.push_endpoint_method_with_kind(
            Method::SubmoduleList(crate::api::schema::SubmoduleListParams {
                workspace_id: Some(workspace_id.clone()),
                cwd: None,
            }),
            PendingEndpointKind::PrepareSubmoduleOpen { workspace_id },
            outcome,
        );
    }

    pub(super) fn refresh_submodule_context(
        &mut self,
        workspace_id: String,
        toggle: bool,
        outcome: &mut ClientShellInput,
    ) {
        let enabled = toggle.then(|| {
            self.submodule_context(&workspace_id)
                .is_none_or(|context| !context.sharing_enabled)
        });
        let sent = self.push_endpoint_method_with_kind(
            Method::SubmoduleContextRefresh(crate::api::schema::SubmoduleContextParams {
                workspace_id,
                enabled,
                codex_sources: None,
                claude_sources: None,
            }),
            PendingEndpointKind::SubmoduleContextRefresh,
            outcome,
        );
        if sent {
            if let Some(ClientShellAction::Endpoint { request, .. }) = outcome.actions.last() {
                if let Some(endpoint) = self
                    .endpoints
                    .iter_mut()
                    .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
                {
                    endpoint
                        .submodules
                        .context_requests
                        .insert(request.id.clone());
                }
            }
        }
    }

    pub(super) fn handle_submodule_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        match (kind, result) {
            (
                PendingEndpointKind::PrepareSubmoduleOpen { workspace_id },
                Ok(ResponseResult::SubmoduleList { submodules, .. }),
            ) => {
                if submodules.is_empty() {
                    self.set_endpoint_error("No registered submodules in this checkout.");
                } else {
                    self.overlay = Some(ClientShellOverlay::WorktreeOpen(
                        ClientWorktreeOpenOverlay {
                            submodules: true,
                            source_workspace_id: workspace_id,
                            entries: submodules
                                .into_iter()
                                .map(|entry| ClientWorktreeOpenEntry {
                                    initialized: entry.initialized,
                                    label: entry.path.clone(),
                                    path: entry.path,
                                    branch: None,
                                    is_linked_worktree: false,
                                    is_detached: false,
                                    open_workspace_id: None,
                                })
                                .collect(),
                            selected: 0,
                            query: TextEditor::default(),
                            search_focused: false,
                            error: None,
                            opening: false,
                        },
                    ));
                }
            }
            (PendingEndpointKind::SubmoduleOpen, Ok(ResponseResult::SubmoduleOpened { .. })) => {
                self.overlay = None;
                self.invalidate_submodule_metadata();
            }
            (PendingEndpointKind::SubmoduleOpen, Err(error)) => {
                if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut() {
                    open.opening = false;
                    open.error = Some(error.message);
                }
            }
            (_, Err(error)) => self.set_endpoint_error(&error.message),
            _ => self.set_endpoint_error("Unexpected submodule response."),
        }
        outcome
            .actions
            .extend(self.take_submodule_metadata_actions());
        true
    }
}
