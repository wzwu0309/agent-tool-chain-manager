//! Resolve a managed Skill's immutable content from a central-store Git commit.
//!
//! The central working directory may have changes that have not been committed.
//! Deployment must read the selected *commit* and the Skill subtree recorded
//! in that commit's tracked metadata, never treat working-tree bytes as that
//! version. This module only resolves version references; deployment and
//! database ownership are separate concerns.

use anyhow::{bail, Context, Result};
use git2::{ObjectType, Oid, Repository};
use serde::Serialize;
use std::path::{Component, Path};

use super::sync_metadata::SkillMetaFile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSkillVersion {
    pub skill_id: String,
    /// Full commit OID in the central store, not the Skill's own version number.
    pub commit_oid: String,
    /// OID of the selected Skill's *subtree*, not the repository root tree.
    pub tree_oid: String,
    /// Relative path as it existed in the selected commit.
    pub path_at_commit: String,
}

/// Resolve a Skill by its stable ID at a selected full Git commit OID, or at
/// HEAD if no commit was requested. Does not read or modify the working tree.
///
/// Explicit commits must belong to HEAD's ancestry: a stale/orphaned or
/// unrelated commit must not be silently offered as the current Skill version.
/// Git history can contain multiple commits with the same Skill tree.
pub fn resolve_skill_version(
    skills_dir: &Path,
    skill_id: &str,
    commit_oid: Option<&str>,
) -> Result<GitSkillVersion> {
    if !valid_name_component(skill_id) {
        bail!("invalid SkillId");
    }

    let repo = open_exact_store(skills_dir)?;
    let head = repo
        .head()
        .context("central Skill Git repository has no HEAD")?
        .peel_to_commit()
        .context("central Skill Git HEAD has no commit")?;

    let commit = match commit_oid {
        None => head.clone(),
        Some(raw) => {
            let oid = Oid::from_str(raw).context("invalid Git CommitId")?;
            let selected = repo
                .find_commit(oid)
                .context("selected Git CommitId is not available locally")?;
            if selected.id() != head.id() && !repo.graph_descendant_of(head.id(), selected.id())? {
                bail!("selected Git CommitId is not an ancestor of current HEAD");
            }
            selected
        }
    };

    resolve_skill_version_from_commit(&repo, skill_id, &commit)
}

/// Resolve an immutable historical commit without HEAD ancestry gating.
/// Only for validating a baseline already recorded by this application:
/// users still select new deployment versions through the public resolver.
fn resolve_skill_version_from_commit(
    repo: &Repository,
    skill_id: &str,
    commit: &git2::Commit<'_>,
) -> Result<GitSkillVersion> {
    if !valid_name_component(skill_id) {
        bail!("invalid SkillId");
    }
    let tree = commit.tree().context("selected commit has no tree")?;
    let meta_path = format!(".skills-manager/skills/{skill_id}.json");
    let meta_entry = tree
        .get_path(Path::new(&meta_path))
        .with_context(|| format!("SkillId {skill_id} is not registered at selected commit"))?;
    if meta_entry.kind() != Some(ObjectType::Blob) || meta_entry.filemode() != 0o100644 {
        bail!("Skill metadata at selected commit is not a regular file");
    }
    let meta_blob = repo.find_blob(meta_entry.id())?;
    let meta: SkillMetaFile = serde_json::from_slice(meta_blob.content())
        .with_context(|| format!("malformed Skill metadata for {skill_id}"))?;
    if meta.skill_id != skill_id {
        bail!("Skill metadata ID mismatch at selected commit");
    }
    if !valid_relative_skill_path(&meta.path) {
        bail!("invalid Skill path at selected commit");
    }

    let content_entry = tree.get_path(Path::new(&meta.path)).with_context(|| {
        format!(
            "SkillId {skill_id} has no content directory at {} in selected commit",
            meta.path
        )
    })?;
    if content_entry.kind() != Some(ObjectType::Tree) {
        bail!("Skill content at selected commit is not a directory");
    }

    Ok(GitSkillVersion {
        skill_id: skill_id.to_owned(),
        commit_oid: commit.id().to_string(),
        tree_oid: content_entry.id().to_string(),
        path_at_commit: meta.path,
    })
}

