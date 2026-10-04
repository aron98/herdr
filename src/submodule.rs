//! Server-owned provenance for registered submodule checkouts and their worktrees.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod discovery;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) use discovery::{discover, select};

// Keep filesystem reconciliation and its app-state completion in one mutation lease.
static PREPARATION_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
pub(crate) type PreparationPermit = std::sync::Arc<tokio::sync::SemaphorePermit<'static>>;
pub(crate) fn preparation_slot() -> Result<PreparationPermit, String> {
    PREPARATION_SLOT
        .try_acquire()
        .map(std::sync::Arc::new)
        .map_err(|_| "Another submodule context operation is pending; retry shortly".into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Association {
    pub parent_path: PathBuf,
    pub parent_repo_key: String,
    pub submodule_path: String,
    pub checkout_path: PathBuf,
    pub repo_key: String,
    pub sharing_enabled: bool,
    #[serde(default)]
    pub codex_sources: Vec<PathBuf>,
    #[serde(default)]
    pub claude_sources: Vec<PathBuf>,
    #[serde(default = "pending_status")]
    pub status: String,
    #[serde(default)]
    pub messages: Vec<String>,
}

fn pending_status() -> String {
    "pending".into()
}

impl Association {
    pub(crate) fn prepare(&mut self) {
        let valid_checkout =
            crate::workspace::git_space_metadata(&self.checkout_path).is_some_and(|space| {
                space.key == self.repo_key && space.repo_root == self.checkout_path
            });
        let valid_parent = !self.sharing_enabled
            || crate::workspace::git_space_metadata(&self.parent_path).is_some_and(|space| {
                space.key == self.parent_repo_key && space.repo_root == self.parent_path
            });
        if !valid_parent || !valid_checkout {
            self.status = "unavailable".into();
            self.messages = vec![
                "The recorded parent or child checkout is unavailable or has changed repository."
                    .into(),
            ];
            return;
        }
        if self.sharing_enabled {
            match select(&self.parent_path, &self.submodule_path) {
                Ok(registered)
                    if registered.repo_key == self.repo_key
                        && registered.parent_repo_key == self.parent_repo_key => {}
                Ok(_) => {
                    self.status = "unavailable".into();
                    self.messages =
                        vec!["The registered submodule now belongs to another repository.".into()];
                    return;
                }
                Err(error) => {
                    self.status = "unavailable".into();
                    self.messages =
                        vec![format!("Parent submodule context is unavailable: {error}")];
                    return;
                }
            }
        }
        let result = if self.sharing_enabled {
            crate::skill_sharing::refresh(&crate::skill_sharing::SharingRequest {
                parent: self.parent_path.clone(),
                checkout: self.checkout_path.clone(),
                codex_sources: self.codex_sources.clone(),
                claude_sources: self.claude_sources.clone(),
            })
        } else {
            crate::skill_sharing::disable(&self.checkout_path)
        };
        match result {
            Ok(report) => {
                self.status = report.status;
                self.messages = report.messages;
            }
            Err(error) => {
                self.status = "failed".into();
                self.messages = vec![error.to_string()];
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum OperationData {
    List {
        parent_path: PathBuf,
        submodules: Vec<crate::api::schema::SubmoduleInfo>,
    },
    Open(Association),
    Refresh(Vec<ContextUpdate>),
}

#[derive(Debug)]
pub(crate) struct ContextUpdate {
    pub workspace_id: String,
    pub previous: Association,
    pub context: Association,
}

#[derive(Debug)]
pub(crate) struct OperationResult {
    pub _write_permit: Option<PreparationPermit>,
    pub _permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
    pub request: crate::api::schema::Request,
    pub client_local: bool,
    pub result: Result<OperationData, String>,
    pub respond_to: std::sync::mpsc::Sender<String>,
}
