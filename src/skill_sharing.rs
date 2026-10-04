use std::{
    io,
    path::{Path, PathBuf},
};

pub(crate) struct SharingRequest {
    pub parent: PathBuf,
    pub checkout: PathBuf,
    pub codex_sources: Vec<PathBuf>,
    pub claude_sources: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SharingReport {
    pub status: String,
    pub messages: Vec<String>,
}

pub(crate) fn refresh(request: &SharingRequest) -> io::Result<SharingReport> {
    reconcile(
        &crate::config::state_dir().join("skill-sharing"),
        &request.checkout,
        Some(request),
    )
}
pub(crate) fn disable(checkout: &Path) -> io::Result<SharingReport> {
    reconcile(
        &crate::config::state_dir().join("skill-sharing"),
        checkout,
        None,
    )
}
mod inventory;
#[cfg(test)]
mod tests;

use inventory::{Inventory, Link};
use std::{collections::BTreeMap, fs, process::Command};

fn reconcile(
    root: &Path,
    checkout: &Path,
    request: Option<&SharingRequest>,
) -> io::Result<SharingReport> {
    fs::create_dir_all(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "skill inventory must be a regular directory",
        ));
    }
    inventory::regular_file_or_missing(&root.join("lock"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("lock"))?;
    lock.lock()?;
    let checkout = fs::canonicalize(checkout)?;
    let mut inventory = Inventory::load(root)?;
    let mut report = SharingReport {
        status: if request.is_some() {
            "ready"
        } else {
            "disabled"
        }
        .into(),
        messages: Vec::new(),
    };
    let desired = match request {
        Some(request) => {
            if !request.parent.is_dir() {
                return Ok(SharingReport {
                    status: "unavailable".into(),
                    messages: vec![format!(
                        "Parent checkout is unavailable: {}",
                        request.parent.display()
                    )],
                });
            }
            desired_links(request)?
        }
        None => BTreeMap::new(),
    };
    let tracked = tracked_paths(&checkout)?;
    let exclude = git_exclusion_path(&checkout)?;
    if inventory
        .exclusions
        .iter()
        .any(|entry| entry.checkouts.contains(&checkout) && entry.path != exclude)
    {
        conflict(
            &mut report,
            &exclude,
            "Git exclusion ownership changed; inventory needs review",
        );
        return Ok(report);
    }
    let mut index = 0;
    while index < inventory.links.len() {
        let link = &inventory.links[index];
        if link.checkout != checkout {
            index += 1;
            continue;
        }
        let destination = checkout.join(&link.relative);
        if link.exclude != exclude {
            conflict(
                &mut report,
                &destination,
                "Git administration changed; ownership needs review",
            );
            index += 1;
            continue;
        }
        if let Err(error) = safe_ancestors(&checkout, &link.relative, false) {
            conflict(&mut report, &destination, &error.to_string());
            index += 1;
            continue;
        }
        let tracked = is_tracked(&tracked, &link.relative);
        let exact = fs::read_link(&destination).is_ok_and(|target| target == link.target);
        if !tracked && exact && desired.get(&link.relative) == Some(&link.target) {
            let link = link.clone();
            inventory.ensure_exclusion(root, &link)?;
            index += 1;
            continue;
        }
        if !tracked && exact {
            crate::platform::remove_directory_link(&destination)?;
        }
        inventory.links.remove(index);
        inventory.save(root)?;
    }
    for (relative, target) in desired {
        if inventory
            .links
            .iter()
            .any(|link| link.checkout == checkout && link.relative == relative)
        {
            continue;
        }
        let destination = checkout.join(&relative);
        if is_tracked(&tracked, &relative) {
            conflict(&mut report, &destination, "path is tracked by Git");
            continue;
        }
        if let Err(error) = safe_ancestors(&checkout, &relative, false) {
            conflict(&mut report, &destination, &error.to_string());
            continue;
        }
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                conflict(
                    &mut report,
                    &destination,
                    "existing checkout entry takes precedence",
                );
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        safe_ancestors(&checkout, &relative, true)?;
        let link = Link {
            checkout: checkout.clone(),
            relative,
            target,
            exclude: exclude.clone(),
        };
        // Persist intent before either Git exclusion or directory-link mutation.
        inventory.links.push(link.clone());
        inventory.save(root)?;
        inventory.ensure_exclusion(root, &link)?;
        crate::platform::create_directory_link(&link.target, &destination)?;
    }
    inventory.prune_exclusions(root, &checkout, &exclude)?;
    Ok(report)
}
fn conflict(report: &mut SharingReport, path: &Path, reason: &str) {
    report.status = "conflicts".into();
    report
        .messages
        .push(format!("{}: {reason}", path.display()));
}
fn desired_links(request: &SharingRequest) -> io::Result<BTreeMap<PathBuf, PathBuf>> {
    let mut common = BTreeMap::new();
    catalog(&request.parent.join(".agents/skills"), &mut common, false)?;
    let mut claude = common.clone();
    catalog(&request.parent.join(".claude/skills"), &mut claude, true)?;
    for source in &request.codex_sources {
        catalog(&source_path(&request.parent, source), &mut common, false)?;
    }
    for source in &request.claude_sources {
        catalog(&source_path(&request.parent, source), &mut claude, false)?;
    }
    Ok(common
        .into_iter()
        .map(|(name, target)| (Path::new(".agents/skills").join(name), target))
        .chain(
            claude
                .into_iter()
                .map(|(name, target)| (Path::new(".claude/skills").join(name), target)),
        )
        .collect())
}
fn source_path(parent: &Path, source: &Path) -> PathBuf {
    if source.is_absolute() {
        source.to_owned()
    } else {
        parent.join(source)
    }
}
fn catalog(
    path: &Path,
    entries: &mut BTreeMap<PathBuf, PathBuf>,
    overwrite: bool,
) -> io::Result<()> {
    let directory = match fs::read_dir(path) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in directory {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = PathBuf::from(entry.file_name());
        if overwrite || !entries.contains_key(&name) {
            entries.insert(name, fs::canonicalize(entry.path())?);
        }
    }
    Ok(())
}
fn validate_destination(relative: &Path) -> io::Result<()> {
    let components: Vec<_> = relative.components().collect();
    if !matches!(components.as_slice(), [std::path::Component::Normal(agent), std::path::Component::Normal(skills), std::path::Component::Normal(_)] if (*agent == ".agents" || *agent == ".claude") && *skills == "skills")
    {
        return Err(io::Error::other(
            "inventory destination is outside native skill directories",
        ));
    }
    Ok(())
}
fn safe_ancestors(checkout: &Path, relative: &Path, create: bool) -> io::Result<()> {
    validate_destination(relative)?;
    let mut path = checkout.to_owned();
    let Some(parent) = relative.parent() else {
        return Err(io::Error::other("skill has no parent directory"));
    };
    for component in parent.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(io::Error::other("invalid skill destination"));
        }
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(io::Error::other(
                    "destination ancestor is not a regular directory",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if create {
                    fs::create_dir(&path)?;
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn git_output(checkout: &Path, arguments: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(output.stdout)
}
fn git_path(checkout: &Path, arguments: &[&str]) -> io::Result<PathBuf> {
    let output = String::from_utf8(git_output(checkout, arguments)?).map_err(io::Error::other)?;
    Ok(PathBuf::from(output.trim_end_matches(['\n', '\r'])))
}
fn git_exclusion_path(checkout: &Path) -> io::Result<PathBuf> {
    let path = git_path(
        checkout,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ],
    )?;
    match fs::canonicalize(&path) {
        Ok(resolved) => Ok(resolved),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("Git exclusion path has no parent"))?;
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other("Git exclusion path has no filename"))?;
            fs::create_dir_all(parent)?;
            Ok(fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}
fn tracked_paths(checkout: &Path) -> io::Result<Vec<PathBuf>> {
    let output =
        String::from_utf8(git_output(checkout, &["ls-files", "-z"])?).map_err(io::Error::other)?;
    Ok(output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect())
}
fn is_tracked(tracked: &[PathBuf], relative: &Path) -> bool {
    tracked
        .iter()
        .any(|path| path.starts_with(relative) || relative.starts_with(path))
}