/// Bounded, read-only Git history for one Skill, not a global snapshot list.
/// Only the current HEAD ancestry is selectable by the standard deploy Core.
/// A historical CommitId is always passed in full to the existing resolver.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSkillHistory {
    pub versions: Vec<GitSkillHistoryItem>,
    pub uncommitted_central_changes_excluded: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSkillHistoryItem {
    pub commit_oid: String,
    pub tree_oid: String,
    pub path_at_commit: String,
    pub message: String,
    pub committed_at: String,
    pub is_head: bool,
}

pub fn list_skill_history(
    skills_dir: &Path,
    skill_id: &str,
    limit: usize,
) -> Result<GitSkillHistory> {
    if !valid_name_component(skill_id) {
        bail!("invalid SkillId for history query");
    }
    let repo = open_exact_store(skills_dir)?;
    let head = repo.head()?.peel_to_commit()?.id();
    let mut walk = repo.revwalk()?;
    walk.push(head)?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    let mut versions = Vec::new();
    // Bound both returned results and traversal for large central stores.
    for result in walk.take(1000) {
        let oid = result?;
        let commit = repo.find_commit(oid)?;
        let version = match resolve_skill_version_from_commit(&repo, skill_id, &commit) {
            Ok(version) => version,
            Err(_) => continue,
        };
        let committed_at = chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
            .map(|time| time.to_rfc3339())
            .unwrap_or_default();
        versions.push(GitSkillHistoryItem {
            commit_oid: version.commit_oid,
            tree_oid: version.tree_oid,
            path_at_commit: version.path_at_commit,
            message: commit.summary().unwrap_or_default().to_string(),
            committed_at,
            is_head: oid == head,
        });
        if versions.len() >= limit.clamp(1, 50) {
            break;
        }
    }
    Ok(GitSkillHistory {
        versions,
        uncommitted_central_changes_excluded: has_uncommitted_central_changes(skills_dir)?,
    })
}

/// Local immutable reachability anchor for each deployed Git commit.
/// Each deployment can accumulate multiple commit pins during updates.
/// Pins are never implicitly deleted while recovery data might be needed.
const DEPLOYMENT_PIN_PREFIX: &str = "refs/skills-manager/project-deployments/";

pub fn deployment_pin_ref(deployment_id: &str, commit_oid: &str) -> Result<String> {
    let uuid = uuid::Uuid::parse_str(deployment_id)
        .context("invalid managed Project DeploymentId for Git pin")?;
    let commit = Oid::from_str(commit_oid).context("invalid Git commit OID to pin")?;
    Ok(format!("{DEPLOYMENT_PIN_PREFIX}{uuid}/{commit}"))
}

