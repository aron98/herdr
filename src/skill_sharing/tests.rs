use super::*;
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    request: SharingRequest,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "herdr-sharing-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let parent = root.join("parent with spaces");
        let checkout = root.join("child");
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir_all(&checkout).unwrap();
        git(&checkout, &["init", "-q"]);
        Self {
            root,
            request: SharingRequest {
                parent,
                checkout,
                codex_sources: vec![],
                claude_sources: vec![],
            },
        }
    }
    fn skill(&self, catalog: &str, name: &str) -> PathBuf {
        let path = self.request.parent.join(catalog).join("skills").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), name).unwrap();
        path
    }
    fn refresh(&self) -> SharingReport {
        reconcile(
            &self.root.join("inventory"),
            &self.request.checkout,
            Some(&self.request),
        )
        .unwrap()
    }
    fn disable(&self) -> SharingReport {
        reconcile(&self.root.join("inventory"), &self.request.checkout, None).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
#[test]
fn native_catalogs_override_common_only_for_claude() {
    let f = Fixture::new();
    let common = f.skill(".agents", "shared");
    let native = f.skill(".claude", "shared");
    let report = f.refresh();
    assert_eq!(report.status, "ready");
    assert_eq!(
        fs::read_link(f.request.checkout.join(".agents/skills/shared")).unwrap(),
        common
    );
    assert_eq!(
        fs::read_link(f.request.checkout.join(".claude/skills/shared")).unwrap(),
        native
    );
    assert!(git(&f.request.checkout, &["status", "--porcelain"]).is_empty());
}
#[test]
fn preserves_existing_and_tracked_missing_entries() {
    let f = Fixture::new();
    f.skill(".agents", "existing");
    f.skill(".agents", "tracked");
    let base = f.request.checkout.join(".agents/skills");
    fs::create_dir_all(base.join("existing")).unwrap();
    fs::create_dir_all(base.join("tracked")).unwrap();
    fs::write(base.join("tracked/SKILL.md"), "local").unwrap();
    git(&f.request.checkout, &["add", "."]);
    fs::remove_dir_all(base.join("tracked")).unwrap();
    let report = f.refresh();
    assert_eq!(report.status, "conflicts");
    assert!(!fs::symlink_metadata(base.join("existing"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!base.join("tracked").exists());
}
#[test]
fn refresh_reconciles_removed_skills_and_native_precedence() {
    let f = Fixture::new();
    let common = f.skill(".agents", "shared");
    let native = f.skill(".claude", "shared");
    f.refresh();
    fs::remove_dir_all(native).unwrap();
    f.refresh();
    assert_eq!(
        fs::read_link(f.request.checkout.join(".claude/skills/shared")).unwrap(),
        common
    );
    fs::remove_dir_all(common).unwrap();
    f.refresh();
    assert!(!f.request.checkout.join(".agents/skills/shared").exists());
}
#[test]
fn disable_preserves_user_replacements() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    f.refresh();
    let path = f.request.checkout.join(".agents/skills/shared");
    crate::platform::remove_directory_link(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let report = f.disable();
    assert_eq!(report.status, "disabled");
    assert!(path.is_dir());
    assert!(!f.request.checkout.join(".claude/skills/shared").exists());
}
#[test]
fn rejects_destination_symlink_ancestors() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    let outside = f.root.join("outside");
    fs::create_dir(&outside).unwrap();
    crate::platform::create_directory_link(&outside, &f.request.checkout.join(".agents")).unwrap();
    assert_eq!(f.refresh().status, "conflicts");
    assert!(!outside.join("skills").exists());
}
#[test]
fn reports_missing_parent_as_unavailable() {
    let f = Fixture::new();
    fs::remove_dir_all(&f.request.parent).unwrap();
    assert_eq!(f.refresh().status, "unavailable");
}
#[test]
fn shared_exclusions_survive_disabling_one_worktree() {
    let f = Fixture::new();
    f.skill(".agents", "shared [one]");
    fs::write(f.request.checkout.join("tracked"), "base").unwrap();
    git(&f.request.checkout, &["add", "."]);
    git(
        &f.request.checkout,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "base",
        ],
    );
    let other = f.root.join("other");
    git(
        &f.request.checkout,
        &["worktree", "add", "-qb", "other", other.to_str().unwrap()],
    );
    f.refresh();
    let request = SharingRequest {
        parent: f.request.parent.clone(),
        checkout: other.clone(),
        codex_sources: vec![],
        claude_sources: vec![],
    };
    reconcile(&f.root.join("inventory"), &other, Some(&request)).unwrap();
    f.disable();
    assert!(git(&other, &["status", "--porcelain"]).is_empty());
    reconcile(&f.root.join("inventory"), &other, None).unwrap();
    let exclusion = fs::read_to_string(f.request.checkout.join(".git/info/exclude")).unwrap();
    assert!(!exclusion.contains("herdr skill sharing"));
}
#[test]
fn preexisting_exclusion_content_is_preserved() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    let path = f.request.checkout.join(".git/info/exclude");
    let original = b"# user rules\n/.agents/skills/shared\nkeep-me";
    fs::write(&path, original).unwrap();
    f.refresh();
    f.disable();
    assert_eq!(fs::read(path).unwrap(), original);
}
#[test]
fn preexisting_identical_link_is_never_adopted() {
    let f = Fixture::new();
    let target = f.skill(".agents", "shared");
    let path = f.request.checkout.join(".agents/skills/shared");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::platform::create_directory_link(&target, &path).unwrap();
    assert_eq!(f.refresh().status, "conflicts");
    f.disable();
    assert_eq!(fs::read_link(path).unwrap(), target);
}
#[test]
fn rejects_tracked_missing_ancestor_file() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    let path = f.request.checkout.join(".agents");
    fs::write(&path, "tracked config").unwrap();
    git(&f.request.checkout, &["add", "."]);
    fs::remove_file(&path).unwrap();
    assert_eq!(f.refresh().status, "conflicts");
    assert!(!path.exists());
}
#[test]
fn additional_sources_do_not_replace_native_skills() {
    let mut f = Fixture::new();
    let native = f.skill(".agents", "shared");
    let extra = f.root.join("extra");
    fs::create_dir_all(extra.join("shared")).unwrap();
    fs::create_dir_all(extra.join("added")).unwrap();
    f.request.codex_sources.push(extra.clone());
    f.refresh();
    assert_eq!(
        fs::read_link(f.request.checkout.join(".agents/skills/shared")).unwrap(),
        native
    );
    assert_eq!(
        fs::read_link(f.request.checkout.join(".agents/skills/added")).unwrap(),
        extra.join("added")
    );
    assert!(!f.request.checkout.join(".claude/skills/added").exists());
}

