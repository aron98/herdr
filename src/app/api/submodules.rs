use super::responses::{encode_error, encode_success};
use crate::api::schema::{Method, Request, ResponseResult, SubmoduleContextInfo};
use crate::app::App;
use crate::submodule::{Association, OperationData, OperationResult};
use std::path::PathBuf;

#[cfg(test)]
mod inheritance_tests;
#[cfg(test)]
mod persistence_tests;
mod restore;
#[cfg(test)]
mod tests;
mod worker;

impl App {
    fn submodule_source_path(
        &self,
        workspace_id: &Option<String>,
        cwd: &Option<String>,
    ) -> Result<PathBuf, String> {
        if workspace_id.is_some() && cwd.is_some() {
            return Err("Supply only one of workspace_id or cwd".into());
        }
        if let Some(cwd) = cwd {
            let path = crate::worktree::expand_tilde_path(cwd);
            return if path.is_absolute() {
                Ok(path)
            } else {
                Err("cwd must be absolute".into())
            };
        }
        let idx = match workspace_id {
            Some(id) => self.parse_workspace_id(id),
            None => self.state.active.or_else(|| {
                self.state
                    .workspaces
                    .get(self.state.selected)
                    .map(|_| self.state.selected)
            }),
        }
        .ok_or("Workspace not found")?;
        let ws = &self.state.workspaces[idx];
        Ok(ws
            .submodule_context
            .as_ref()
            .map(|context| context.checkout_path.clone())
            .or_else(|| ws.worktree_space().map(|space| space.checkout_path.clone()))
            .or_else(|| ws.git_space().map(|space| space.repo_root.clone()))
            .unwrap_or_else(|| ws.identity_cwd.clone()))
    }

    fn submodule_context_info(&self, idx: usize, context: &Association) -> SubmoduleContextInfo {
        let parent_idx = self.state.workspaces.iter().position(|ws| {
            let path = ws
                .submodule_context
                .as_ref()
                .map(|c| &c.checkout_path)
                .or_else(|| ws.worktree_space().map(|space| &space.checkout_path))
                .or_else(|| ws.git_space().map(|space| &space.repo_root))
                .unwrap_or(&ws.identity_cwd);
            path == &context.parent_path
        });
        SubmoduleContextInfo {
            workspace_id: self.public_workspace_id(idx),
            parent_path: context.parent_path.display().to_string(),
            submodule_path: context.submodule_path.clone(),
            checkout_path: context.checkout_path.display().to_string(),
            repo_key: context.repo_key.clone(),
            parent_workspace_id: parent_idx.map(|idx| self.public_workspace_id(idx)),
            sharing_enabled: context.sharing_enabled,
            status: context.status.clone(),
            messages: context.messages.clone(),
        }
    }

    pub(crate) fn handle_submodule_contexts(&self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SubmoduleContexts {
                contexts: self.submodule_contexts(),
            },
        )
    }

    pub(crate) fn submodule_contexts(&self) -> Vec<SubmoduleContextInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .filter_map(|(idx, ws)| {
                ws.submodule_context
                    .as_ref()
                    .map(|context| self.submodule_context_info(idx, context))
            })
            .collect()
    }

    pub(crate) fn handle_api_submodule_finished(&mut self, result: OperationResult) {
        let response = match result.result {
            Err(error) => encode_error(result.request.id, "submodule_failed", error),
            Ok(OperationData::List {
                parent_path,
                submodules,
            }) => encode_success(
                result.request.id,
                ResponseResult::SubmoduleList {
                    parent_path: parent_path.display().to_string(),
                    submodules,
                },
            ),
            Ok(OperationData::Open(context)) => self.finish_submodule_open(result.request, context),
            Ok(OperationData::Refresh(updates)) => {
                self.finish_submodule_refresh(result.request.id, updates)
            }
        };
        if let Err(error) = result.respond_to.send(response) {
            tracing::debug!(%error, "submodule response receiver closed");
        }
    }

    fn finish_submodule_refresh(
        &mut self,
        id: String,
        updates: Vec<crate::submodule::ContextUpdate>,
    ) -> String {
        let mut requested = None;
        for (number, update) in updates.into_iter().enumerate() {
            let idx = self.state.workspaces.iter().position(|ws| {
                ws.id == update.workspace_id
                    && ws.submodule_context.as_ref() == Some(&update.previous)
            });
            if let Some(idx) = idx {
                self.state.workspaces[idx].submodule_context = Some(update.context.clone());
                self.state.mark_session_dirty();
                if number == 0 {
                    requested = Some(self.submodule_context_info(idx, &update.context));
                }
            }
        }
        match requested {
            Some(context) => encode_success(id, ResponseResult::SubmoduleContext { context }),
            None => encode_error(
                id,
                "stale_submodule_context",
                "Workspace context changed while preparing skills",
            ),
        }
    }

    fn finish_submodule_open(&mut self, request: Request, context: Association) -> String {
        let Method::SubmoduleOpen(params) = request.method else {
            return encode_error(request.id, "invalid_request", "Expected submodule.open");
        };
        let existing = self.state.workspaces.iter().position(|ws| {
            ws.submodule_context
                .as_ref()
                .is_some_and(|c| c.checkout_path == context.checkout_path)
                || ws
                    .worktree_space()
                    .is_some_and(|space| space.checkout_path == context.checkout_path)
                || ws
                    .git_space()
                    .is_some_and(|space| space.repo_root == context.checkout_path)
                || ws.identity_cwd == context.checkout_path
        });
        let idx = match existing {
            Some(idx) => {
                if params.focus {
                    self.state.switch_workspace(idx);
                }
                idx
            }
            None => match self
                .create_workspace_with_options(context.checkout_path.clone(), params.focus)
            {
                Ok(idx) => idx,
                Err(error) => {
                    return encode_error(request.id, "submodule_open_failed", error.to_string())
                }
            },
        };
        self.set_worktree_membership(
            idx,
            crate::workspace::WorktreeSpaceMembership {
                key: context.repo_key.clone(),
                label: context
                    .checkout_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| context.submodule_path.clone()),
                repo_root: context.checkout_path.clone(),
                checkout_path: context.checkout_path.clone(),
                is_linked_worktree: false,
            },
            existing.is_some(),
        );
        self.state.workspaces[idx].submodule_context = Some(context);
        self.state.mark_session_dirty();
        if existing.is_none() {
            self.emit_workspace_open_events(idx);
        }
        encode_success(
            request.id,
            ResponseResult::SubmoduleOpened {
                workspace_id: self.public_workspace_id(idx),
            },
        )
    }
}