/// Create a persistent ref *before* publishing any project bytes or changing
/// SQLite's baseline. Failed deployments may leave extra safe pins behind.
pub fn pin_deployment_version(
    skills_dir: &Path,
    deployment_id: &str,
    commit_oid: &str,
) -> Result<()> {
    let ref_name = deployment_pin_ref(deployment_id, commit_oid)?;
    let repo = open_exact_store(skills_dir)?;
    let oid = Oid::from_str(commit_oid)?;
    repo.find_commit(oid)
        .with_context(|| format!("cannot pin missing Git CommitId {commit_oid}"))?;
    match repo.find_reference(&ref_name) {
        Ok(existing) if existing.target() == Some(oid) => return Ok(()),
        Ok(_) => bail!("Git deployment version pin conflicts with existing reference"),
        Err(err) if err.code() == git2::ErrorCode::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    repo.reference(&ref_name, oid, false, "pin deployed Project Skill version")
        .context("failed to persist Git deployment version pin")?;
    if repo.find_reference(&ref_name)?.target() != Some(oid) {
        bail!("Git deployment version pin verification failed");
    }
    Ok(())
}

pub fn verify_deployment_pin(
    skills_dir: &Path,
    deployment_id: &str,
    commit_oid: &str,
) -> Result<()> {
    let name = deployment_pin_ref(deployment_id, commit_oid)?;
    let repo = open_exact_store(skills_dir)?;
    let target = Oid::from_str(commit_oid)?;
    let reference = repo
        .find_reference(&name)
        .with_context(|| format!("Git deployment pin missing for {deployment_id}"))?;
    if reference.target() != Some(target) {
        bail!("Git deployment pin does not match the stored CommitId");
    }
    repo.find_commit(target)
        .context("Git deployment baseline CommitId is no longer present locally")?;
    Ok(())
}

/// Validate a previously recorded immutable Copy baseline without requiring
/// its CommitId to remain in HEAD ancestry. The deployment-specific pin must
/// already exist; this read-only check never creates or moves refs.
pub fn verify_recorded_skill_base(
    skills_dir: &Path,
    deployment_id: &str,
    skill_id: &str,
    commit_oid: &str,
    tree_oid: &str,
    path_at_commit: &str,
) -> Result<()> {
    verify_deployment_pin(skills_dir, deployment_id, commit_oid)?;
    let repo = open_exact_store(skills_dir)?;
    let commit = repo.find_commit(Oid::from_str(commit_oid)?)?;
    let version = resolve_skill_version_from_commit(&repo, skill_id, &commit)?;
    if version.tree_oid != tree_oid || version.path_at_commit != path_at_commit {
        bail!("Stored Copy Git Commit/Tree/path baseline no longer matches the registered SkillId");
    }
    Ok(())
}

/// Backfill versions previously deployed without pins. Never substitute HEAD
/// for a missing historical commit: report object loss as a recovery error.
pub fn reconcile_deployment_pins(
    skills_dir: &Path,
    store: &super::skill_store::SkillStore,
) -> Result<Vec<String>> {
    let rows = store.get_managed_project_copy_version_refs()?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut problems = Vec::new();
    for row in rows {
        let attempt = (|| -> Result<()> {
            let commit_oid = row
                .commit_oid
                .as_deref()
                .context("missing recorded CommitId")?;
            let tree_oid = row
                .tree_oid
                .as_deref()
                .context("missing recorded Skill TreeOid")?;
            let old_path = row
                .path_at_commit
                .as_deref()
                .context("missing recorded PathAtCommit")?;
            let repo = open_exact_store(skills_dir)?;
            let commit = repo
                .find_commit(Oid::from_str(commit_oid)?)
                .context("recorded Git commit object is unavailable")?;
            let selected = resolve_skill_version_from_commit(&repo, &row.skill_id, &commit)?;
            if selected.tree_oid != tree_oid || selected.path_at_commit != old_path {
                bail!("stored SkillId/TreeOid/PathAtCommit do not match the recorded Git commit");
            }
            pin_deployment_version(skills_dir, &row.deployment_id, commit_oid)?;
            Ok(())
        })();
        if let Err(err) = attempt {
            problems.push(format!(
                "deployment {} cannot retain trustworthy Git baseline: {err:#}",
                row.deployment_id,
            ));
        }
    }
    Ok(problems)
}

/// Re-clone must not discard a central object database containing local pins.
/// Remote backup deliberately omits these hidden refs. Even if a commit object
/// happens to exist in the new clone, its future GC safety is unproven.
pub fn has_local_deployment_pins(skills_dir: &Path) -> Result<bool> {
    let repo = open_exact_store(skills_dir)?;
    let refs = repo.references_glob(&format!("{DEPLOYMENT_PIN_PREFIX}*"))?;
    for reference in refs {
        let reference = reference?;
        if reference
            .name()
            .is_some_and(|name| name.starts_with(DEPLOYMENT_PIN_PREFIX))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Informational only: deployment still uses committed Git blobs, never
/// uncommitted central working-tree bytes (including untracked files).
pub fn has_uncommitted_central_changes(skills_dir: &Path) -> Result<bool> {
    let repo = open_exact_store(skills_dir)?;
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut options))?;
    Ok(!statuses.is_empty())
}

fn open_exact_store(skills_dir: &Path) -> Result<Repository> {
    let repo = Repository::open(skills_dir).with_context(|| {
        format!(
            "central Skill store at {} is not an initialized Git repository; initialize it explicitly in Git Backup or run CLI git init",
            skills_dir.display()
        )
    })?;
    let workdir = repo
        .workdir()
        .context("central Skill Git repository is bare")?;
    if std::fs::canonicalize(workdir)? != std::fs::canonicalize(skills_dir)? {
        bail!("Central Skill store does not have its own Git repository");
    }
    Ok(repo)
}

fn valid_name_component(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn valid_relative_skill_path(raw: &str) -> bool {
    let path = Path::new(raw);
    !raw.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !path
            .components()
            .any(|component| component.as_os_str() == ".git")
}

/// Write the exact committed Skill subtree to a **new** destination. Neither
/// uncommitted source files nor ignored working-tree files are ever read.
/// Callers should materialize inside a same-parent staging directory and only
/// publish after verifying the result.
pub fn materialize_skill_version(
    skills_dir: &Path,
    version: &GitSkillVersion,
    destination: &Path,
) -> Result<()> {
    let repo = open_exact_store(skills_dir)?;
    let tree = repo.find_tree(Oid::from_str(&version.tree_oid)?)?;
    if std::fs::symlink_metadata(destination).is_ok() {
        bail!(
            "Skill deployment target already exists: {}",
            destination.display()
        );
    }
    std::fs::create_dir(destination)?;
    materialize_tree(&repo, &tree, destination, Path::new(""))?;
    validate_symlink_resolution(destination)?;
    if !matches_skill_tree(skills_dir, &version.tree_oid, destination)? {
        bail!("materialized Skill differs from selected Git Tree");
    }
    Ok(())
}

/// Compare the entire on-disk directory to the committed Git tree, without
/// following symlinks. Unlike the legacy content hash this includes hidden
/// files, executable bits, symlink targets, and unexpected empty directories.
pub fn matches_skill_tree(skills_dir: &Path, tree_oid: &str, target: &Path) -> Result<bool> {
    let repo = open_exact_store(skills_dir)?;
    let tree = repo.find_tree(Oid::from_str(tree_oid)?)?;
    if !match_tree(&repo, &tree, target)? {
        return Ok(false);
    }
    Ok(validate_symlink_resolution(target).is_ok())
}

fn materialize_tree(
    repo: &Repository,
    tree: &git2::Tree<'_>,
    target: &Path,
    relative: &Path,
) -> Result<()> {
    for entry in tree {
        let name = entry
            .name()
            .context("non-UTF8 Git filename is not supported")?;
        if name == ".git" || !valid_entry_name(name) {
            bail!("unsafe Git filename in Skill Tree");
        }
        let child = target.join(name);
        match entry.filemode() {
            0o040000 => {
                std::fs::create_dir(&child)?;
                let subtree = repo.find_tree(entry.id())?;
                materialize_tree(repo, &subtree, &child, &relative.join(name))?;
            }
            0o100644 | 0o100755 => {
                let blob = repo.find_blob(entry.id())?;
                std::fs::write(&child, blob.content())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if entry.filemode() == 0o100755 {
                        0o755
                    } else {
                        0o644
                    };
                    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(mode))?;
                }
            }
            0o120000 => {
                let blob = repo.find_blob(entry.id())?;
                let link =
                    std::str::from_utf8(blob.content()).context("symlink target is not UTF-8")?;
                if !safe_symlink_target(relative, link) {
                    bail!("Skill Tree contains an escaping or absolute symlink: {name}");
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(link, &child)?;
                #[cfg(not(unix))]
                bail!("Git Skill symlinks cannot be deployed on this platform");
            }
            _ => bail!("unsupported Git Tree entry mode {:o}", entry.filemode()),
        }
    }
    Ok(())
}

fn match_tree(repo: &Repository, tree: &git2::Tree<'_>, root: &Path) -> Result<bool> {
    let meta = match std::fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Ok(false);
    }
    let actual = std::fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    if actual.len() != tree.len() {
        return Ok(false);
    }
    for entry in tree {
        let name = entry
            .name()
            .context("non-UTF8 Git filename is not supported")?;
        if name == ".git" || !valid_entry_name(name) {
            bail!("unsafe Git filename in Skill Tree");
        }
        let path = root.join(name);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };
        let same = match entry.filemode() {
            0o040000 => match_tree(repo, &repo.find_tree(entry.id())?, &path)?,
            0o100644 | 0o100755 if meta.is_file() && !meta.file_type().is_symlink() => {
                #[cfg(unix)]
                let executable_matches = {
                    use std::os::unix::fs::PermissionsExt;
                    (meta.permissions().mode() & 0o111 != 0) == (entry.filemode() == 0o100755)
                };
                #[cfg(not(unix))]
                let executable_matches = true;
                executable_matches
                    && Oid::hash_object(ObjectType::Blob, &std::fs::read(&path)?)? == entry.id()
            }
            0o120000 if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&path)?;
                Oid::hash_object(ObjectType::Blob, link.to_string_lossy().as_bytes())? == entry.id()
            }
            _ => false,
        };
        if !same {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Lexical containment is not enough: a relative symlink may point through
/// another symlink that ultimately escapes the Skill root. Validate the full
/// chain after materialization, and reject dangling links or loops.
fn validate_symlink_resolution(root: &Path) -> Result<()> {
    let canonical_root = std::fs::canonicalize(root)?;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            let resolved = std::fs::canonicalize(entry.path()).with_context(|| {
                format!("unresolvable Skill symlink {}", entry.path().display())
            })?;
            if !resolved.starts_with(&canonical_root) {
                bail!("Skill symlink escapes the deployment root");
            }
        }
    }
    Ok(())
}

