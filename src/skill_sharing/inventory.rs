use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Inventory {
    pub links: Vec<Link>,
    pub exclusions: Vec<Exclusion>,
    pub sequence: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Link {
    pub checkout: PathBuf,
    pub relative: PathBuf,
    pub target: PathBuf,
    pub exclude: PathBuf,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Exclusion {
    pub path: PathBuf,
    pub pattern: String,
    pub block: String,
    #[serde(default)]
    pub relative: PathBuf,
    #[serde(default)]
    pub checkouts: Vec<PathBuf>,
}
impl Inventory {
    pub fn load(root: &Path) -> io::Result<Self> {
        regular_file_or_missing(&root.join("inventory.json"))?;
        match fs::read(root.join("inventory.json")) {
            Ok(bytes) => {
                let mut inventory: Self =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                inventory.validate_exclusions()?;
                Ok(inventory)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }
    fn validate_exclusions(&mut self) -> io::Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for entry in &mut self.exclusions {
            // Older live inventories can recover provenance from their owned links.
            // An orphan without provenance is unverifiable and must not be mutated.
            if entry.relative.as_os_str().is_empty() || entry.checkouts.is_empty() {
                for link in &self.links {
                    if link.exclude == entry.path
                        && exclusion_pattern(&link.relative)? == entry.pattern
                    {
                        entry.relative = link.relative.clone();
                        if !entry.checkouts.contains(&link.checkout) {
                            entry.checkouts.push(link.checkout.clone());
                        }
                    }
                }
            }
            super::validate_destination(&entry.relative)?;
            let id = entry
                .block
                .strip_prefix("\n# herdr skill sharing ")
                .and_then(|rest| rest.split_once('\n'))
                .and_then(|(id, _)| id.parse::<u64>().ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid exclusion ownership block",
                    )
                })?;
            if !entry.path.is_absolute()
                || entry.checkouts.is_empty()
                || entry
                    .checkouts
                    .iter()
                    .any(|checkout| !checkout.is_absolute())
                || entry.pattern != exclusion_pattern(&entry.relative)?
                || id == 0
                || id > self.sequence
                || !ids.insert(id)
                || entry.block != format!("\n# herdr skill sharing {id}\n{}\n", entry.pattern)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid exclusion ownership record",
                ));
            }
        }
        Ok(())
    }
    pub fn save(&self, root: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        let path = root.join("inventory.json");
        let temporary = root.join("inventory.tmp");
        regular_file_or_missing(&temporary)?;
        regular_file_or_missing(&path)?;
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        crate::platform::replace_file(&temporary, &path)?;
        crate::platform::sync_parent_directory(root)
    }
    pub fn ensure_exclusion(&mut self, root: &Path, link: &Link) -> io::Result<()> {
        let pattern = exclusion_pattern(&link.relative)?;
        let content = read_exclusions(&link.exclude)?;
        if let Some(index) = self
            .exclusions
            .iter()
            .position(|entry| entry.path == link.exclude && entry.pattern == pattern)
        {
            if !self.exclusions[index].checkouts.contains(&link.checkout) {
                self.exclusions[index].checkouts.push(link.checkout.clone());
                self.save(root)?;
            }
            let entry = &self.exclusions[index];
            if !content.contains(&entry.block) {
                append_exclusion(&link.exclude, &entry.block)?;
            }
            return Ok(());
        }
        if content.lines().any(|line| line == pattern) {
            return Ok(());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("skill inventory sequence exhausted"))?;
        // The leading newline is part of our owned block, preserving a user's missing final newline.
        let block = format!("\n# herdr skill sharing {}\n{pattern}\n", self.sequence);
        self.exclusions.push(Exclusion {
            path: link.exclude.clone(),
            pattern,
            block: block.clone(),
            relative: link.relative.clone(),
            checkouts: vec![link.checkout.clone()],
        });
        self.save(root)?;
        append_exclusion(&link.exclude, &block)
    }
    pub fn prune_exclusions(
        &mut self,
        root: &Path,
        checkout: &Path,
        exclude: &Path,
    ) -> io::Result<()> {
        let mut index = 0;
        while index < self.exclusions.len() {
            let entry = &self.exclusions[index];
            // Only the Git path authenticated by this operation is writable.
            if entry.path != exclude || !entry.checkouts.iter().any(|owner| owner == checkout) {
                index += 1;
                continue;
            }
            let mut needed = false;
            for link in &self.links {
                if link.exclude == entry.path && exclusion_pattern(&link.relative)? == entry.pattern
                {
                    needed = true;
                    break;
                }
            }
            if needed {
                index += 1;
                continue;
            }
            let content = read_exclusions(&entry.path)?;
            if content.contains(&entry.block) {
                let updated = content.replacen(&entry.block, "", 1);
                replace_exclusions_with(&entry.path, &content, |file| {
                    file.write_all(updated.as_bytes())
                })?;
            }
            self.exclusions.remove(index);
            self.save(root)?;
        }
        Ok(())
    }
}
fn read_exclusions(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}
fn append_exclusion(path: &Path, block: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = read_exclusions(path)?;
    replace_exclusions_with(path, &content, |file| {
        file.write_all(content.as_bytes())?;
        file.write_all(block.as_bytes())
    })
}
pub(super) fn exclusion_pattern(relative: &Path) -> io::Result<String> {
    let text = relative
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "skill names must be UTF-8"))?;
    if text.contains(['\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "skill names cannot contain line breaks",
        ));
    }
    let mut result = String::from("/");
    for component in relative.components() {
        if result.len() > 1 {
            result.push('/');
        }
        let part = component.as_os_str().to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "skill names must be UTF-8")
        })?;
        for character in part.chars() {
            if matches!(character, '\\' | '*' | '?' | '[' | ']' | '#' | '!' | ' ') {
                result.push('\\');
            }
            result.push(character);
        }
    }
    Ok(result)
}

pub(super) fn regular_file_or_missing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::other(
            "skill inventory entry must be a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn replace_exclusions_with(
    path: &Path,
    expected: &str,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    regular_file_or_missing(path)?;
    let existed = path.try_exists()?;
    if read_exclusions(path)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Git exclusions changed during preparation",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Git exclusions have no parent"))?;
    let (temporary, mut file) = loop {
        let id = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".herdr-skill-exclude-{}-{id}.tmp",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (PendingExclusion(temporary), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    if existed {
        file.set_permissions(fs::metadata(path)?.permissions())?;
    }
    write(&mut file)?;
    file.sync_all()?;
    drop(file);
    regular_file_or_missing(path)?;
    if path.try_exists()? != existed || read_exclusions(path)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Git exclusions changed during preparation",
        ));
    }
    crate::platform::replace_file(&temporary.0, path)?;
    crate::platform::sync_parent_directory(parent)
}

struct PendingExclusion(PathBuf);
impl Drop for PendingExclusion {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.0) {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %self.0.display(), "failed to remove staged Git exclusions");
            }
        }
    }
}
