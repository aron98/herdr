use super::*;

/// Built when workspace topology or repository context changes, never while drawing.
#[derive(Clone, Debug, Default)]
pub(super) struct WorkspaceHierarchy {
    ordered: Vec<WorkspaceEntry>,
    parents: Vec<Option<usize>>,
    group_keys: Vec<Option<String>>,
    aggregate_status: Vec<crate::api::schema::AgentStatus>,
}

impl WorkspaceHierarchy {
    pub(super) fn build(
        snapshot: &ClientShellSnapshot,
        contexts: &[crate::api::schema::SubmoduleContextInfo],
    ) -> Self {
        if contexts.is_empty() {
            return Self::default();
        }
        let workspaces = &snapshot.workspaces;
        let ids: HashMap<&str, usize> = workspaces
            .iter()
            .enumerate()
            .map(|(index, ws)| (ws.workspace_id.as_str(), index))
            .collect();
        let mut repo_roots = HashMap::new();
        for (index, ws) in workspaces.iter().enumerate() {
            if let Some(worktree) = ws
                .worktree
                .as_ref()
                .filter(|worktree| !worktree.is_linked_worktree)
            {
                repo_roots.entry(worktree.key.as_str()).or_insert(index);
            }
        }
        let mut parents = vec![None; workspaces.len()];
        let mut submodules = vec![false; workspaces.len()];
        for (index, ws) in workspaces.iter().enumerate() {
            if let Some(worktree) = ws
                .worktree
                .as_ref()
                .filter(|worktree| worktree.is_linked_worktree)
            {
                parents[index] = repo_roots.get(worktree.key.as_str()).copied();
            }
        }
        for context in contexts {
            let Some(&index) = ids.get(context.workspace_id.as_str()) else {
                continue;
            };
            // A linked worktree remains beneath its repository's main checkout.
            if parents[index].is_none() {
                parents[index] = context
                    .parent_workspace_id
                    .as_deref()
                    .and_then(|id| ids.get(id))
                    .copied()
                    .filter(|parent| *parent != index);
                submodules[index] = true;
            }
        }
        // Reject malformed cycles at the metadata boundary rather than looping in render.
        for index in 0..parents.len() {
            let mut cursor = parents[index];
            let mut remaining = parents.len();
            while let Some(parent) = cursor {
                if parent == index || remaining == 0 {
                    parents[index] = None;
                    break;
                }
                remaining = remaining.saturating_sub(1);
                cursor = parents[parent];
            }
        }
        let mut children = vec![Vec::new(); workspaces.len()];
        let mut roots = Vec::new();
        for (index, parent) in parents.iter().enumerate() {
            match parent {
                Some(parent) => children[*parent].push(index),
                None => roots.push(index),
            }
        }
        let group_keys = children
            .iter()
            .enumerate()
            .map(|(index, children)| {
                if children.is_empty() {
                    None
                } else {
                    Some(
                        workspaces[index]
                            .worktree
                            .as_ref()
                            .filter(|worktree| !worktree.is_linked_worktree)
                            .map(|worktree| worktree.key.clone())
                            .unwrap_or_else(|| {
                                format!("submodule-parent:{}", workspaces[index].workspace_id)
                            }),
                    )
                }
            })
            .collect();
        let mut ordered = Vec::with_capacity(workspaces.len());
        let mut stack: Vec<(usize, usize, bool)> = roots
            .into_iter()
            .rev()
            .map(|index| (index, 0, false))
            .collect();
        while let Some((index, depth, last_child)) = stack.pop() {
            ordered.push(WorkspaceEntry {
                index,
                depth,
                submodule: submodules[index],
                indented: depth > 0,
                last_child,
            });
            for (position, child) in children[index].iter().enumerate().rev() {
                stack.push((
                    *child,
                    depth.saturating_add(1),
                    position + 1 == children[index].len(),
                ));
            }
        }
        let mut hierarchy = Self {
            ordered,
            parents,
            group_keys,
            aggregate_status: Vec::new(),
        };
        hierarchy.update_status(snapshot);
        hierarchy
    }

    pub(super) fn update_status(&mut self, snapshot: &ClientShellSnapshot) {
        if !self.active() {
            return;
        }
        self.aggregate_status.clear();
        self.aggregate_status.extend(
            snapshot
                .workspaces
                .iter()
                .map(|workspace| workspace.agent_status),
        );
        for entry in self.ordered.iter().rev() {
            if let Some(parent) = self.parents[entry.index] {
                let status = self.aggregate_status[entry.index];
                if status_priority(status) > status_priority(self.aggregate_status[parent]) {
                    self.aggregate_status[parent] = status;
                }
            }
        }
    }

    pub(super) fn displayed_status(
        &self,
        snapshot: &ClientShellSnapshot,
        index: usize,
        collapsed: &HashSet<String>,
    ) -> crate::api::schema::AgentStatus {
        let workspace = &snapshot.workspaces[index];
        if !self.active() {
            return super::sidebar::displayed_workspace_status(snapshot, workspace, collapsed);
        }
        if self
            .group_key(index)
            .is_some_and(|key| collapsed.contains(key))
        {
            self.aggregate_status[index]
        } else {
            workspace.agent_status
        }
    }

    pub(super) fn group_key(&self, index: usize) -> Option<&str> {
        self.group_keys.get(index).and_then(Option::as_deref)
    }

    pub(super) fn active(&self) -> bool {
        !self.ordered.is_empty()
    }

    pub(super) fn entries(
        &self,
        snapshot: &ClientShellSnapshot,
        collapsed: &HashSet<String>,
    ) -> Vec<WorkspaceEntry> {
        if !self.active() {
            return super::sidebar::workspace_entries(snapshot, collapsed);
        }
        let mut focus_path = HashSet::new();
        let mut cursor = snapshot.workspaces.iter().position(|ws| ws.focused);
        while let Some(index) = cursor {
            focus_path.insert(index);
            cursor = self.parents[index];
        }
        let mut hidden_depth = None;
        let mut entries = Vec::with_capacity(self.ordered.len());
        for entry in &self.ordered {
            if hidden_depth.is_some_and(|depth| entry.depth <= depth) {
                hidden_depth = None;
            }
            if hidden_depth.is_some() && !focus_path.contains(&entry.index) {
                continue;
            }
            entries.push(*entry);
            if hidden_depth.is_none()
                && self
                    .group_key(entry.index)
                    .is_some_and(|key| collapsed.contains(key))
            {
                hidden_depth = Some(entry.depth);
            }
        }
        entries
    }
}
