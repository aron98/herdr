use super::super::responses::encode_error;
use crate::api::schema::{Method, Request};
use crate::app::App;
use crate::submodule::{ContextUpdate, OperationData, OperationResult};
use std::path::PathBuf;

impl App {
    pub(crate) fn start_submodule_request(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
        client_local: bool,
    ) {
        let input = self.capture_submodule_input(&request.method);
        let input = match input {
            Ok(input) => input,
            Err(error) => {
                send_error(&respond_to, request.id, error);
                return;
            }
        };
        let write_permit = if matches!(&request.method, Method::SubmoduleList(_)) {
            None
        } else {
            match crate::submodule::preparation_slot() {
                Ok(permit) => Some(permit),
                Err(error) => {
                    send_error(&respond_to, request.id, error);
                    return;
                }
            }
        };
        let Ok(permit) = self.worktree_read_slots.clone().try_acquire_owned() else {
            send_error(
                &respond_to,
                request.id,
                "Too many repository operations are pending; retry shortly".into(),
            );
            return;
        };
        let event_tx = self.event_tx.clone();
        let failure_sender = respond_to.clone();
        let id = request.id.clone();
        let spawn = std::thread::Builder::new()
            .name("submodule-operation".into())
            .spawn(move || {
                let result = match input {
                    Input::Source { cwd, known } => match &request.method {
                        Method::SubmoduleList(_) => {
                            crate::submodule::discover(&cwd).map(|(parent_path, submodules)| {
                                OperationData::List {
                                    parent_path,
                                    submodules,
                                }
                            })
                        }
                        Method::SubmoduleOpen(params) => {
                            crate::submodule::select(&cwd, &params.path).map(|mut context| {
                                if let Some(previous) = known.into_iter().find(|previous| {
                                    previous.parent_path == context.parent_path
                                        && previous.submodule_path == context.submodule_path
                                        && previous.repo_key == context.repo_key
                                }) {
                                    context.codex_sources = previous.codex_sources;
                                    context.claude_sources = previous.claude_sources;
                                    context.sharing_enabled = previous.sharing_enabled;
                                }
                                context.sharing_enabled &= params.share_skills;
                                context.prepare();
                                OperationData::Open(context)
                            })
                        }
                        _ => Err("Invalid submodule source operation".into()),
                    },
                    Input::Refresh(mut updates) => {
                        for update in &mut updates {
                            update.context.prepare();
                        }
                        Ok(OperationData::Refresh(updates))
                    }
                };
                let result = OperationResult {
                    _write_permit: write_permit,
                    _permit: std::sync::Arc::new(permit),
                    request,
                    client_local,
                    result,
                    respond_to,
                };
                if let Err(error) = event_tx
                    .blocking_send(crate::events::AppEvent::SubmoduleFinished(Box::new(result)))
                {
                    tracing::debug!(%error, "submodule event receiver closed");
                }
            });
        if let Err(error) = spawn {
            send_error(&failure_sender, id, error.to_string());
        }
    }

    fn capture_submodule_input(&self, method: &Method) -> Result<Input, String> {
        match method {
            Method::SubmoduleList(params) => self
                .submodule_source_path(&params.workspace_id, &params.cwd)
                .map(|cwd| Input::Source {
                    cwd,
                    known: Vec::new(),
                }),
            Method::SubmoduleOpen(params) => self
                .submodule_source_path(&params.workspace_id, &params.cwd)
                .map(|cwd| Input::Source {
                    cwd,
                    known: self
                        .state
                        .workspaces
                        .iter()
                        .filter_map(|ws| ws.submodule_context.clone())
                        .collect(),
                }),
            Method::SubmoduleContextRefresh(params) => {
                let idx = self
                    .parse_workspace_id(&params.workspace_id)
                    .ok_or("Workspace not found")?;
                let ws = &self.state.workspaces[idx];
                let previous = ws
                    .submodule_context
                    .clone()
                    .ok_or("Workspace has no submodule context")?;
                let mut context = previous.clone();
                if let Some(enabled) = params.enabled {
                    context.sharing_enabled = enabled;
                }
                if let Some(sources) = &params.codex_sources {
                    context.codex_sources = sources.iter().map(PathBuf::from).collect();
                }
                if let Some(sources) = &params.claude_sources {
                    context.claude_sources = sources.iter().map(PathBuf::from).collect();
                }
                let mut updates = vec![ContextUpdate {
                    workspace_id: ws.id.clone(),
                    previous: previous.clone(),
                    context: context.clone(),
                }];
                for other in &self.state.workspaces {
                    if other.id == ws.id {
                        continue;
                    }
                    if let Some(related) = &other.submodule_context {
                        if related.parent_path == previous.parent_path
                            && related.repo_key == previous.repo_key
                            && related.submodule_path == previous.submodule_path
                        {
                            let mut prepared = related.clone();
                            prepared.sharing_enabled = context.sharing_enabled;
                            prepared.codex_sources = context.codex_sources.clone();
                            prepared.claude_sources = context.claude_sources.clone();
                            updates.push(ContextUpdate {
                                workspace_id: other.id.clone(),
                                previous: related.clone(),
                                context: prepared,
                            });
                        }
                    }
                }
                Ok(Input::Refresh(updates))
            }
            _ => Err("Invalid submodule operation".into()),
        }
    }
}

enum Input {
    Source {
        cwd: PathBuf,
        known: Vec<crate::submodule::Association>,
    },
    Refresh(Vec<ContextUpdate>),
}

fn send_error(sender: &std::sync::mpsc::Sender<String>, id: String, error: String) {
    if let Err(error) = sender.send(encode_error(id, "submodule_failed", error)) {
        tracing::debug!(%error, "submodule response receiver closed");
    }
}