fn valid_entry_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

/// Every symlink stays inside this Skill root even after the deployment folder
/// is moved. Do not allow external project files to be read through a link.
fn safe_symlink_target(parent: &Path, link: &str) -> bool {
    let path = Path::new(link);
    if link.is_empty() || path.is_absolute() {
        return false;
    }
    let mut depth = parent.components().count();
    for part in path.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    struct Fixture {
        dir: tempfile::TempDir,
        repo: Repository,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let repo = Repository::init(dir.path()).unwrap();
            Self { dir, repo }
        }

        fn write_skill(&self, path: &str, text: &str) {
            let full = self.dir.path().join(path);
            fs::create_dir_all(&full).unwrap();
            fs::write(full.join("SKILL.md"), text).unwrap();
            let metadata = self
                .dir
                .path()
                .join(".skills-manager/skills/demo-skill.json");
            fs::create_dir_all(metadata.parent().unwrap()).unwrap();
            let value = serde_json::json!({
                "schema_version": 1,
                "skill_id": "demo-skill",
                "path": path,
                "path_key": path,
                "enabled": true,
                "tags": [],
                "source": {"type": "local", "ref": null, "subpath": null, "branch": null}
            });
            fs::write(metadata, serde_json::to_vec(&value).unwrap()).unwrap();
        }

        fn commit(&self, message: &str) -> Oid {
            let mut index = self.repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = self.repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("Test", "test@example.com").unwrap();
            let head = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
            let parents: Vec<&git2::Commit<'_>> = head.iter().collect();
            self.repo
                .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
                .unwrap()
        }
    }

    #[test]
    fn selected_commit_ignores_uncommitted_worktree_changes() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "committed");
        let first = fx.commit("initial");
        let at_first =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&first.to_string())).unwrap();
        assert_eq!(at_first.commit_oid, first.to_string());
        assert_eq!(at_first.path_at_commit, "toolkit/demo");
        assert_eq!(
            at_first.tree_oid,
            fx.repo
                .find_commit(first)
                .unwrap()
                .tree()
                .unwrap()
                .get_path(Path::new("toolkit/demo"))
                .unwrap()
                .id()
                .to_string()
        );

        fs::write(fx.dir.path().join("toolkit/demo/SKILL.md"), "uncommitted").unwrap();
        let default = resolve_skill_version(fx.dir.path(), "demo-skill", None).unwrap();
        assert_eq!(default, at_first);
    }

    #[test]
    fn skill_history_only_lists_selectable_head_ancestors_and_excludes_uncommitted_bytes() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "v1");
        let oldest = fx.commit("initial Skill version");
        fx.write_skill("toolkit/demo", "v2");
        let latest = fx.commit("second Skill version");
        fs::write(fx.dir.path().join("toolkit/demo/SKILL.md"), "uncommitted").unwrap();
        let history = list_skill_history(fx.dir.path(), "demo-skill", 25).unwrap();
        assert!(history.uncommitted_central_changes_excluded);
        assert_eq!(history.versions.len(), 2);
        assert_eq!(history.versions[0].commit_oid, latest.to_string());
        assert!(history.versions[0].is_head);
        assert_eq!(history.versions[1].commit_oid, oldest.to_string());
        assert!(!history.versions[1].is_head);
        for item in &history.versions {
            assert_eq!(
                resolve_skill_version(fx.dir.path(), "demo-skill", Some(&item.commit_oid))
                    .unwrap()
                    .tree_oid,
                item.tree_oid,
            );
        }
        assert_eq!(
            list_skill_history(fx.dir.path(), "demo-skill", 1)
                .unwrap()
                .versions
                .len(),
            1
        );
        assert!(list_skill_history(fx.dir.path(), "../escape", 20).is_err());
    }

    #[test]
    fn version_picker_never_offers_orphaned_commit_after_head_rewrite() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "original");
        let earlier = fx.commit("historical");
        fx.write_skill("toolkit/demo", "orphaned");
        let orphan = fx.commit("will be orphaned");
        let head_name = fx.repo.head().unwrap().name().unwrap().to_string();
        fx.repo
            .reference(&head_name, earlier, true, "rewrite HEAD")
            .unwrap();
        let history = list_skill_history(fx.dir.path(), "demo-skill", 50).unwrap();
        assert_eq!(history.versions.len(), 1);
        assert_eq!(history.versions[0].commit_oid, earlier.to_string());
        assert!(!history
            .versions
            .iter()
            .any(|v| v.commit_oid == orphan.to_string()));
        assert!(
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&orphan.to_string())).is_err()
        );
    }

    #[test]
    fn historic_skill_path_and_tree_are_taken_from_selected_commit() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/old", "v1");
        let old = fx.commit("first");
        fs::rename(
            fx.dir.path().join("toolkit/old"),
            fx.dir.path().join("toolkit/new"),
        )
        .unwrap();
        fx.write_skill("toolkit/new", "v2");
        let newer = fx.commit("move skill");

        let v1 =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&old.to_string())).unwrap();
        let v2 =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&newer.to_string())).unwrap();
        assert_eq!(v1.path_at_commit, "toolkit/old");
        assert_eq!(v2.path_at_commit, "toolkit/new");
        assert_ne!(v1.tree_oid, v2.tree_oid);
        assert_eq!(
            resolve_skill_version(fx.dir.path(), "demo-skill", None).unwrap(),
            v2
        );
    }

    #[test]
    fn rejects_unknown_skill_invalid_id_and_uninitialized_store() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "v1");
        fx.commit("first");
        assert!(resolve_skill_version(fx.dir.path(), "../another", None).is_err());
        assert!(resolve_skill_version(fx.dir.path(), "not-managed", None).is_err());
        let plain = tempfile::tempdir().unwrap();
        assert!(resolve_skill_version(plain.path(), "demo-skill", None).is_err());
    }

    #[test]
    fn materialize_is_exact_and_detects_every_unexpected_file() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "committed text");
        fs::write(fx.dir.path().join("toolkit/demo/.hidden"), "hidden").unwrap();
        fs::create_dir(fx.dir.path().join("toolkit/demo/refs")).unwrap();
        fs::write(fx.dir.path().join("toolkit/demo/refs/guide.md"), "guide").unwrap();
        let commit = fx.commit("snapshot");
        let version =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&commit.to_string())).unwrap();

        fs::write(fx.dir.path().join("toolkit/demo/SKILL.md"), "uncommitted").unwrap();
        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("deployed");
        materialize_skill_version(fx.dir.path(), &version, &dest).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("SKILL.md")).unwrap(),
            "committed text"
        );
        assert!(matches_skill_tree(fx.dir.path(), &version.tree_oid, &dest).unwrap());
        fs::create_dir(dest.join("unexpected-empty")).unwrap();
        assert!(!matches_skill_tree(fx.dir.path(), &version.tree_oid, &dest).unwrap());
        fs::remove_dir(dest.join("unexpected-empty")).unwrap();
        fs::write(dest.join(".hidden"), "edited").unwrap();
        assert!(!matches_skill_tree(fx.dir.path(), &version.tree_oid, &dest).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn safe_symlinks_work_and_escaping_symlinks_are_rejected() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "base");
        let refs = fx.dir.path().join("toolkit/demo/refs");
        fs::create_dir_all(&refs).unwrap();
        std::os::unix::fs::symlink("../SKILL.md", refs.join("local-link")).unwrap();
        let safe = fx.commit("safe link");
        let version =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&safe.to_string())).unwrap();
        let dst = tempfile::tempdir().unwrap();
        materialize_skill_version(fx.dir.path(), &version, &dst.path().join("safe")).unwrap();
        assert!(
            matches_skill_tree(fx.dir.path(), &version.tree_oid, &dst.path().join("safe"),)
                .unwrap()
        );

        std::os::unix::fs::symlink("/tmp", fx.dir.path().join("toolkit/demo/escape")).unwrap();
        let bad = fx.commit("escaping link");
        let bad_version =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&bad.to_string())).unwrap();
        assert!(
            materialize_skill_version(fx.dir.path(), &bad_version, &dst.path().join("reject"),)
                .is_err()
        );

        // Neither link is absolute, but following the second through the
        // first leaves the Skill root. A lexical-only check misses this.
        fs::remove_file(fx.dir.path().join("toolkit/demo/escape")).unwrap();
        std::os::unix::fs::symlink("..", fx.dir.path().join("toolkit/demo/refs/out")).unwrap();
        std::os::unix::fs::symlink("refs/out/..", fx.dir.path().join("toolkit/demo/chain"))
            .unwrap();
        let chained = fx.commit("chained escape");
        let chained_version =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&chained.to_string())).unwrap();
        assert!(materialize_skill_version(
            fx.dir.path(),
            &chained_version,
            &dst.path().join("chained"),
        )
        .is_err());
    }

    #[test]
    fn pinned_commit_survives_head_rewrite_and_real_git_gc() {
        use std::process::Command;
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "v1");
        let initial = fx.commit("initial");
        fx.write_skill("toolkit/demo", "v2");
        let pinned = fx.commit("deployment version");
        let deployment_id = uuid::Uuid::new_v4().to_string();
        pin_deployment_version(fx.dir.path(), &deployment_id, &pinned.to_string()).unwrap();
        pin_deployment_version(fx.dir.path(), &deployment_id, &pinned.to_string()).unwrap();
        verify_deployment_pin(fx.dir.path(), &deployment_id, &pinned.to_string()).unwrap();
        assert!(has_local_deployment_pins(fx.dir.path()).unwrap());
        let head_ref = fx.repo.head().unwrap().name().unwrap().to_string();
        fx.repo
            .reference(&head_ref, initial, true, "simulate history rewrite")
            .unwrap();
        let run = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(fx.dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["reflog", "expire", "--all", "--expire=now"]);
        run(&["gc", "--prune=now"]);
        verify_deployment_pin(fx.dir.path(), &deployment_id, &pinned.to_string()).unwrap();
        let version = fx.repo.find_commit(pinned).unwrap();
        assert_eq!(version.id(), pinned);
        // HEAD ancestry selection remains restrictive, even though the
        // historical object is intentionally retained for baseline recovery.
        assert!(
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&pinned.to_string()),).is_err()
        );
    }

    #[test]
    fn pins_are_deployment_scoped_immutable_and_never_retargeted() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "first");
        let commit = fx.commit("first");
        let id = uuid::Uuid::new_v4().to_string();
        let another = uuid::Uuid::new_v4().to_string();
        pin_deployment_version(fx.dir.path(), &id, &commit.to_string()).unwrap();
        pin_deployment_version(fx.dir.path(), &another, &commit.to_string()).unwrap();
        let ref_name = deployment_pin_ref(&id, &commit.to_string()).unwrap();
        assert_eq!(
            fx.repo.find_reference(&ref_name).unwrap().target(),
            Some(commit)
        );
        assert!(deployment_pin_ref("../../outside", &commit.to_string()).is_err());
        assert!(deployment_pin_ref(&id, "not-a-git-oid").is_err());
        fx.write_skill("toolkit/demo", "second");
        let unrelated_target = fx.commit("another valid commit");
        fx.repo
            .reference(
                &ref_name,
                unrelated_target,
                true,
                "simulate corrupted pin target",
            )
            .unwrap();
        assert!(verify_deployment_pin(fx.dir.path(), &id, &commit.to_string()).is_err());
        assert!(
            pin_deployment_version(fx.dir.path(), &id, &commit.to_string()).is_err(),
            "a conflicting pin must never be forcibly rewritten"
        );
    }

    #[test]
    fn same_skill_tree_can_have_distinct_commit_ids() {
        let fx = Fixture::new();
        fx.write_skill("toolkit/demo", "v1");
        let first = fx.commit("first");
        fs::write(fx.dir.path().join("README.md"), "unrelated").unwrap();
        let second = fx.commit("unrelated commit");
        let before =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&first.to_string())).unwrap();
        let after =
            resolve_skill_version(fx.dir.path(), "demo-skill", Some(&second.to_string())).unwrap();
        assert_ne!(before.commit_oid, after.commit_oid);
        assert_eq!(before.tree_oid, after.tree_oid);
    }
}
