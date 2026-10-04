use super::*;
use std::path::Path;

pub(crate) struct Fixture {
    pub root: PathBuf,
    pub parent: PathBuf,
    pub child: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("herdr-submodule-{}-{stamp}", std::process::id()));
        let parent = root.join("parent repo");
        let source = root.join("child source");
        for repo in [&parent, &source] {
            std::fs::create_dir_all(repo).unwrap();
            git(repo, &["init", "-q"]);
            git(repo, &["config", "user.email", "test@example.invalid"]);
            git(repo, &["config", "user.name", "Test"]);
            std::fs::write(repo.join("file"), "test").unwrap();
            git(repo, &["add", "."]);
            git(repo, &["commit", "-qm", "initial"]);
        }
        git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--",
                source.to_str().unwrap(),
                "nested/child module",
            ],
        );
        git(&parent, &["commit", "-qam", "submodule"]);
        let parent = std::fs::canonicalize(parent).unwrap();
        let child = parent.join("nested/child module");
        Self {
            root,
            parent,
            child,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
pub(crate) fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn submodule_discovery_uses_registered_gitlinks_not_nesting() {
    let fixture = Fixture::new();
    let (_, entries) = discover(&fixture.parent).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].initialized);
    assert!(select(&fixture.parent, "nested").is_err());
    assert!(select(&fixture.parent, "../child source").is_err());
    git(
        &fixture.parent,
        &[
            "config",
            "--file",
            ".gitmodules",
            "submodule.fake.path",
            "file",
        ],
    );
    assert_eq!(discover(&fixture.parent).unwrap().1.len(), 1);
    git(
        &fixture.parent,
        &["submodule", "deinit", "-f", "--", "nested/child module"],
    );
    assert!(!discover(&fixture.parent).unwrap().1[0].initialized);
    assert!(select(&fixture.parent, "nested/child module")
        .unwrap_err()
        .contains("uninitialized"));
}

#[test]
fn submodule_discovery_retains_exact_parent_worktree() {
    let fixture = Fixture::new();
    let parent_worktree = fixture.root.join("parent worktree");
    git(
        &fixture.parent,
        &[
            "worktree",
            "add",
            "--detach",
            parent_worktree.to_str().unwrap(),
        ],
    );
    git(
        &parent_worktree,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
        ],
    );
    let context = select(&parent_worktree, "nested/child module").unwrap();
    assert_eq!(
        context.parent_path,
        std::fs::canonicalize(&parent_worktree).unwrap()
    );
    assert_eq!(
        context.checkout_path,
        context.parent_path.join("nested/child module")
    );
    assert_ne!(context.parent_path, fixture.parent);
}

#[test]
fn submodule_context_rejects_removed_parent_registration() {
    let fixture = Fixture::new();
    let mut context = select(&fixture.parent, "nested/child module").unwrap();
    std::fs::remove_file(fixture.parent.join(".gitmodules")).unwrap();
    context.prepare();
    assert_eq!(context.status, "unavailable");
    assert!(context.messages[0].contains("not a registered submodule"));
}