#[test]
fn disable_recovers_link_intent_after_interrupted_creation() {
    let f = Fixture::new();
    let target = f.skill(".agents", "shared");
    let root = f.root.join("inventory");
    fs::create_dir(&root).unwrap();
    let link = Link {
        checkout: f.request.checkout.clone(),
        relative: PathBuf::from(".agents/skills/shared"),
        target,
        exclude: f.request.checkout.join(".git/info/exclude"),
    };
    let mut inventory = Inventory::default();
    inventory.links.push(link.clone());
    inventory.save(&root).unwrap();
    inventory.ensure_exclusion(&root, &link).unwrap();
    fs::create_dir_all(f.request.checkout.join(".agents/skills")).unwrap();
    crate::platform::create_directory_link(&link.target, &f.request.checkout.join(&link.relative))
        .unwrap();
    f.disable();
    assert!(fs::symlink_metadata(f.request.checkout.join(&link.relative)).is_err());
    assert!(!fs::read_to_string(&link.exclude)
        .unwrap()
        .contains("herdr skill sharing"));
}

#[test]
fn refresh_recovers_exclusion_intent_before_file_write() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    f.refresh();
    let root = f.root.join("inventory");
    let inventory = Inventory::load(&root).unwrap();
    let path = &inventory.exclusions[0].path;
    let content = fs::read_to_string(path).unwrap();
    fs::write(path, content.replace(&inventory.exclusions[0].block, "")).unwrap();
    f.refresh();
    assert!(git(&f.request.checkout, &["status", "--porcelain"]).is_empty());
}

#[test]
fn disable_preserves_newly_tracked_owned_link() {
    let f = Fixture::new();
    let target = f.skill(".agents", "shared");
    f.refresh();
    git(&f.request.checkout, &["add", "-f", ".agents/skills/shared"]);
    f.disable();
    assert_eq!(
        fs::read_link(f.request.checkout.join(".agents/skills/shared")).unwrap(),
        target
    );
}

#[test]
fn relative_sources_resolve_against_parent_checkout() {
    let mut f = Fixture::new();
    let extra = f.request.parent.join("more skills");
    fs::create_dir_all(extra.join("added")).unwrap();
    f.request.claude_sources.push(PathBuf::from("more skills"));
    f.refresh();
    assert_eq!(
        fs::read_link(f.request.checkout.join(".claude/skills/added")).unwrap(),
        extra.join("added")
    );
}

#[test]
fn resolves_git_exclusions_for_real_submodule_checkout() {
    let mut f = Fixture::new();
    let target = f.skill(".agents", "shared");
    fs::write(f.request.checkout.join("tracked"), "base").unwrap();
    git(&f.request.checkout, &["add", "."]);
    git(
        &f.request.checkout,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "base",
        ],
    );
    git(&f.request.parent, &["init", "-q"]);
    git(
        &f.request.parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            f.request.checkout.to_str().unwrap(),
            "modules/child",
        ],
    );
    f.request.checkout = f.request.parent.join("modules/child");
    assert!(f.request.checkout.join(".git").is_file());
    let report = f.refresh();
    assert_eq!(report.status, "ready");
    assert_eq!(
        fs::read_link(f.request.checkout.join(".agents/skills/shared")).unwrap(),
        target
    );
    assert!(git(&f.request.checkout, &["status", "--porcelain"]).is_empty());
}

