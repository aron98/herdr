use super::*;

impl App {
    pub(crate) fn refresh_restored_submodules(&mut self) {
        // One bounded worker batch also handles sessions with more contexts than worker slots.
        let contexts = self
            .state
            .workspaces
            .iter_mut()
            .filter_map(|ws| {
                let context = ws.submodule_context.as_mut()?;
                context.status = "pending".into();
                Some((ws.id.clone(), context.clone()))
            })
            .collect::<Vec<_>>();
        if contexts.is_empty() {
            return;
        }
        let write_permit = match crate::submodule::preparation_slot() {
            Ok(permit) => permit,
            Err(error) => {
                for ws in &mut self.state.workspaces {
                    if let Some(context) = ws.submodule_context.as_mut() {
                        context.status = "failed".into();
                        context.messages = vec![error.clone()];
                    }
                }
                return;
            }
        };
        let Ok(permit) = self.worktree_read_slots.clone().try_acquire_owned() else {
            for ws in &mut self.state.workspaces {
                if let Some(context) = ws.submodule_context.as_mut() {
                    context.status = "failed".into();
                    context.messages = vec![
                        "Restore worker capacity unavailable; refresh context to retry.".into(),
                    ];
                }
            }
            return;
        };
        let permit = std::sync::Arc::new(permit);
        let event_tx = self.event_tx.clone();
        let spawn = std::thread::Builder::new()
            .name("submodule-restore".into())
            .spawn(move || {
                for (workspace_id, previous) in contexts {
                    let mut context = previous.clone();
                    context.prepare();
                    let (respond_to, _rx) = std::sync::mpsc::channel();
                    let request = Request {
                        id: "restore-submodule".into(),
                        method: Method::SubmoduleContextRefresh(
                            crate::api::schema::SubmoduleContextParams {
                                workspace_id: workspace_id.clone(),
                                enabled: None,
                                codex_sources: None,
                                claude_sources: None,
                            },
                        ),
                    };
                    let result = OperationResult {
                        _write_permit: Some(write_permit.clone()),
                        _permit: permit.clone(),
                        request,
                        client_local: false,
                        result: Ok(OperationData::Refresh(vec![
                            crate::submodule::ContextUpdate {
                                workspace_id,
                                previous,
                                context,
                            },
                        ])),
                        respond_to,
                    };
                    if let Err(error) = event_tx
                        .blocking_send(crate::events::AppEvent::SubmoduleFinished(Box::new(result)))
                    {
                        tracing::debug!(%error, "submodule restore event receiver closed");
                        break;
                    }
                }
            });
        if let Err(error) = spawn {
            tracing::warn!(%error, "could not start submodule context restore");
            for ws in &mut self.state.workspaces {
                if let Some(context) = ws.submodule_context.as_mut() {
                    context.status = "failed".into();
                    context.messages = vec![format!("Could not start context restore: {error}")];
                }
            }
        }
    }
}
