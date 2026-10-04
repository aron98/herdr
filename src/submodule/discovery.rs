use super::Association;
use crate::api::schema::SubmoduleInfo;
use std::path::{Component, Path, PathBuf};

fn git(path: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

pub(crate) fn discover(cwd: &Path) -> Result<(PathBuf, Vec<SubmoduleInfo>), String> {
    let space = crate::workspace::git_space_metadata(cwd).ok_or("A Git checkout is required")?;
    let parent = std::fs::canonicalize(&space.repo_root).map_err(|e| e.to_string())?;
    let index = git(&parent, &["ls-files", "--stage", "-z"])?;
    if !parent.join(".gitmodules").is_file() {
        return Ok((parent, Vec::new()));
    }
    let config = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(&parent)
        .args([
            "config",
            "--null",
            "--file",
            ".gitmodules",
            "--get-regexp",
            "^submodule\\..*\\.path$",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !config.status.success() && config.status.code() != Some(1) {
        return Err(String::from_utf8_lossy(&config.stderr).trim().to_owned());
    }
    let registered: Vec<&[u8]> = config
        .stdout
        .split(|b| *b == 0)
        .filter_map(|record| record.split_once_byte(b'\n').map(|(_, value)| value))
        .collect();
    let mut submodules = Vec::new();
    for record in index.split(|b| *b == 0) {
        let Some((header, path)) = record.split_once_byte(b'\t') else {
            continue;
        };
        if !header.starts_with(b"160000 ")
            || !header.ends_with(b" 0")
            || !registered.contains(&path)
        {
            continue;
        }
        let path = std::str::from_utf8(path).map_err(|_| "Submodule paths must be UTF-8")?;
        if !Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            continue;
        }
        let checkout = parent.join(path);
        let canonical = std::fs::canonicalize(&checkout).unwrap_or_else(|_| checkout.clone());
        let initialized = canonical.starts_with(&parent)
            && crate::workspace::git_space_metadata(&checkout)
                .is_some_and(|child| child.repo_root == canonical && child.key != space.key);
        submodules.push(SubmoduleInfo {
            path: path.into(),
            checkout_path: canonical.display().to_string(),
            initialized,
        });
    }
    submodules.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((parent, submodules))
}

pub(crate) fn select(cwd: &Path, path: &str) -> Result<Association, String> {
    let (parent_path, entries) = discover(cwd)?;
    let entry = entries
        .into_iter()
        .find(|entry| entry.path == path)
        .ok_or("Path is not a registered submodule in this parent checkout")?;
    if !entry.initialized {
        return Err("Submodule is uninitialized; initialize it with Git before opening it".into());
    }
    let checkout_path = PathBuf::from(entry.checkout_path);
    let parent =
        crate::workspace::git_space_metadata(&parent_path).ok_or("Parent checkout disappeared")?;
    let child = crate::workspace::git_space_metadata(&checkout_path)
        .ok_or("Submodule checkout disappeared")?;
    Ok(Association {
        parent_path,
        parent_repo_key: parent.key,
        submodule_path: entry.path,
        checkout_path,
        repo_key: child.key,
        sharing_enabled: true,
        codex_sources: Vec::new(),
        claude_sources: Vec::new(),
        status: "pending".into(),
        messages: Vec::new(),
    })
}

trait SplitOnceByte {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])>;
}
impl SplitOnceByte for [u8] {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])> {
        let index = self.iter().position(|b| *b == byte)?;
        Some((&self[..index], &self[index + 1..]))
    }
}