#[test]
fn rejects_symlinked_inventory_directory() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    let outside = f.root.join("outside");
    fs::create_dir(&outside).unwrap();
    crate::platform::create_directory_link(&outside, &f.root.join("inventory")).unwrap();
    assert!(reconcile(
        &f.root.join("inventory"),
        &f.request.checkout,
        Some(&f.request)
    )
    .is_err());
    assert!(!outside.join("lock").exists());
}

#[test]
fn refuses_cleanup_after_git_administration_changes() {
    let f = Fixture::new();
    let target = f.skill(".agents", "shared");
    f.refresh();
    fs::rename(f.request.checkout.join(".git"), f.root.join("old-git")).unwrap();
    fs::write(
        f.request.checkout.join(".git"),
        format!("gitdir: {}", f.root.join("old-git").display()),
    )
    .unwrap();
    assert_eq!(f.disable().status, "conflicts");
    assert_eq!(
        fs::read_link(f.request.checkout.join(".agents/skills/shared")).unwrap(),
        target
    );
}

#[test]
fn rejects_inventory_paths_outside_skill_directories() {
    let f = Fixture::new();
    let root = f.root.join("inventory");
    fs::create_dir(&root).unwrap();
    let outside = f.request.checkout.join("user-link");
    crate::platform::create_directory_link(&f.request.parent, &outside).unwrap();
    let mut inventory = Inventory::default();
    inventory.links.push(Link {
        checkout: f.request.checkout.clone(),
        relative: PathBuf::from("user-link"),
        target: f.request.parent.clone(),
        exclude: f.request.checkout.join(".git/info/exclude"),
    });
    inventory.save(&root).unwrap();
    assert_eq!(f.disable().status, "conflicts");
    assert!(fs::read_link(outside).is_ok());
}

#[test]
fn corrupted_orphan_exclusion_cannot_erase_unrelated_file() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    f.refresh();
    let unrelated = f.root.join("user-notes");
    let original = "valuable user contents\n";
    fs::write(&unrelated, original).unwrap();
    let path = f.root.join("inventory/inventory.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    journal["links"] = serde_json::json!([]);
    journal["exclusions"] = serde_json::json!([{"path": unrelated, "pattern": "/.agents/skills/shared", "block": original}]);
    fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let result = reconcile(&f.root.join("inventory"), &f.request.checkout, None);
    assert_eq!(fs::read_to_string(unrelated).unwrap(), original);
    assert!(result.is_err());
}

#[test]
fn redirected_exclusion_record_cannot_remove_valid_block_from_user_file() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    f.refresh();
    let path = f.root.join("inventory/inventory.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let unrelated = f.root.join("user-notes");
    let original = journal["exclusions"][0]["block"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(&unrelated, &original).unwrap();
    journal["links"] = serde_json::json!([]);
    journal["exclusions"][0]["path"] = serde_json::json!(unrelated);
    fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let result = reconcile(&f.root.join("inventory"), &f.request.checkout, None);
    assert_eq!(fs::read_to_string(unrelated).unwrap(), original);
    assert!(result.is_err() || result.unwrap().status == "conflicts");
}

#[test]
fn exclusion_write_failure_preserves_original_bytes_and_retry() {
    use std::io::Write;
    let f = Fixture::new();
    let path = f.request.checkout.join(".git/info/exclude");
    let original = "# user rules\nprivate-a\nprivate-b\nprivate-c\n";
    fs::write(&path, original).unwrap();
    let result = inventory::replace_exclusions_with(&path, original, |file| {
        file.write_all(b"partial")?;
        Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "injected failure after partial write",
        ))
    });
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    inventory::replace_exclusions_with(&path, original, |file| file.write_all(b"updated\n"))
        .unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "updated\n");
}

#[test]
fn exclusion_replacement_preserves_intervening_user_edit() {
    use std::io::Write;
    let f = Fixture::new();
    let path = f.request.checkout.join(".git/info/exclude");
    fs::write(&path, "original\n").unwrap();
    let result = inventory::replace_exclusions_with(&path, "original\n", |file| {
        fs::write(&path, "user edit\n")?;
        file.write_all(b"generated\n")
    });
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "user edit\n");
}

#[cfg(unix)]
#[test]
fn git_exclusion_symlink_keeps_custom_filename_and_user_link() {
    let f = Fixture::new();
    f.skill(".agents", "shared");
    let actual = f.root.join("custom-ignore");
    let original = "# user excludes\nkeep-me\n";
    fs::write(&actual, original).unwrap();
    let link = f.request.checkout.join(".git/info/exclude");
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&actual, &link).unwrap();
    assert_eq!(f.refresh().status, "ready");
    assert!(git(&f.request.checkout, &["status", "--porcelain"]).is_empty());
    f.disable();
    assert_eq!(fs::read_to_string(actual).unwrap(), original);
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    assert!(!f.root.join("exclude").exists());
}
