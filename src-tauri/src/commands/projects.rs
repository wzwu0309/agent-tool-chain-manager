use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::core::audit_log::AuditDraft;
use crate::core::error::TargetConflictDetail;
use crate::core::repo_lock::RepoLock;
use crate::core::skill_store::{
    ProjectRecord, ProjectSkillDeploymentRecord, ProjectSkillOperationRecord, SkillRecord,
    SkillStore,
};
use crate::core::timing::should_log_first_or_slow;
use crate::core::{
    central_repo, error::AppError, file_manager, git_skill_versions, installer, project_scanner,
    sync_engine, sync_metadata, tool_adapters,
};

#[derive(Serialize, Default, Debug)]
pub struct SyncHealthDto {
    pub in_sync: usize,
    pub project_newer: usize,
    pub center_newer: usize,
    pub diverged: usize,
    pub project_only: usize,
}

#[derive(Serialize, Debug)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub path: String,
    pub workspace_type: String,
    pub linked_agent_name: Option<String>,
    pub supports_skill_toggle: bool,
    pub sort_order: i32,
    pub skill_count: usize,
    pub sync_health: SyncHealthDto,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct ProjectSkillDocumentDto {
    pub skill_name: String,
    pub filename: String,
    pub content: String,
}

#[derive(Serialize, Clone)]
pub struct ProjectAgentTargetDto {
    pub key: String,
    pub display_name: String,
    pub enabled: bool,
    pub installed: bool,
    pub is_custom: bool,
}

fn agent_skill_configs(store: &SkillStore) -> Vec<project_scanner::AgentSkillConfig> {
    let mut grouped: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for adapter in tool_adapters::all_tool_adapters(store) {
        let project_dir = adapter.project_relative_skills_dir().to_string();
        if project_dir.is_empty() {
            continue;
        }
        if let Some((_, agents)) = grouped.iter_mut().find(|(dir, _)| *dir == project_dir) {
            agents.push((adapter.key, adapter.display_name));
        } else {
            grouped.push((project_dir, vec![(adapter.key, adapter.display_name)]));
        }
    }

    grouped
        .into_iter()
        .filter_map(|(relative_skills_dir, agents)| {
            let (key, first_display_name) = agents.first()?.clone();
            let display_name = if agents.len() == 1 {
                first_display_name
            } else {
                agents
                    .into_iter()
                    .map(|(_, display_name)| display_name)
                    .collect::<Vec<_>>()
                    .join(" / ")
            };
            Some(project_scanner::AgentSkillConfig {
                key,
                display_name,
                relative_skills_dir,
            })
        })
        .collect()
}

fn linked_workspace_agent_key(rec: &ProjectRecord) -> String {
    rec.linked_agent_key
        .clone()
        .unwrap_or_else(|| slugify_skill_dir_name(&rec.name))
}

fn linked_workspace_agent_name(rec: &ProjectRecord) -> String {
    rec.linked_agent_name
        .clone()
        .unwrap_or_else(|| rec.name.clone())
}

fn read_workspace_skills(
    rec: &ProjectRecord,
    configs: &[project_scanner::AgentSkillConfig],
) -> Vec<project_scanner::ProjectSkillInfo> {
    if rec.workspace_type == "linked" {
        return project_scanner::read_linked_workspace_skills(
            Path::new(&rec.path),
            rec.disabled_path.as_deref().map(Path::new),
            &linked_workspace_agent_key(rec),
            &linked_workspace_agent_name(rec),
            true,
        );
    }
    project_scanner::read_project_skills(Path::new(&rec.path), configs)
}

/// Resolve the enabled and disabled skills root directories for a given agent in a workspace.
fn resolve_agent_skills_roots(
    store: &SkillStore,
    rec: &ProjectRecord,
    agent: &str,
) -> Option<(PathBuf, Option<PathBuf>)> {
    if rec.workspace_type == "linked" {
        if linked_workspace_agent_key(rec) != agent {
            return None;
        }
        return Some((
            PathBuf::from(&rec.path),
            rec.disabled_path.as_ref().map(PathBuf::from),
        ));
    }

    let adapter = tool_adapters::all_tool_adapters(store)
        .into_iter()
        .find(|adapter| adapter.key == agent)?;
    let project_dir = adapter.project_relative_skills_dir();
    let skills_root = Path::new(&rec.path).join(project_dir);
    let disabled_root = Path::new(&rec.path).join(format!("{}-disabled", project_dir));
    Some((skills_root, Some(disabled_root)))
}

fn project_agent_targets_for_record(
    store: &SkillStore,
    rec: &ProjectRecord,
) -> Vec<ProjectAgentTargetDto> {
    if rec.workspace_type == "linked" {
        return vec![ProjectAgentTargetDto {
            key: linked_workspace_agent_key(rec),
            display_name: linked_workspace_agent_name(rec),
            enabled: true,
            installed: true,
            is_custom: false,
        }];
    }

    let disabled_tools: std::collections::HashSet<String> = store
        .get_setting("disabled_tools")
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default()
        .into_iter()
        .collect();

    let adapters = tool_adapters::all_tool_adapters(store);
    agent_skill_configs(store)
        .into_iter()
        .map(|config| {
            // One row stands for every agent that reads this project directory
            // (Cline, Warp and GitLab Duo all read `.agents/skills`), under the
            // key of whichever comes first. It is usable while any of them is
            // installed and enabled, not only the one whose key it carries.
            let sharing: Vec<_> = adapters
                .iter()
                .filter(|adapter| {
                    adapter.project_relative_skills_dir() == config.relative_skills_dir
                })
                .collect();
            let installed: Vec<bool> = sharing
                .iter()
                .map(|adapter| adapter.is_installed())
                .collect();
            let any_installed = installed.contains(&true);
            // Count only the agents that are installed; with none installed the
            // row is unavailable anyway and reports that first.
            let enabled = sharing
                .iter()
                .zip(&installed)
                .filter(|(_, installed)| **installed || !any_installed)
                .any(|(adapter, _)| !disabled_tools.contains(&adapter.key));
            ProjectAgentTargetDto {
                enabled,
                installed: any_installed,
                is_custom: sharing.first().is_some_and(|adapter| adapter.is_custom),
                key: config.key,
                display_name: config.display_name,
            }
        })
        .collect()
}

/// Convert a project record into its DTO, folding the copies of one logical
/// skill across agents together by relative path.
fn project_to_dto(
    store: &SkillStore,
    rec: &ProjectRecord,
    all_managed: &[SkillRecord],
    configs: &[project_scanner::AgentSkillConfig],
) -> Result<ProjectDto, AppError> {
    let skills = read_workspace_skills(rec, configs);
    let mut grouped_statuses: HashMap<String, String> = HashMap::new();

    for skill in &skills {
        let matched = find_best_center_match(skill, all_managed);
        let status = project_skill_status(store, &rec.id, skill, matched)?;
        let key = skill.relative_path.to_lowercase();
        let existing = grouped_statuses
            .entry(key)
            .or_insert_with(|| status.clone());
        if sync_status_priority(&status) > sync_status_priority(existing) {
            *existing = status;
        }
    }

    let skill_count = grouped_statuses.len();
    let mut health = SyncHealthDto::default();
    for status in grouped_statuses.values() {
        match status.as_str() {
            "in_sync" => health.in_sync += 1,
            "project_newer" => health.project_newer += 1,
            "center_newer" => health.center_newer += 1,
            "diverged" => health.diverged += 1,
            _ => health.project_only += 1,
        }
    }

    Ok(ProjectDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        path: rec.path.clone(),
        workspace_type: rec.workspace_type.clone(),
        linked_agent_name: rec.linked_agent_name.clone(),
        supports_skill_toggle: rec.workspace_type != "linked" || rec.disabled_path.is_some(),
        sort_order: rec.sort_order,
        skill_count,
        sync_health: health,
        created_at: rec.created_at,
        updated_at: rec.updated_at,
    })
}

/// The same authoritative status selector for the project list/sidebar and
/// the project details. A managed Copy is measured against its pinned Git
/// subtree, never against mutable center contents or filesystem mtimes.
fn project_skill_status(
    store: &SkillStore,
    project_id: &str,
    skill: &project_scanner::ProjectSkillInfo,
    matched: Option<&SkillRecord>,
) -> Result<String, AppError> {
    match store
        .get_project_skill_deployment(project_id, &skill.path)
        .map_err(AppError::db)?
    {
        Some(row) if row.mode == "copy" && row.management_state == "managed" => {
            Ok(managed_copy_status(&row, Path::new(&skill.path)))
        }
        _ => Ok(classify_sync_status(skill, matched)),
    }
}

/// Severity of a sync status, used to reduce one logical skill's per-agent
/// copies to a single verdict: the worst one the group carries.
fn sync_status_priority(status: &str) -> u8 {
    match status {
        "diverged" => 5,
        "project_newer" => 4,
        "center_newer" => 3,
        "project_only" => 2,
        "in_sync" => 1,
        _ => 0,
    }
}

pub(crate) fn ensure_safe_skill_relative_path(skill_relative_path: &str) -> Result<(), AppError> {
    if skill_relative_path.trim().is_empty() {
        return Err(AppError::invalid_input("Invalid skill directory path"));
    }
    let mut saw_component = false;
    for component in Path::new(skill_relative_path).components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(AppError::invalid_input("Invalid skill directory path"));
        }
        saw_component = true;
    }
    if !saw_component {
        return Err(AppError::invalid_input("Invalid skill directory path"));
    }
    Ok(())
}

pub(crate) fn ensure_dir_within_root(path: &Path, root: &Path) -> Result<(), AppError> {
    // First check that the lexical path (before symlink resolution) is under root.
    // This ensures the link itself lives where expected.
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let abs_root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    if !abs_path.starts_with(&abs_root) {
        return Err(AppError::invalid_input("Invalid skill directory path"));
    }
    Ok(())
}

#[cfg(test)]
fn remove_workspace_skill_target(path: &Path) -> Result<(), AppError> {
    sync_engine::remove_target(path).map_err(AppError::io)
}

// Walks upward from `start`, removing each empty directory until reaching
// (and including) `root`. Stops at the first non-empty directory or any
// other error. `fs::remove_dir` only succeeds on empty directories, so this
// will never delete a directory that still holds skills.
#[cfg(test)]
fn cleanup_empty_dirs_up_to(start: &Path, root: &Path) {
    let Ok(root_canonical) = std::fs::canonicalize(root) else {
        return;
    };
    let mut current = start.to_path_buf();
    loop {
        let Ok(current_canonical) = std::fs::canonicalize(&current) else {
            return;
        };
        if !current_canonical.starts_with(&root_canonical) {
            return;
        }
        if std::fs::remove_dir(&current).is_err() {
            return;
        }
        if current_canonical == root_canonical {
            return;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return,
        }
    }
}

/// Only move exactly one entry. Never interpret a duplicate Symlink as
/// disposable: another user may own it, even if it points to central Store.
#[cfg(test)]
fn set_project_skill_enabled_state(
    skills_dir: &Path,
    disabled_dir: &Path,
    skill_relative_path: &str,
    enabled: bool,
) -> Result<(), AppError> {
    ensure_safe_skill_relative_path(skill_relative_path)?;
    let enabled_path = skills_dir.join(skill_relative_path);
    let disabled_path = disabled_dir.join(skill_relative_path);
    let (source, source_root, destination, dest_root) = if enabled {
        (&disabled_path, disabled_dir, &enabled_path, skills_dir)
    } else {
        (&enabled_path, skills_dir, &disabled_path, disabled_dir)
    };
    let source_exists = std::fs::symlink_metadata(source).is_ok();
    let dest_exists = std::fs::symlink_metadata(destination).is_ok();
    if source_exists && dest_exists {
        return Err(AppError::invalid_input(
            "Both enabled and disabled entries exist; resolve duplicate explicitly",
        ));
    }
    if !source_exists {
        if dest_exists {
            ensure_dir_within_root(destination, dest_root)?;
            return Ok(());
        }
        return Err(AppError::not_found("Skill directory not found"));
    }
    ensure_dir_within_root(source, source_root)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rename_directory_no_replace(source, destination)?;
    if enabled {
        if let Some(parent) = source.parent() {
            cleanup_empty_dirs_up_to(parent, source_root);
        }
    }
    Ok(())
}

fn ensure_distinct_linked_workspace_roots(
    skills_root: &Path,
    disabled_root: &Path,
) -> Result<(), AppError> {
    let skills_canonical = std::fs::canonicalize(skills_root)?;
    let disabled_canonical = std::fs::canonicalize(disabled_root)?;

    if skills_canonical == disabled_canonical
        || skills_canonical.starts_with(&disabled_canonical)
        || disabled_canonical.starts_with(&skills_canonical)
    {
        return Err(AppError::invalid_input(
            "Skills directory and disabled skills directory must not overlap",
        ));
    }

    Ok(())
}

pub(crate) fn slugify_skill_dir_name(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in name.chars().flat_map(|c| c.to_lowercase()) {
        let valid = ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.';
        if valid {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '_' || c == '.');
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn source_ref_matches_skill_path(
    skill_path: &str,
    skill_canonical: Option<&PathBuf>,
    managed: &SkillRecord,
) -> bool {
    let Some(source_ref) = managed.source_ref.as_deref() else {
        return false;
    };
    if source_ref == skill_path {
        return true;
    }
    let Some(skill_canonical) = skill_canonical else {
        return false;
    };
    let Ok(source_canonical) = std::fs::canonicalize(source_ref) else {
        return false;
    };
    source_canonical == *skill_canonical
}

pub(crate) fn find_best_center_match<'a>(
    skill: &project_scanner::ProjectSkillInfo,
    all_managed: &'a [SkillRecord],
) -> Option<&'a SkillRecord> {
    let skill_hash = skill.content_hash.as_deref();
    let canonical_skill_path = std::fs::canonicalize(&skill.path).ok();

    // source_ref is the strongest direct link there is.
    if let Some(managed) = all_managed.iter().find(|managed| {
        source_ref_matches_skill_path(&skill.path, canonical_skill_path.as_ref(), managed)
    }) {
        return Some(managed);
    }

    // A content hash that names exactly one library skill outranks any
    // directory or name evidence: it says the two directories hold the same
    // bytes, while a directory name only says they were once called the same
    // thing. An export written under a slugified name can land on a directory
    // that now reads as a *different* skill (a library holding both
    // "Code Review" and "code-review" exports the first as `code-review`),
    // and this match also decides where an import writes back — binding the
    // wrong row there overwrites the other skill.
    //
    // Only a unique hash qualifies. Several skills sharing one hash is the
    // arbitrary-pick this fix exists to remove, and those fall through to the
    // directory and name layers below.
    if let Some(hash) = skill_hash {
        let mut by_hash = all_managed
            .iter()
            .filter(|managed| managed.content_hash.as_deref() == Some(hash));
        if let Some(first) = by_hash.next() {
            if by_hash.next().is_none() {
                return Some(first);
            }
        }
    }

    // The central directory name is steadier than the frontmatter name:
    // several skills shipped from one repo can share the latter.
    let by_central_dir: Vec<&SkillRecord> = all_managed
        .iter()
        .filter(|managed| {
            Path::new(&managed.central_path)
                .file_name()
                .map(|name| name.to_string_lossy().eq_ignore_ascii_case(&skill.dir_name))
                .unwrap_or(false)
        })
        .collect();
    if let Some(managed) = unique_center_match(&by_central_dir, skill_hash) {
        return Some(managed);
    }

    // Covers the ordinary skill whose name and directory agree.
    let by_name: Vec<&SkillRecord> = all_managed
        .iter()
        .filter(|managed| {
            slugify_skill_dir_name(&managed.name).eq_ignore_ascii_case(&skill.dir_name)
        })
        .collect();
    if let Some(managed) = unique_center_match(&by_name, skill_hash) {
        return Some(managed);
    }

    None
}

/// Pick the one skill among candidates sharing an identity signal,
/// disambiguating by content hash when the signal alone leaves several.
fn unique_center_match<'a>(
    candidates: &[&'a SkillRecord],
    skill_hash: Option<&str>,
) -> Option<&'a SkillRecord> {
    match candidates.len() {
        0 => None,
        1 => Some(candidates[0]),
        _ => {
            let hash = skill_hash?;
            let mut filtered = candidates
                .iter()
                .copied()
                .filter(|managed| managed.content_hash.as_deref() == Some(hash));
            let first = filtered.next()?;
            filtered.next().is_none().then_some(first)
        }
    }
}

/// Publish a staged directory only if the destination is still unoccupied.
/// On macOS/Linux this is an OS-level no-replace rename, not a vulnerable
/// stat-then-rename pair. Other platforms retain a conservative preflight.
fn rename_directory_no_replace(source: &Path, target: &Path) -> Result<(), AppError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            target,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|err| AppError::io(std::io::Error::from_raw_os_error(err.raw_os_error())))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        if std::fs::symlink_metadata(target).is_ok() {
            return Err(AppError::invalid_input("Destination already occupied"));
        }
        std::fs::rename(source, target).map_err(AppError::io)
    }
}

/// Best-effort startup repair for a crash after the original Copy was moved
/// but before the SQLite outcome was committed. Never adopt, delete or replace
/// an occupied project target. Ambiguous states are reported for inspection.
pub fn recover_incomplete_project_skill_operations(
    store: &SkillStore,
) -> Result<Vec<String>, AppError> {
    let _lock = match RepoLock::acquire_foreground("project Skill operation recovery") {
        Ok(lock) => lock,
        Err(err) => {
            return Ok(vec![format!(
                "Recovery deferred while repository is busy: {err}"
            )]);
        }
    };
    let operations = store
        .pending_project_skill_operations()
        .map_err(AppError::db)?;
    let mut notices = Vec::new();
    for op in operations {
        let target = PathBuf::from(&op.target_path);
        let recovery_root = PathBuf::from(&op.recovery_root);
        // Only recover paths that still belong to the registered project.
        // A stale/malicious SQLite path must not move arbitrary directories.
        let project_root = store
            .get_project_by_id(&op.project_id)
            .map_err(AppError::db)?
            .and_then(|project| std::fs::canonicalize(project.path).ok());
        let parent_root = target
            .parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok());
        if !matches!((&project_root, &parent_root),
            (Some(project), Some(parent)) if parent.starts_with(project)
        ) {
            notices.push(format!(
                "Recovery refused for operation {}: destination is outside project",
                op.id,
            ));
            continue;
        }
        let expected = target
            .parent()
            .map(|p| p.join(".atm-recovery").join(&op.id));
        // Do not trust database-stored paths alone to authorize a write.
        if expected.as_deref() != Some(recovery_root.as_path()) {
            notices.push(format!("Unsafe recovery path for operation {}", op.id));
            continue;
        }
        if op.kind == "toggle" && matches!(op.phase.as_str(), "preparing" | "publishing") {
            let Some(ref dest) = op.destination_path else {
                notices.push(format!("Toggle {} missing destination", op.id));
                continue;
            };
            let destination = Path::new(dest);
            let destination_parent = destination
                .parent()
                .and_then(|parent| std::fs::canonicalize(parent).ok());
            if !matches!((&project_root, &destination_parent),
                (Some(project), Some(parent)) if parent.starts_with(project))
                || op.observed_identity.is_none()
            {
                notices.push(format!(
                    "Toggle {} recovery destination or identity is invalid",
                    op.id,
                ));
                continue;
            }
            let expected_identity = op.observed_identity.as_deref();
            let original = project_target_identity_string(&target);
            let moved = project_target_identity_string(destination);
            if original.as_deref() == expected_identity && moved.is_none() {
                let _ = store.transition_project_skill_operation(
                    &op.id,
                    &op.phase,
                    "recovery_available",
                );
                notices.push(format!(
                    "Interrupted toggle {} left original entry at {}",
                    op.id,
                    target.display(),
                ));
                continue;
            }
            if op.phase == "publishing"
                && original.is_none()
                && moved.as_deref() == expected_identity
            {
                if rename_directory_no_replace(destination, &target).is_ok() {
                    let _ = store.transition_project_skill_operation(
                        &op.id,
                        "publishing",
                        "recovery_available",
                    );
                    notices.push(format!(
                        "Restored interrupted toggle {} to {}",
                        op.id,
                        target.display(),
                    ));
                    continue;
                }
            }
            notices.push(format!(
                "Toggle {} requires manual recovery: original {}, destination {}",
                op.id,
                target.display(),
                destination.display(),
            ));
            continue;
        }
        if !matches!(op.phase.as_str(), "preparing" | "publishing") {
            notices.push(format!(
                "Project Skill operation {} ({}) requires follow-up at {} (phase {})",
                op.id, op.kind, op.recovery_root, op.phase,
            ));
            continue;
        }
        if op.phase == "preparing" {
            // No destructive filesystem action is permitted during preparing.
            // End the active reservation only when the original still exists
            // for update/delete or the deploy destination is still absent.
            let unchanged_position = if op.kind == "deploy" {
                std::fs::symlink_metadata(&target).is_err()
            } else {
                std::fs::symlink_metadata(&target).is_ok()
            };
            if unchanged_position {
                let _ = store.transition_project_skill_operation(
                    &op.id,
                    "preparing",
                    "recovery_available",
                );
                notices.push(format!(
                    "Interrupted {} preparation {} retained at {}",
                    op.kind,
                    op.id,
                    recovery_root.display(),
                ));
                continue;
            }
        }
        if op.phase == "publishing"
            && op.kind == "deploy"
            && std::fs::symlink_metadata(&target).is_err()
        {
            let _ = store.transition_project_skill_operation(
                &op.id,
                "publishing",
                "recovery_available",
            );
            notices.push(format!(
                "Interrupted deploy {} staged content retained at {}",
                op.id,
                recovery_root.display(),
            ));
            continue;
        }
        if op.phase == "publishing" && matches!(op.kind.as_str(), "update" | "delete") {
            let previous = recovery_root.join("previous");
            if std::fs::symlink_metadata(&target).is_err()
                && std::fs::symlink_metadata(&previous)
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
                && std::fs::symlink_metadata(&recovery_root)
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
                && std::fs::symlink_metadata(recovery_root.parent().unwrap())
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            {
                if rename_directory_no_replace(&previous, &target).is_ok() {
                    let _ = store.transition_project_skill_operation(
                        &op.id,
                        "publishing",
                        "recovery_available",
                    );
                    notices.push(format!(
                        "Restored interrupted {} operation {} to {}",
                        op.kind,
                        op.id,
                        target.display(),
                    ));
                    continue;
                }
            }
        }
        // Deploy operations are especially conservative: a matching Tree
        // might have been placed by an outsider during the interrupted run.
        // Keep their journal and filesystem bytes; never auto-grant ownership.
        notices.push(format!(
            "Incomplete project Skill {} operation {}: target {}, recovery {}",
            op.kind,
            op.id,
            target.display(),
            recovery_root.display(),
        ));
    }
    Ok(notices)
}

/// The only authoritative Copy classification uses its immutable deployment
/// baseline; mtimes and the mutable central working tree play no role.
fn managed_copy_status(deployment: &ProjectSkillDeploymentRecord, path: &Path) -> String {
    let Some(base) = deployment.base_tree_oid.as_deref() else {
        return "diverged".to_string();
    };
    let root = central_repo::skills_dir();
    let local_clean = git_skill_versions::matches_skill_tree(&root, base, path).unwrap_or(false);
    let central_clean =
        git_skill_versions::resolve_skill_version(&root, &deployment.skill_id, None)
            .is_ok_and(|v| v.tree_oid == base);
    match (local_clean, central_clean) {
        (true, true) => "in_sync",
        (true, false) => "center_newer",
        (false, true) => "project_newer",
        (false, false) => "diverged",
    }
    .to_string()
}

pub(crate) fn classify_sync_status(
    skill: &project_scanner::ProjectSkillInfo,
    managed: Option<&SkillRecord>,
) -> String {
    let Some(managed) = managed else {
        return "project_only".to_string();
    };

    // Fast path: compare project hash against DB-stored center hash
    if skill.content_hash.is_some()
        && managed.content_hash.as_deref() == skill.content_hash.as_deref()
    {
        return "in_sync".to_string();
    }

    // The DB hash may be stale, and `updated_at` is the wrong clock for the
    // comparison further down, so read the center from disk once and answer
    // both questions from the same walk.
    let center_entries =
        crate::core::content_hash::list_content_files(Path::new(&managed.central_path));

    if let Some(project_hash) = skill.content_hash.as_deref() {
        if project_hash == crate::core::content_hash::hash_entries(&center_entries) {
            return "in_sync".to_string();
        }
    }

    let Some(project_modified_at) = skill.last_modified_at else {
        return "diverged".to_string();
    };

    // The project side is a filesystem mtime, so the center has to be one too.
    // `updated_at` is a database column stamped when the row was written:
    // editing files in the library does not move it, and a metadata-only write
    // moves it while no content changed. Comparing the two rulers reported
    // "center is newer" for a project copy the user had just edited, and that
    // status invites a pull, which overwrites the edit — the diagnosis behind
    // #328.
    let Some(center_modified_at) = crate::core::content_hash::latest_modified_ms(&center_entries)
    else {
        return "diverged".to_string();
    };
    let threshold_ms = 1_000;
    if project_modified_at > center_modified_at + threshold_ms {
        "project_newer".to_string()
    } else if center_modified_at > project_modified_at + threshold_ms {
        "center_newer".to_string()
    } else {
        "diverged".to_string()
    }
}

static GET_PROJECTS_FIRST_CALL: AtomicBool = AtomicBool::new(true);

/// Read-only view of persistent deployment/update/delete/toggle journal.
/// This deliberately offers no cleanup/restore action: a retained backup may
/// still be written by an uncooperative process holding an old file handle.
#[derive(Debug, Serialize)]
pub struct ProjectSkillRecoveryDto {
    pub operation_id: String,
    pub project_id: String,
    pub skill_id: String,
    pub deployment_id: Option<String>,
    pub kind: String,
    pub phase: String,
    pub target_path: String,
    pub recovery_root: String,
    pub destination_path: Option<String>,
    pub target_exists: bool,
    pub recovery_root_exists: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn get_project_skill_recoveries_internal(
    store: &SkillStore,
    project_id: &str,
) -> Result<Vec<ProjectSkillRecoveryDto>, AppError> {
    if store
        .get_project_by_id(project_id)
        .map_err(AppError::db)?
        .is_none()
    {
        return Err(AppError::not_found("Workspace not found"));
    }
    let mut rows = store
        .pending_project_skill_operations()
        .map_err(AppError::db)?
        .into_iter()
        .filter(|op| op.project_id == project_id)
        .map(|op| ProjectSkillRecoveryDto {
            operation_id: op.id,
            project_id: op.project_id,
            skill_id: op.skill_id,
            deployment_id: op.deployment_id,
            kind: op.kind,
            phase: op.phase,
            target_exists: std::fs::symlink_metadata(&op.target_path).is_ok(),
            recovery_root_exists: std::fs::symlink_metadata(&op.recovery_root)
                .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()),
            target_path: op.target_path,
            recovery_root: op.recovery_root,
            destination_path: op.destination_path,
            created_at: op.created_at,
            updated_at: op.updated_at,
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then(a.operation_id.cmp(&b.operation_id))
    });
    Ok(rows)
}

#[tauri::command]
pub async fn get_project_skill_recoveries(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
) -> Result<Vec<ProjectSkillRecoveryDto>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        get_project_skill_recoveries_internal(&store, &project_id)
    })
    .await?
}

#[tauri::command]
pub async fn get_projects(store: State<'_, Arc<SkillStore>>) -> Result<Vec<ProjectDto>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || get_projects_internal(&store)).await?
}

pub fn get_projects_internal(store: &SkillStore) -> Result<Vec<ProjectDto>, AppError> {
    let start = Instant::now();
    let records = store.get_all_projects().map_err(AppError::db)?;
    let all_managed = store.get_all_skills().map_err(AppError::db)?;
    let configs = agent_skill_configs(&store);
    let count = records.len();
    let dtos: Vec<ProjectDto> = records
        .iter()
        .map(|r| project_to_dto(store, r, &all_managed, &configs))
        .collect::<Result<Vec<_>, _>>()?;
    let elapsed_ms = start.elapsed().as_millis();
    if should_log_first_or_slow(&GET_PROJECTS_FIRST_CALL, elapsed_ms, 100) {
        log::info!("get_projects: {count} projects in {elapsed_ms} ms");
    }
    Ok(dtos)
}

#[tauri::command]
pub async fn add_project(
    store: State<'_, Arc<SkillStore>>,
    path: String,
) -> Result<ProjectDto, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || add_project_internal(&store, path)).await?
}

/// Validate a directory for registration and build its record without
/// touching the disk or the database, so a dry run sees the same refusals as
/// the real add.
///
/// The path is stored canonicalized and compared canonicalized: the `path`
/// column is UNIQUE on the raw string, so `/a/b`, `/a/b/` and a symlink to it
/// used to register as three projects, and a byte-identical repeat surfaced
/// only as a database error.
pub fn prepare_project_record(store: &SkillStore, path: &Path) -> Result<ProjectRecord, AppError> {
    if !path.is_dir() {
        return Err(AppError::invalid_input("Directory does not exist"));
    }
    let canonical = dunce::canonicalize(path)?;
    let stored = stored_project_path(&canonical, &std::path::absolute(path)?);
    let canonical_string = stored.to_string_lossy().to_string();
    let existing = store.get_all_projects().map_err(AppError::db)?;
    if let Some(project) = existing.iter().find(|project| {
        project.path == canonical_string
            || dunce::canonicalize(&project.path).is_ok_and(|p| p == canonical)
    }) {
        return Err(AppError::invalid_input(format!(
            "Project '{}' is already registered at {} (id: {})",
            project.name, project.path, project.id
        )));
    }

    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let now = chrono::Utc::now().timestamp_millis();
    Ok(ProjectRecord {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        path: canonical_string,
        workspace_type: "project".to_string(),
        linked_agent_key: None,
        linked_agent_name: None,
        disabled_path: None,
        sort_order: 0,
        created_at: now,
        updated_at: now,
    })
}

/// The path to store for a project. On Windows `canonicalize` returns a
/// verbatim path (`\\?\C:\repo`), in which `/` is not a separator, so joining
/// a project-relative directory such as `.agents/skills` onto it names
/// nothing. `dunce::canonicalize` drops the prefix only where that keeps the
/// same meaning (plain drive paths); it keeps `\\?\UNC\…` and names Windows
/// would reinterpret (`repo.`, `CON`). For those, store the absolute form of
/// what the caller gave, which is what was stored before canonicalization.
fn stored_project_path(canonical: &Path, absolute_input: &Path) -> PathBuf {
    if canonical.to_string_lossy().starts_with(r"\\?\") {
        absolute_input.to_path_buf()
    } else {
        canonical.to_path_buf()
    }
}

pub fn add_project_internal(store: &SkillStore, path: String) -> Result<ProjectDto, AppError> {
    let record = prepare_project_record(store, Path::new(&path))?;
    // Registration only records the project. Agent-specific directories are
    // created on first deployment, not merely because a project is tracked.
    store.insert_project(&record).map_err(AppError::db)?;
    let all_managed = store.get_all_skills().map_err(AppError::db)?;
    let configs = agent_skill_configs(&store);
    project_to_dto(store, &record, &all_managed, &configs)
}

#[tauri::command]
pub async fn add_linked_workspace(
    store: State<'_, Arc<SkillStore>>,
    name: String,
    path: String,
    disabled_path: Option<String>,
) -> Result<ProjectDto, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::invalid_input("Workspace name is required"));
        }

        let skills_root = PathBuf::from(path.trim());
        if !skills_root.is_dir() {
            return Err(AppError::invalid_input("Skills directory does not exist"));
        }

        let disabled_path = disabled_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let disabled_path = if let Some(disabled) = disabled_path {
            let disabled_root = PathBuf::from(&disabled);
            if !disabled_root.is_dir() {
                return Err(AppError::invalid_input(
                    "Disabled skills directory does not exist",
                ));
            }
            ensure_distinct_linked_workspace_roots(&skills_root, &disabled_root)?;
            Some(disabled)
        } else {
            let mut disabled_root = skills_root.clone();
            let derived = disabled_root
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| format!("{}-disabled", name));
            match derived {
                Some(name) => {
                    disabled_root.set_file_name(name);
                    match std::fs::create_dir_all(&disabled_root) {
                        Ok(()) => {
                            ensure_distinct_linked_workspace_roots(&skills_root, &disabled_root)?;
                            Some(disabled_root.to_string_lossy().to_string())
                        }
                        Err(_) => None,
                    }
                }
                None => None,
            }
        };

        let now = chrono::Utc::now().timestamp_millis();
        let record = ProjectRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.clone(),
            path: skills_root.to_string_lossy().to_string(),
            workspace_type: "linked".to_string(),
            linked_agent_key: Some(slugify_skill_dir_name(&name)),
            linked_agent_name: Some(name),
            disabled_path,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        };

        store.insert_project(&record).map_err(AppError::db)?;
        let all_managed = store.get_all_skills().map_err(AppError::db)?;
        let configs = agent_skill_configs(&store);
        project_to_dto(&store, &record, &all_managed, &configs)
    })
    .await?
}

#[tauri::command]
pub async fn remove_project(store: State<'_, Arc<SkillStore>>, id: String) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || remove_project_internal(&store, id)).await?
}

pub fn remove_project_internal(store: &SkillStore, id: String) -> Result<(), AppError> {
    store.delete_project(&id).map_err(AppError::db)
}

/// Show a workspace's folder in the OS file manager, selected where the
/// platform can (Linux `xdg-open` opens the containing directory).
///
/// The path is resolved from the id here instead of being accepted from the
/// frontend, so a caller cannot ask the OS to open an arbitrary path. The
/// folder is checked first because a file manager given a missing path either
/// fails silently or opens somewhere the user did not ask for.
#[tauri::command]
pub async fn reveal_project_folder(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let record = store
            .get_project_by_id(&project_id)
            .map_err(AppError::db)?
            .ok_or_else(|| AppError::not_found("Workspace not found"))?;

        let path = PathBuf::from(&record.path);
        if !path.is_dir() {
            return Err(AppError::invalid_input(format!(
                "Folder does not exist: {}",
                record.path
            )));
        }

        // explorer.exe exits non-zero even when it succeeds, so the platform
        // branch is judged by `exit_is_failure` rather than checked here.
        let status = file_manager::reveal_item(&path).map_err(AppError::io)?;
        if file_manager::exit_is_failure(status.success()) {
            return Err(AppError::io(format!(
                "File manager exited with status: {status}"
            )));
        }
        Ok(())
    })
    .await?
}

#[tauri::command]
pub async fn reorder_projects(
    ids: Vec<String>,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.reorder_projects(&ids).map_err(AppError::db))
        .await?
}

#[tauri::command]
pub async fn scan_projects(
    root: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<String>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root_path = Path::new(&root);
        if !root_path.is_dir() {
            return Err(AppError::invalid_input("Directory does not exist"));
        }
        let configs = agent_skill_configs(&store);
        Ok(project_scanner::scan_projects_in_dir(
            root_path, 4, &configs,
        ))
    })
    .await?
}

#[tauri::command]
pub async fn get_project_agent_targets(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
) -> Result<Vec<ProjectAgentTargetDto>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        get_project_agent_targets_internal(&store, project_id)
    })
    .await?
}

pub fn get_project_agent_targets_internal(
    store: &SkillStore,
    project_id: String,
) -> Result<Vec<ProjectAgentTargetDto>, AppError> {
    let record = store
        .get_project_by_id(&project_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Workspace not found"))?;
    Ok(project_agent_targets_for_record(&store, &record))
}

#[tauri::command]
pub async fn get_project_skills(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
) -> Result<Vec<project_scanner::ProjectSkillInfo>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || get_project_skills_internal(&store, project_id))
        .await?
}

pub fn get_project_skills_internal(
    store: &SkillStore,
    project_id: String,
) -> Result<Vec<project_scanner::ProjectSkillInfo>, AppError> {
    let record = store
        .get_project_by_id(&project_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Workspace not found"))?;

    let configs = agent_skill_configs(&store);
    let mut skills = read_workspace_skills(&record, &configs);

    let all_managed = store.get_all_skills().unwrap_or_default();
    let tags_map = store.get_tags_map().unwrap_or_default();
    for skill in &mut skills {
        let matched = find_best_center_match(skill, &all_managed);
        skill.in_center = matched.is_some();
        skill.center_skill_id = matched.map(|m| m.id.clone());
        skill.tags = skill
            .center_skill_id
            .as_ref()
            .and_then(|skill_id| tags_map.get(skill_id).cloned())
            .unwrap_or_default();
        skill.sync_status = project_skill_status(store, &project_id, skill, matched)?;
        if let Some(deployment) = store
            .get_project_skill_deployment(&project_id, &skill.path)
            .map_err(AppError::db)?
        {
            if deployment.mode == "copy" && deployment.management_state == "managed" {
                skill.in_center = true;
                skill.center_skill_id = Some(deployment.skill_id.clone());
                skill.tags = tags_map
                    .get(&deployment.skill_id)
                    .cloned()
                    .unwrap_or_default();
            }
        }
    }

    Ok(skills)
}

#[tauri::command]
pub async fn get_project_skill_document(
    project_id: String,
    skill_relative_path: String,
    agent: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<ProjectSkillDocumentDto, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ensure_safe_skill_relative_path(&skill_relative_path)?;

        let record = store
            .get_project_by_id(&project_id)
            .map_err(AppError::db)?
            .ok_or_else(|| AppError::not_found("Workspace not found"))?;

        let (skills_root, disabled_root) = resolve_agent_skills_roots(&store, &record, &agent)
            .ok_or_else(|| AppError::not_found(format!("Unknown workspace agent: {}", agent)))?;
        let disabled_root_copy = disabled_root.clone();
        let skill_dir = skills_root.join(&skill_relative_path);
        let skill_dir = if skill_dir.is_dir() {
            ensure_dir_within_root(&skill_dir, &skills_root)?;
            skill_dir
        } else if let Some(disabled_root) = disabled_root {
            let disabled = disabled_root.join(&skill_relative_path);
            if disabled.is_dir() {
                ensure_dir_within_root(&disabled, &disabled_root)?;
                disabled
            } else {
                return Err(AppError::not_found("Skill directory not found"));
            }
        } else {
            return Err(AppError::not_found("Skill directory not found"));
        };

        // Collect all allowed roots for symlink target validation
        let mut allowed_roots: Vec<PathBuf> = vec![skills_root.clone()];
        if let Some(dr) = disabled_root_copy {
            allowed_roots.push(dr);
        }
        // For project workspaces, also allow the project root itself
        if record.workspace_type != "linked" {
            allowed_roots.push(PathBuf::from(&record.path));
        }

        let candidates = ["SKILL.md", "skill.md", "CLAUDE.md", "README.md"];
        for candidate in &candidates {
            let file_path = skill_dir.join(candidate);
            if !file_path.exists() {
                continue;
            }
            // For symlinks, verify the resolved target stays within an allowed root
            if let Ok(meta) = std::fs::symlink_metadata(&file_path) {
                if meta.file_type().is_symlink() {
                    let resolved = match std::fs::canonicalize(&file_path) {
                        Ok(r) => r,
                        Err(_) => continue, // broken symlink
                    };
                    let in_allowed_root = allowed_roots.iter().any(|root| {
                        std::fs::canonicalize(root)
                            .map(|canon| resolved.starts_with(&canon))
                            .unwrap_or(false)
                    });
                    if !in_allowed_root {
                        continue;
                    }
                }
            }
            if file_path.is_file() {
                let content = std::fs::read_to_string(&file_path)?;
                return Ok(ProjectSkillDocumentDto {
                    skill_name: skill_relative_path,
                    filename: candidate.to_string(),
                    content,
                });
            }
        }

        Err(AppError::not_found(
            "No document file found in skill directory",
        ))
    })
    .await?
}

#[tauri::command]
pub async fn import_project_skill_to_center(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
    skill_relative_path: String,
    agent: String,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // "Update to center" is the user's explicit overwrite; the frontend
        // already refuses when several copies differ (pushSkillToCenterAndAlign).
        import_project_skill_to_center_internal(
            &store,
            project_id,
            skill_relative_path,
            agent,
            true,
        )
        .map(|_| ())
    })
    .await?
}

/// Push a project skill into the library: overwrite the library skill it
/// matches, or create a new local skill. Without `force`, refuses to overwrite
/// a library copy whose content differs. A copy already in sync is left alone.
pub fn import_project_skill_to_center_internal(
    store: &SkillStore,
    project_id: String,
    skill_relative_path: String,
    agent: String,
    force: bool,
) -> Result<ProjectImportResult, AppError> {
    // The CLI and the app can write the library at the same time, so the
    // destination is resolved and authorized under the lock: a plan made
    // before it could miss a same-named skill another writer just installed
    // and overwrite it without --force.
    let _lock = RepoLock::acquire_foreground("import project skill").map_err(AppError::io)?;
    let (mut outcome, skill) =
        resolve_project_import(store, &project_id, &skill_relative_path, &agent)?;
    if let Some(refusal) = project_import_refusal(&outcome, force) {
        return Err(refusal);
    }
    if outcome.action == ProjectImportAction::Unchanged {
        return Ok(outcome);
    }

    let source_path = PathBuf::from(&skill.path);
    let all_managed = store.get_all_skills().map_err(AppError::db)?;
    // Write to the library skill the authorized plan matched (by the same
    // find_best_center_match the UI uses), nothing re-derived after it.
    // After updating, bind source_ref so future imports match by exact path.
    let matched = outcome
        .skill_id
        .as_ref()
        .and_then(|id| all_managed.iter().find(|managed| &managed.id == id));
    if let Some(existing) = matched {
        let result = installer::install_from_local_to_destination(
            &source_path,
            Some(&existing.name),
            Path::new(&existing.central_path),
        )
        .map_err(AppError::io)?;
        store
            .update_skill_after_install(
                &existing.id,
                &existing.name,
                result.description.as_deref(),
                existing.source_revision.as_deref(),
                existing.remote_revision.as_deref(),
                Some(&result.content_hash),
                "local_only",
            )
            .map_err(AppError::db)?;
        // Only update source_ref when the match was already by source_ref
        // path (not by hash or name). This avoids permanently rebinding
        // unrelated center skills that merely share a name or content.
        let already_matched_by_ref = source_ref_matches_skill_path(
            &skill.path,
            std::fs::canonicalize(&skill.path).ok().as_ref(),
            existing,
        );
        if existing.source_type == "local" && already_matched_by_ref {
            store
                .update_skill_source_ref(&existing.id, &skill.path)
                .map_err(AppError::db)?;
        }
        sync_metadata::write_all_from_db_unlocked(store).map_err(AppError::io)?;
        store.log_audit(
            AuditDraft::new("import")
                .skill(existing.id.clone(), existing.name.clone())
                .detail(format!("from project copy {}", skill.path))
                .ok(),
        );
        outcome.action = ProjectImportAction::Overwrite;
        outcome.skill_id = Some(existing.id.clone());
        outcome.name = existing.name.clone();
        outcome.central_path = Some(existing.central_path.clone());
        return Ok(outcome);
    }

    let result =
        installer::install_from_local(&source_path, Some(&skill.name)).map_err(AppError::io)?;

    let now = chrono::Utc::now().timestamp_millis();
    let id = uuid::Uuid::new_v4().to_string();

    let skill_record = SkillRecord {
        id: id.clone(),
        name: result.name.clone(),
        description: result.description.clone(),
        source_type: "local".to_string(),
        source_ref: Some(skill.path.clone()),
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        central_path: result.central_path.to_string_lossy().to_string(),
        content_hash: Some(result.content_hash.clone()),
        enabled: true,
        created_at: now,
        updated_at: now,
        status: "ok".to_string(),
        update_status: "local_only".to_string(),
        last_checked_at: Some(now),
        last_check_error: None,
    };

    store.insert_skill(&skill_record).map_err(AppError::db)?;
    sync_metadata::write_all_from_db_unlocked(store).map_err(AppError::io)?;
    store.log_audit(
        AuditDraft::new("import")
            .skill(id.clone(), result.name.clone())
            .detail(format!("from project copy {}", skill.path))
            .ok(),
    );

    outcome.action = ProjectImportAction::Create;
    outcome.skill_id = Some(id);
    outcome.name = result.name;
    outcome.central_path = Some(skill_record.central_path);
    Ok(outcome)
}

#[tauri::command]
pub async fn update_project_skill_to_center(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
    skill_relative_path: String,
    agent: String,
) -> Result<(), AppError> {
    import_project_skill_to_center(store, project_id, skill_relative_path, agent).await
}

#[tauri::command]
pub fn slugify_skill_names(names: Vec<String>) -> Vec<String> {
    names.iter().map(|n| slugify_skill_dir_name(n)).collect()
}

#[tauri::command]
pub async fn get_project_skill_git_history(
    store: State<'_, Arc<SkillStore>>,
    skill_id: String,
    limit: Option<u32>,
) -> Result<git_skill_versions::GitSkillHistory, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Do not make arbitrary filesystem history query a path read primitive:
        // require a SkillId registered in this central Store.
        if store
            .get_skill_by_id(&skill_id)
            .map_err(AppError::db)?
            .is_none()
        {
            return Err(AppError::not_found("Skill not found"));
        }
        git_skill_versions::list_skill_history(
            &central_repo::skills_dir(),
            &skill_id,
            limit.unwrap_or(25) as usize,
        )
        .map_err(AppError::io)
    })
    .await?
}

#[derive(Debug, Serialize)]
pub struct ProjectSkillExportRecovery {
    pub operation_id: String,
    pub phase: String,
    pub recovery_root: String,
}

#[derive(Debug, Serialize)]
pub struct ProjectSkillExportTargetResult {
    pub skill_id: String,
    pub agent: String,
    pub path: String,
    pub result: String,
    pub error: Option<String>,
    pub recovery: Vec<ProjectSkillExportRecovery>,
}

#[derive(Debug, Serialize)]
pub struct ProjectSkillExportReport {
    pub ok: bool,
    pub targets: Vec<ProjectSkillExportTargetResult>,
}

/// Same authority and no-clobber publisher as ordinary GUI deployment.
/// A partially succeeded per-agent request is a report, not a thrown error
/// whose caller could incorrectly interpret as a full rollback.
pub fn export_skill_to_project_with_report_internal(
    store: &SkillStore,
    skill_id: String,
    project_id: String,
    agents: Option<Vec<String>>,
    commit_oid: Option<&str>,
) -> Result<ProjectSkillExportReport, AppError> {
    let plan = plan_gui_project_skill_deploy(store, &skill_id, &project_id, agents, commit_oid)?;
    let mut failed = false;
    let mut targets = Vec::with_capacity(plan.len());
    for target in &plan {
        if failed {
            targets.push(ProjectSkillExportTargetResult {
                skill_id: target.skill_id.clone(),
                agent: target.agent.clone(),
                path: target.path.clone(),
                result: "not_attempted".to_string(),
                error: None,
                recovery: Vec::new(),
            });
            continue;
        }
        let previous = store
            .pending_project_skill_operations()
            .map_err(AppError::db)?
            .into_iter()
            .map(|op| op.id)
            .collect::<std::collections::HashSet<_>>();
        let applied = apply_project_deploy(store, &[target.clone()]).and_then(|_| {
            if std::fs::symlink_metadata(&target.path).is_err() {
                return Err(AppError::invalid_input(
                    "Deployment reported success but target no longer exists",
                ));
            }
            Ok(())
        });
        let (result, error, recovery) = match applied {
            Ok(()) => ("created".to_string(), None, Vec::new()),
            Err(err) => {
                failed = true;
                let recoveries = store
                    .pending_project_skill_operations()
                    .map_err(AppError::db)?
                    .into_iter()
                    .filter(|op| {
                        !previous.contains(&op.id)
                            && op.project_id == project_id
                            && op.target_path == target.path
                    })
                    .map(|op| ProjectSkillExportRecovery {
                        operation_id: op.id,
                        phase: op.phase,
                        recovery_root: op.recovery_root,
                    })
                    .collect();
                ("failed".to_string(), Some(err.to_string()), recoveries)
            }
        };
        targets.push(ProjectSkillExportTargetResult {
            skill_id: target.skill_id.clone(),
            agent: target.agent.clone(),
            path: target.path.clone(),
            result,
            error,
            recovery,
        });
    }
    Ok(ProjectSkillExportReport {
        ok: !failed,
        targets,
    })
}

#[tauri::command]
pub async fn export_skill_to_project_with_report(
    store: State<'_, Arc<SkillStore>>,
    skill_id: String,
    project_id: String,
    agents: Option<Vec<String>>,
    commit_oid: Option<String>,
) -> Result<ProjectSkillExportReport, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        export_skill_to_project_with_report_internal(
            &store,
            skill_id,
            project_id,
            agents,
            commit_oid.as_deref(),
        )
    })
    .await?
}

#[tauri::command]
pub async fn export_skill_to_project(
    store: State<'_, Arc<SkillStore>>,
    skill_id: String,
    project_id: String,
    agents: Option<Vec<String>>,
    commit_oid: Option<String>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        export_skill_to_project_internal_with_commit(
            &store,
            skill_id,
            project_id,
            agents,
            commit_oid.as_deref(),
        )
    })
    .await?
}

pub fn export_skill_to_project_internal(
    store: &SkillStore,
    skill_id: String,
    project_id: String,
    agents: Option<Vec<String>>,
) -> Result<(), AppError> {
    export_skill_to_project_internal_with_commit(store, skill_id, project_id, agents, None)
}

pub fn export_skill_to_project_internal_with_commit(
    store: &SkillStore,
    skill_id: String,
    project_id: String,
    agents: Option<Vec<String>>,
    commit_oid: Option<&str>,
) -> Result<(), AppError> {
    let plan = plan_gui_project_skill_deploy(store, &skill_id, &project_id, agents, commit_oid)?;
    apply_project_deploy(store, &plan)
}

fn plan_gui_project_skill_deploy(
    store: &SkillStore,
    skill_id: &str,
    project_id: &str,
    agents: Option<Vec<String>>,
    commit_oid: Option<&str>,
) -> Result<Vec<ProjectDeployTarget>, AppError> {
    let project = get_project_record(store, project_id)?;

    let skill = store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Skill not found"))?;

    let source = PathBuf::from(&skill.central_path);
    let dir_name = sync_engine::target_dir_name(&source, &skill.name);
    ensure_safe_skill_relative_path(&dir_name)?;
    let requested_agent_keys = agents.filter(|items| !items.is_empty()).unwrap_or_else(|| {
        if project.workspace_type == "linked" {
            vec![linked_workspace_agent_key(&project)]
        } else {
            vec!["claude_code".to_string()]
        }
    });
    let agent_keys = if project.workspace_type == "linked" {
        requested_agent_keys
    } else {
        let available_targets: std::collections::HashSet<String> =
            project_agent_targets_for_record(store, &project)
                .into_iter()
                .filter(|target| target.installed && target.enabled)
                .map(|target| target.key)
                .collect();
        // A shared directory's row carries one member's key; map the others
        // onto it so naming any agent that reads the directory works.
        let mut filtered: Vec<String> = Vec::new();
        for key in requested_agent_keys {
            let key = map_to_group_key(store, &project, key);
            if available_targets.contains(&key) && !filtered.contains(&key) {
                filtered.push(key);
            }
        }
        if filtered.is_empty() {
            return Err(AppError::invalid_input(
                "No enabled installed agents selected for this project",
            ));
        }
        filtered
    };

    let plan = plan_project_deploy_with_commit(
        store,
        project_id,
        &[skill_id.to_string()],
        &agent_keys,
        commit_oid,
    )?;
    // The GUI only offers "add" where nothing is there yet, so anything in the
    // way, even this same skill, is refused as before.
    if let Some(occupied) = plan
        .iter()
        .find(|target| target.state != ProjectDeployState::Create)
    {
        return Err(AppError::invalid_input(format!(
            "Skill \"{}\" already exists in this workspace for agent {}",
            skill.name, occupied.agent
        )));
    }
    Ok(plan)
}

/// Read-only Core preview for CLI updates. Both this preview and the
/// mutating update below use the same Git Tree/DeploymentId authority.
/// An update always revalidates under RepoLock: this preview is NOT a lease.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedCopyUpdatePlan {
    pub project_id: String,
    pub skill_relative_path: String,
    pub deployment_id: String,
    pub skill_id: String,
    pub agent: String,
    pub target_path: String,
    pub base_commit_oid: String,
    pub base_tree_oid: String,
    pub target_commit_oid: String,
    pub target_tree_oid: String,
    pub will_change_content: bool,
    pub will_change_version: bool,
    pub uncommitted_central_changes_excluded: bool,
}

/// Resolve exactly one enabled/disabled path. Do not infer management from
/// matching contents and never choose one of two conflicting directories.
pub fn plan_managed_project_copy_update(
    store: &SkillStore,
    project_id: &str,
    skill_relative_path: &str,
    agent: &str,
    commit_oid: Option<&str>,
) -> Result<ManagedCopyUpdatePlan, AppError> {
    ensure_safe_skill_relative_path(skill_relative_path)?;
    let project = get_project_record(store, project_id)?;
    ensure_workspace_path_exists(&project)?;
    let (enabled_root, disabled_root) = resolve_agent_skills_roots(store, &project, agent)
        .ok_or_else(|| AppError::not_found(format!("Unknown project agent: {agent}")))?;
    let enabled = enabled_root.join(skill_relative_path);
    let disabled = disabled_root
        .as_ref()
        .map(|root| root.join(skill_relative_path));
    let enabled_present = std::fs::symlink_metadata(&enabled).is_ok();
    let disabled_present = disabled
        .as_ref()
        .is_some_and(|path| std::fs::symlink_metadata(path).is_ok());
    if enabled_present && disabled_present {
        return Err(AppError::invalid_input(
            "Both enabled and disabled Skill entries exist; update is ambiguous",
        ));
    }
    let (target_path, target_root) = if enabled_present {
        (enabled, enabled_root)
    } else if disabled_present {
        (
            disabled.expect("disabled path exists"),
            disabled_root.expect("disabled root exists"),
        )
    } else {
        return Err(AppError::not_found("Project Skill target not found"));
    };
    ensure_dir_within_root(&target_path, &target_root)?;
    let row = store
        .get_project_skill_deployment(project_id, &target_path.to_string_lossy())
        .map_err(AppError::db)?
        .ok_or_else(|| {
            AppError::invalid_input(
                "Project Copy is not managed; explicit adoption is required before update",
            )
        })?;
    if row.mode != "copy" || row.management_state != "managed" {
        return Err(AppError::invalid_input(
            "Only an explicitly managed Copy can be updated",
        ));
    }
    let base_tree = row
        .base_tree_oid
        .as_deref()
        .ok_or_else(|| AppError::invalid_input("Managed Copy has no trusted Git Tree baseline"))?;
    let base_commit = row.base_commit_oid.as_deref().ok_or_else(|| {
        AppError::invalid_input("Managed Copy has no trusted Git Commit baseline")
    })?;
    let root = central_repo::skills_dir();
    git_skill_versions::verify_deployment_pin(&root, &row.id, base_commit).map_err(AppError::io)?;
    if !git_skill_versions::matches_skill_tree(&root, base_tree, &target_path)
        .map_err(AppError::io)?
    {
        return Err(AppError::invalid_input(
            "Project Skill contains local edits; Copy update refused to avoid losing user data",
        ));
    }
    let target = git_skill_versions::resolve_skill_version(&root, &row.skill_id, commit_oid)
        .map_err(AppError::io)?;
    let dirty = git_skill_versions::has_uncommitted_central_changes(&root).map_err(AppError::io)?;
    Ok(ManagedCopyUpdatePlan {
        project_id: project_id.to_owned(),
        skill_relative_path: skill_relative_path.to_owned(),
        deployment_id: row.id,
        skill_id: row.skill_id,
        agent: agent.to_owned(),
        target_path: target_path.to_string_lossy().to_string(),
        base_commit_oid: base_commit.to_owned(),
        base_tree_oid: base_tree.to_owned(),
        will_change_content: target.tree_oid != base_tree,
        will_change_version: target.commit_oid != base_commit,
        target_commit_oid: target.commit_oid,
        target_tree_oid: target.tree_oid,
        uncommitted_central_changes_excluded: dirty,
    })
}

/// UI applies only the precise version and target that were explicitly
/// previewed. Caller-controlled preview fields are never an authority: replan
/// against registered Project/Agent and validate all target and baseline IDs,
/// then commit using the existing RepoLock-protected Core update.
pub fn apply_managed_project_copy_update_plan(
    store: &SkillStore,
    preview: &ManagedCopyUpdatePlan,
) -> Result<(), AppError> {
    let actual = plan_managed_project_copy_update(
        store,
        &preview.project_id,
        &preview.skill_relative_path,
        &preview.agent,
        Some(&preview.target_commit_oid),
    )?;
    if actual.deployment_id != preview.deployment_id
        || actual.skill_id != preview.skill_id
        || actual.target_path != preview.target_path
        || actual.base_commit_oid != preview.base_commit_oid
        || actual.base_tree_oid != preview.base_tree_oid
        || actual.target_commit_oid != preview.target_commit_oid
        || actual.target_tree_oid != preview.target_tree_oid
    {
        return Err(AppError::invalid_input(
            "Managed Copy preview is stale or does not belong to the selected target",
        ));
    }
    update_managed_project_copy_checked(
        store,
        &preview.project_id,
        Path::new(&actual.target_path),
        Some(&actual.target_commit_oid),
        Some((
            &actual.deployment_id,
            &actual.base_tree_oid,
            &actual.base_commit_oid,
        )),
    )
}

#[tauri::command]
pub async fn plan_project_skill_copy_update(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
    skill_relative_path: String,
    agent: String,
    commit_oid: Option<String>,
) -> Result<ManagedCopyUpdatePlan, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        plan_managed_project_copy_update(
            &store,
            &project_id,
            &skill_relative_path,
            &agent,
            commit_oid.as_deref(),
        )
    })
    .await?
}

#[tauri::command]
pub async fn apply_project_skill_copy_update(
    store: State<'_, Arc<SkillStore>>,
    preview: ManagedCopyUpdatePlan,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        apply_managed_project_copy_update_plan(&store, &preview)
    })
    .await?
}

/// Update a manager-owned Copy using the *recorded Git subtree* as the
/// conflict baseline. Files modified by the project are never overwritten.
pub fn update_managed_project_copy(
    store: &SkillStore,
    project_id: &str,
    target_path: &Path,
    commit_oid: Option<&str>,
) -> Result<(), AppError> {
    update_managed_project_copy_checked(store, project_id, target_path, commit_oid, None)
}

/// Protects CLI preview→apply handoff against another managed operation
/// changing the DeploymentId or Base Tree between the two calls.
/// Both this check and any filesystem publication occur under RepoLock.
pub fn update_managed_project_copy_checked(
    store: &SkillStore,
    project_id: &str,
    target_path: &Path,
    commit_oid: Option<&str>,
    expected_baseline: Option<(&str, &str, &str)>,
) -> Result<(), AppError> {
    let root = central_repo::skills_dir();
    let _lock = RepoLock::acquire_foreground("project Copy update").map_err(AppError::io)?;
    let target_string = target_path.to_string_lossy();
    let deployment = store
        .get_project_skill_deployment(project_id, &target_string)
        .map_err(AppError::db)?
        .ok_or_else(|| {
            AppError::invalid_input(
                "No managed Copy baseline exists; explicit adoption is required",
            )
        })?;
    if deployment.management_state != "managed" || deployment.mode != "copy" {
        return Err(AppError::invalid_input("This target is not a managed Copy"));
    }
    let base_tree = deployment
        .base_tree_oid
        .as_deref()
        .ok_or_else(|| AppError::invalid_input("Managed Copy has no trusted Git Tree baseline"))?;
    if expected_baseline.is_some_and(|(id, tree, commit)| {
        deployment.id != id
            || base_tree != tree
            || deployment.base_commit_oid.as_deref() != Some(commit)
    }) {
        return Err(AppError::invalid_input(
            "Managed Copy baseline changed since preview; re-run projects update",
        ));
    }
    if !git_skill_versions::matches_skill_tree(&root, base_tree, target_path)
        .map_err(AppError::io)?
    {
        return Err(AppError::invalid_input(
            "Project Skill contains local changes; refusing to overwrite the Copy",
        ));
    }
    let wanted = git_skill_versions::resolve_skill_version(&root, &deployment.skill_id, commit_oid)
        .map_err(AppError::io)?;
    // Every adopted Git base must be reachable independently of mutable HEAD.
    // A failed pin refuses the update before touching user files/SQLite.
    git_skill_versions::pin_deployment_version(&root, &deployment.id, &wanted.commit_oid)
        .map_err(AppError::io)?;
    if wanted.tree_oid == base_tree {
        store
            .update_project_skill_deployment_version(
                &deployment.id,
                &wanted.commit_oid,
                &wanted.tree_oid,
                &wanted.path_at_commit,
            )
            .map_err(AppError::db)?;
        return Ok(());
    }

    let operation_id = uuid::Uuid::new_v4().to_string();
    let recovery_home = target_path
        .parent()
        .ok_or_else(|| AppError::invalid_input("Invalid managed Copy path"))?
        .join(".atm-recovery");
    match std::fs::symlink_metadata(&recovery_home) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(AppError::invalid_input("Unsafe Copy recovery directory"));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&recovery_home)?;
        }
        Err(err) => return Err(AppError::io(err)),
    }
    let operation_root = recovery_home.join(&operation_id);
    let now = chrono::Utc::now().timestamp_millis();
    store
        .insert_project_skill_operation(&ProjectSkillOperationRecord {
            id: operation_id.clone(),
            project_id: project_id.to_owned(),
            skill_id: deployment.skill_id.clone(),
            deployment_id: Some(deployment.id.clone()),
            kind: "update".to_owned(),
            phase: "preparing".to_owned(),
            target_path: target_string.to_string(),
            recovery_root: operation_root.to_string_lossy().to_string(),
            base_tree_oid: Some(base_tree.to_owned()),
            new_tree_oid: Some(wanted.tree_oid.clone()),
            created_at: now,
            updated_at: now,
            destination_path: None,
            observed_identity: None,
        })
        .map_err(AppError::db)?;
    std::fs::create_dir(&operation_root)?;
    let staged = operation_root.join("new");
    let backup = operation_root.join("previous");
    git_skill_versions::materialize_skill_version(&root, &wanted, &staged).map_err(AppError::io)?;
    store
        .transition_project_skill_operation(&operation_id, "preparing", "publishing")
        .map_err(AppError::db)?;

    // Verify immediately before moving the old Copy. An outsider may write
    // through an already-open file handle even after the move; the previous
    // directory is therefore retained persistently after success.
    if !git_skill_versions::matches_skill_tree(&root, base_tree, target_path)
        .map_err(AppError::io)?
    {
        let _ = store.transition_project_skill_operation(
            &operation_id,
            "publishing",
            "recovery_available",
        );
        return Err(AppError::invalid_input(
            "Project Skill changed during update; original Copy preserved",
        ));
    }
    rename_directory_no_replace(target_path, &backup)?;
    if !git_skill_versions::matches_skill_tree(&root, base_tree, &backup).unwrap_or(false) {
        return Err(restore_failed_project_copy_update(
            store,
            &operation_id,
            &root,
            &wanted.tree_oid,
            target_path,
            &backup,
            &operation_root,
            "Project Copy changed while moving it to recovery",
        ));
    }
    // The previous directory is still available at operation_root/previous.
    // Never remove this backup solely because a snapshot compared equal once.
    if let Err(err) = rename_directory_no_replace(&staged, target_path) {
        return Err(restore_failed_project_copy_update(
            store,
            &operation_id,
            &root,
            &wanted.tree_oid,
            target_path,
            &backup,
            &operation_root,
            &format!("Unable to publish the new Copy: {err}"),
        ));
    }
    if !git_skill_versions::matches_skill_tree(&root, &wanted.tree_oid, target_path)
        .unwrap_or(false)
    {
        return Err(restore_failed_project_copy_update(
            store,
            &operation_id,
            &root,
            &wanted.tree_oid,
            target_path,
            &backup,
            &operation_root,
            "Published Copy differs from selected Git version",
        ));
    }
    if let Err(err) = store.commit_project_skill_update(
        &operation_id,
        &deployment.id,
        base_tree,
        &wanted.commit_oid,
        &wanted.tree_oid,
        &wanted.path_at_commit,
    ) {
        return Err(restore_failed_project_copy_update(
            store,
            &operation_id,
            &root,
            &wanted.tree_oid,
            target_path,
            &backup,
            &operation_root,
            &format!("Copy update database commit failed: {err}"),
        ));
    }
    // Deliberately keep operation_root/previous in cleanup_pending:
    // another process might still be writing to a descriptor of the old
    // directory. Cleanup requires an explicit, separately authorized step.
    Ok(())
}

/// Never discard original or unexpectedly changed user data when an update
/// fails. Restoring the old path uses no-replace rename, and an occupied path
/// is never removed. The journal stays available for startup diagnostics.
fn restore_failed_project_copy_update(
    store: &SkillStore,
    operation_id: &str,
    root: &Path,
    new_tree: &str,
    target: &Path,
    backup: &Path,
    operation_root: &Path,
    reason: &str,
) -> AppError {
    if std::fs::symlink_metadata(target).is_ok()
        && git_skill_versions::matches_skill_tree(root, new_tree, target).unwrap_or(false)
    {
        let _ = rename_directory_no_replace(target, &operation_root.join("failed-new"));
    }
    let restored = std::fs::symlink_metadata(target).is_err()
        && std::fs::symlink_metadata(backup).is_ok()
        && rename_directory_no_replace(backup, target).is_ok();
    let phase = if restored {
        "recovery_available"
    } else {
        "needs_attention"
    };
    let _ = store.transition_project_skill_operation(operation_id, "publishing", phase);
    AppError::invalid_input(format!(
        "{reason}; old Copy {} and recovery directory {}",
        if restored {
            "restored"
        } else {
            "retained for recovery"
        },
        operation_root.display(),
    ))
}

#[tauri::command]
pub async fn update_project_skill_from_center(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
    skill_relative_path: String,
    agent: String,
    commit_oid: Option<String>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ensure_safe_skill_relative_path(&skill_relative_path)?;
        let record = get_project_record(&store, &project_id)?;
        let (skills_root, disabled_root) = resolve_agent_skills_roots(&store, &record, &agent)
            .ok_or_else(|| AppError::not_found(format!("Unknown agent: {agent}")))?;
        let normal = skills_root.join(&skill_relative_path);
        let disabled = disabled_root
            .as_ref()
            .map(|root| root.join(&skill_relative_path));
        let (target_path, root) = if std::fs::symlink_metadata(&normal).is_ok() {
            (normal, skills_root)
        } else if let Some(disabled_path) =
            disabled.filter(|p| std::fs::symlink_metadata(p).is_ok())
        {
            (
                disabled_path,
                disabled_root.expect("disabled root was checked"),
            )
        } else {
            return Err(AppError::not_found("Project Skill directory not found"));
        };
        ensure_dir_within_root(&target_path, &root)?;
        if store
            .get_project_skill_deployment(&project_id, &target_path.to_string_lossy())
            .map_err(AppError::db)?
            .is_some()
        {
            return update_managed_project_copy(
                &store,
                &project_id,
                &target_path,
                commit_oid.as_deref(),
            );
        }

        // Unknown real directories are external/untracked regardless of how
        // closely they resemble a central Skill. Finding them is not consent.
        let meta = std::fs::symlink_metadata(&target_path)?;
        if !meta.file_type().is_symlink() {
            return Err(AppError::invalid_input(
                "Project Copy is not managed; explicitly adopt it before updating",
            ));
        }
        if commit_oid.is_some() {
            return Err(AppError::invalid_input(
                "A fixed historical CommitId cannot be deployed as a Symlink",
            ));
        }
        let configs = agent_skill_configs(&store);
        let variants = read_workspace_skills(&record, &configs);
        let variant = variants
            .iter()
            .find(|v| v.path == target_path.to_string_lossy())
            .ok_or_else(|| AppError::not_found("Project link is not a known Skill"))?;
        let all = store.get_all_skills().map_err(AppError::db)?;
        let managed = find_best_center_match(variant, &all)
            .ok_or_else(|| AppError::not_found("No matching central Skill"))?;
        let source = PathBuf::from(&managed.central_path);
        if !sync_engine::is_target_current(
            &source,
            &target_path,
            sync_engine::SyncMode::Symlink,
            None,
            None,
        ) {
            return Err(AppError::invalid_input(
                "Unmanaged or foreign Symlink cannot be replaced automatically",
            ));
        }
        Ok(())
    })
    .await?
}

/// The single Core entry point for a project Skill path migration. Toggle
/// changes location, not managementState; neither source content nor a
/// central-Store match is treated as an adoption authorization.
pub fn toggle_project_skill_internal(
    store: &SkillStore,
    project_id: &str,
    skill_relative_path: &str,
    agent: &str,
    enabled: bool,
) -> Result<(), AppError> {
    let _lock = RepoLock::acquire_foreground("project Skill toggle").map_err(AppError::io)?;
    ensure_safe_skill_relative_path(skill_relative_path)?;
    let record = get_project_record(store, project_id)?;
    ensure_workspace_path_exists(&record)?;
    let (enabled_root, disabled_root) = resolve_agent_skills_roots(store, &record, agent)
        .ok_or_else(|| AppError::not_found(format!("Unknown agent: {agent}")))?;
    let disabled_root = disabled_root.ok_or_else(|| {
        AppError::invalid_input("This workspace does not support disabling skills")
    })?;
    let (source, source_root, destination, destination_root) = if enabled {
        (
            disabled_root.join(skill_relative_path),
            &disabled_root,
            enabled_root.join(skill_relative_path),
            &enabled_root,
        )
    } else {
        (
            enabled_root.join(skill_relative_path),
            &enabled_root,
            disabled_root.join(skill_relative_path),
            &disabled_root,
        )
    };
    let old = std::fs::symlink_metadata(&source);
    let new = std::fs::symlink_metadata(&destination);
    if old.is_err() && new.is_ok() {
        ensure_dir_within_root(&destination, destination_root)?;
        if store
            .get_project_skill_deployment(project_id, &source.to_string_lossy())
            .map_err(AppError::db)?
            .is_some()
        {
            return Err(AppError::invalid_input(
                "Skill was moved but its deployment record is stale; recovery required",
            ));
        }
        return Ok(());
    }
    if old.is_err() {
        return Err(AppError::not_found("Skill directory not found"));
    }
    if new.is_ok() {
        return Err(AppError::invalid_input(
            "Both enabled and disabled Skill entries exist; refusing to remove either",
        ));
    }
    ensure_dir_within_root(&source, source_root)?;
    let observed = project_target_identity(&source)
        .ok_or_else(|| AppError::not_found("Skill target disappeared"))?;
    let observed_string = format!("{}:{}", observed.0, observed.1);
    let old_record = store
        .get_project_skill_deployment(project_id, &source.to_string_lossy())
        .map_err(AppError::db)?;
    if let Some(ref row) = old_record {
        if row.management_state != "managed" {
            return Err(AppError::invalid_input("Skill is not under management"));
        }
        if row.mode == "symlink" && !managed_symlink_matches(row, &source) {
            return Err(AppError::invalid_input("Manager Symlink identity changed"));
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let recovery_root = create_project_recovery_root(&source, &id)?;
    let now = chrono::Utc::now().timestamp_millis();
    store
        .insert_project_skill_operation(&ProjectSkillOperationRecord {
            id: id.clone(),
            project_id: project_id.to_string(),
            skill_id: old_record
                .as_ref()
                .map(|r| r.skill_id.clone())
                .unwrap_or_else(|| "external-unmanaged".to_string()),
            deployment_id: old_record.as_ref().map(|r| r.id.clone()),
            kind: "toggle".to_string(),
            phase: "preparing".to_string(),
            target_path: source.to_string_lossy().to_string(),
            recovery_root: recovery_root.to_string_lossy().to_string(),
            base_tree_oid: old_record.as_ref().and_then(|r| r.base_tree_oid.clone()),
            new_tree_oid: None,
            created_at: now,
            updated_at: now,
            destination_path: Some(destination.to_string_lossy().to_string()),
            observed_identity: Some(observed_string.clone()),
        })
        .map_err(AppError::db)?;
    std::fs::create_dir_all(&recovery_root)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    store
        .transition_project_skill_operation(&id, "preparing", "publishing")
        .map_err(AppError::db)?;
    if project_target_identity(&source) != Some(observed) {
        let _ = store.transition_project_skill_operation(&id, "publishing", "needs_attention");
        return Err(AppError::invalid_input(
            "Skill changed before enable/disable rename",
        ));
    }
    rename_directory_no_replace(&source, &destination)?;
    if project_target_identity(&destination) != Some(observed) {
        let restored = std::fs::symlink_metadata(&source).is_err()
            && rename_directory_no_replace(&destination, &source).is_ok();
        let _ = store.transition_project_skill_operation(
            &id,
            "publishing",
            if restored {
                "recovery_available"
            } else {
                "needs_attention"
            },
        );
        return Err(AppError::invalid_input(
            "Enabled/disabled entry changed during move; recovery required",
        ));
    }
    if let Err(err) = store.commit_project_skill_toggle(&id) {
        let restored = std::fs::symlink_metadata(&source).is_err()
            && project_target_identity(&destination) == Some(observed)
            && rename_directory_no_replace(&destination, &source).is_ok();
        let _ = store.transition_project_skill_operation(
            &id,
            "publishing",
            if restored {
                "recovery_available"
            } else {
                "needs_attention"
            },
        );
        return Err(AppError::invalid_input(format!(
            "Skill toggle DB commit failed ({err}); previous entry {} at {}",
            if restored {
                "restored"
            } else {
                "retained for recovery"
            },
            if restored {
                source.display().to_string()
            } else {
                destination.display().to_string()
            },
        )));
    }
    Ok(())
}

#[tauri::command]
pub async fn toggle_project_skill(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
    skill_relative_path: String,
    agent: String,
    enabled: bool,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        toggle_project_skill_internal(&store, &project_id, &skill_relative_path, &agent, enabled)
    })
    .await?
}

#[tauri::command]
pub async fn delete_project_skill(
    store: State<'_, Arc<SkillStore>>,
    project_id: String,
    skill_relative_path: String,
    agent: String,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // The GUI is a normal undeploy, not an unscoped destructive grant.
        // Preflight UI sync_status is neither ownership nor authorization.
        // An explicitly authorized external/modified deletion will require
        // a separate Core operation carrying target-scoped consent.
        delete_project_skill_internal(&store, project_id, skill_relative_path, agent, false)
    })
    .await?
}

/// Shared Core delete boundary. Ordinary removal requires a manager-owned,
/// unchanged Copy (or a recognized manager Symlink); explicit force is a
/// separately requested destructive CLI action, not inferred from in_sync.
fn quarantine_project_skill_delete(
    store: &SkillStore,
    project_id: &str,
    target: &Path,
    explicit_force: bool,
    authorized_identity: Option<(u64, u64)>,
) -> Result<(), AppError> {
    let _lock = RepoLock::acquire_foreground("project Skill delete").map_err(AppError::io)?;
    let meta = std::fs::symlink_metadata(target)?;
    let is_link = meta.file_type().is_symlink();
    if !is_link && !meta.is_dir() {
        return Err(AppError::invalid_input(
            "Skill deletion target is not a directory or Symlink",
        ));
    }
    let original_identity = project_target_identity(target)
        .ok_or_else(|| AppError::not_found("Deletion target disappeared"))?;
    if authorized_identity.is_some_and(|identity| identity != original_identity) {
        return Err(AppError::invalid_input(
            "Destructive authorization expired: target identity changed before isolation",
        ));
    }
    let old = store
        .get_project_skill_deployment(project_id, &target.to_string_lossy())
        .map_err(AppError::db)?;
    if !explicit_force {
        let row = old.as_ref().ok_or_else(|| {
            AppError::invalid_input(
                "Cannot delete an external Skill without explicit target authorization",
            )
        })?;
        if row.management_state != "managed" {
            return Err(AppError::invalid_input("Skill is not under management"));
        }
        if is_link {
            if !managed_symlink_matches(row, target) {
                return Err(AppError::invalid_input(
                    "External or replaced Symlink requires explicit authorization",
                ));
            }
        } else {
            if row.mode != "copy" {
                return Err(AppError::invalid_input("Not an authorized managed Copy"));
            }
            let tree = row
                .base_tree_oid
                .as_deref()
                .ok_or_else(|| AppError::invalid_input("Missing trusted deletion baseline"))?;
            if !git_skill_versions::matches_skill_tree(&central_repo::skills_dir(), tree, target)
                .map_err(AppError::io)?
            {
                return Err(AppError::invalid_input(
                    "Project Skill differs from its managed baseline; explicit deletion required",
                ));
            }
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let recovery_home = target
        .parent()
        .ok_or_else(|| AppError::invalid_input("Skill target has no parent"))?
        .join(".atm-recovery");
    match std::fs::symlink_metadata(&recovery_home) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(AppError::invalid_input(
                "Unsafe Copy deletion recovery location",
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&recovery_home)?;
        }
        Err(err) => return Err(AppError::io(err)),
    }
    let recovery_root = recovery_home.join(&id);
    let backup = recovery_root.join("previous");
    let now = chrono::Utc::now().timestamp_millis();
    store
        .insert_project_skill_operation(&ProjectSkillOperationRecord {
            id: id.clone(),
            project_id: project_id.to_owned(),
            skill_id: old
                .as_ref()
                .map(|o| o.skill_id.clone())
                .unwrap_or_else(|| "external-unmanaged".to_owned()),
            deployment_id: old.as_ref().map(|o| o.id.clone()),
            kind: "delete".to_owned(),
            phase: "preparing".to_owned(),
            target_path: target.to_string_lossy().to_string(),
            recovery_root: recovery_root.to_string_lossy().to_string(),
            base_tree_oid: old.as_ref().and_then(|o| o.base_tree_oid.clone()),
            new_tree_oid: None,
            created_at: now,
            updated_at: now,
            destination_path: None,
            observed_identity: None,
        })
        .map_err(AppError::db)?;
    std::fs::create_dir(&recovery_root)?;
    store
        .transition_project_skill_operation(&id, "preparing", "publishing")
        .map_err(AppError::db)?;
    rename_directory_no_replace(target, &backup)?;
    // The authorized inode must still be the one that was isolated.
    if !is_same_isolated_target(&backup, original_identity) {
        let restored = std::fs::symlink_metadata(target).is_err()
            && rename_directory_no_replace(&backup, target).is_ok();
        let _ = store.transition_project_skill_operation(
            &id,
            "publishing",
            if restored {
                "recovery_available"
            } else {
                "needs_attention"
            },
        );
        return Err(AppError::invalid_input(format!(
            "Isolated deletion target differs from authorized object; {} at {}",
            if restored {
                "restored"
            } else {
                "retained for recovery"
            },
            if restored { target } else { backup.as_path() }.display(),
        )));
    }
    // Between the preflight and rename, an external writer may alter a Copy.
    // In ordinary removal that is a conflict, not implicit permission.
    let moved_is_clean = if is_link {
        old.as_ref()
            .is_some_and(|record| managed_symlink_matches(record, &backup))
    } else {
        old.as_ref()
            .and_then(|row| row.base_tree_oid.as_deref())
            .is_some_and(|tree| {
                git_skill_versions::matches_skill_tree(&central_repo::skills_dir(), tree, &backup)
                    .unwrap_or(false)
            })
    };
    if !explicit_force && !moved_is_clean {
        let restored = rename_directory_no_replace(&backup, target).is_ok();
        let _ = store.transition_project_skill_operation(
            &id,
            "publishing",
            if restored {
                "recovery_available"
            } else {
                "needs_attention"
            },
        );
        return Err(AppError::invalid_input(format!(
            "Copy changed while moving it; contents {} at {}",
            if restored { "restored" } else { "preserved" },
            if restored { target } else { backup.as_path() }.display(),
        )));
    }
    if let Err(err) = store.commit_project_skill_delete(&id, project_id, &target.to_string_lossy())
    {
        let restored = rename_directory_no_replace(&backup, target).is_ok();
        let _ = store.transition_project_skill_operation(
            &id,
            "publishing",
            if restored {
                "recovery_available"
            } else {
                "needs_attention"
            },
        );
        return Err(AppError::invalid_input(format!(
            "Skill deletion database commit failed ({err}); previous content {} at {}",
            if restored { "restored" } else { "preserved" },
            if restored { target } else { backup.as_path() }.display(),
        )));
    }
    // Success is not permission to destroy old bytes. Keep the quarantine
    // under cleanup_pending; another process may still hold a file handle.
    Ok(())
}

/// Remove one skill directory from a project workspace: the enabled copy if
/// there is one, else the disabled one (the slot the GUI has always picked).
/// Without `force`, refuses unless that very directory is a recognizable
/// skill that is safe to remove.
pub fn delete_project_skill_internal(
    store: &SkillStore,
    project_id: String,
    skill_relative_path: String,
    agent: String,
    force: bool,
) -> Result<(), AppError> {
    ensure_safe_skill_relative_path(&skill_relative_path)?;

    let record = get_project_record(store, &project_id)?;
    ensure_workspace_path_exists(&record)?;

    let (skills_root, disabled_root) = resolve_agent_skills_roots(store, &record, &agent)
        .ok_or_else(|| AppError::not_found(format!("Unknown agent: {}", agent)))?;
    let skills_dir = skills_root.join(&skill_relative_path);
    let disabled_dir = disabled_root
        .as_ref()
        .map(|root| root.join(&skill_relative_path));

    let (target, target_root) = if skills_dir.is_dir() {
        (skills_dir, skills_root)
    } else if let Some(disabled_dir) = disabled_dir.filter(|path| path.is_dir()) {
        (
            disabled_dir,
            disabled_root.expect("present when disabled_dir exists"),
        )
    } else {
        return Err(AppError::not_found("Skill directory not found"));
    };

    ensure_dir_within_root(&target, &target_root)?;
    // Check the directory that is about to go, not one found by name: the
    // enabled and disabled slots can hold different things.
    if !force {
        ensure_removable(store, &record, &target)?;
    }
    quarantine_project_skill_delete(store, &project_id, &target, force, None)
}

/// Remove exactly the copy a removal plan named. Without `force`, the copy is
/// checked again first.
pub fn remove_planned_project_copy(
    store: &SkillStore,
    project_id: &str,
    target: &ProjectRemovalTarget,
    force: bool,
) -> Result<(), AppError> {
    let record = get_project_record(store, project_id)?;
    ensure_workspace_path_exists(&record)?;
    let group = project_agent_group(store, &record, &target.agent)
        .ok_or_else(|| AppError::not_found(format!("Unknown agent: {}", target.agent)))?;
    let path = PathBuf::from(&target.path);
    let root = std::iter::once(PathBuf::from(&group.skills_root))
        .chain(group.disabled_root.clone())
        .find(|root| path.starts_with(root))
        .ok_or_else(|| AppError::invalid_input("Invalid skill directory path"))?;
    ensure_dir_within_root(&path, &root)?;
    if target.expected_target_identity.is_none()
        || target.expected_target_identity != project_target_identity(&path)
    {
        return Err(AppError::invalid_input(
            "The planned Skill target was replaced; destructive authorization is stale",
        ));
    }
    if let Some(existing) = store
        .get_project_skill_deployment(project_id, &path.to_string_lossy())
        .map_err(AppError::db)?
    {
        if existing.skill_id != target.skill_id {
            return Err(AppError::invalid_input(
                "Planned SkillId differs from the registered deployment owner",
            ));
        }
    }
    if !force {
        ensure_removable(store, &record, &path)?;
    }
    quarantine_project_skill_delete(
        store,
        project_id,
        &path,
        force,
        target.expected_target_identity,
    )
}

/// Refuse unless what sits at `target` is a scanned project skill whose
/// removal cannot lose content the library lacks.
fn ensure_removable(
    store: &SkillStore,
    record: &ProjectRecord,
    target: &Path,
) -> Result<(), AppError> {
    let configs = agent_skill_configs(store);
    let variants = read_workspace_skills(record, &configs);
    let all_managed = store.get_all_skills().map_err(AppError::db)?;
    let reason = match variants.iter().find(|v| Path::new(&v.path) == target) {
        None => Some("is not a recognizable skill directory".to_string()),
        Some(variant) => {
            let matched = find_best_center_match(variant, &all_managed);
            let status = classify_sync_status(variant, matched);
            (!managed_removal_is_safe(store, &record.id, variant, matched, &status)?)
                .then(|| unsafe_removal_reason(&status))
        }
    };
    match reason {
        None => Ok(()),
        Some(reason) => Err(AppError::target_conflict(
            "Refusing to remove a project skill copy that may hold content not in the library",
            vec![TargetConflictDetail {
                path: target.to_string_lossy().to_string(),
                reason,
            }],
        )),
    }
}

// ── Planning shared by the project commands and the projects CLI ──────────
//
// The GUI decides a lot in the frontend before it calls a project command: it
// only offers "add" where nothing sits in the way, and it asks before removing
// a copy that is not in sync. The CLI has no such frontend, so the checks it
// needs live here, where a dry run and the real run read the same answer.

fn get_project_record(store: &SkillStore, project_id: &str) -> Result<ProjectRecord, AppError> {
    store
        .get_project_by_id(project_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Workspace not found"))
}

/// A workspace whose folder is gone scans as "no skills", and a deploy into it
/// would `create_dir_all` the project back into existence. Refuse instead.
fn ensure_workspace_path_exists(rec: &ProjectRecord) -> Result<(), AppError> {
    if Path::new(&rec.path).is_dir() {
        Ok(())
    } else {
        Err(AppError::invalid_input(format!(
            "Project path does not exist: {}",
            rec.path
        )))
    }
}

/// One project skills directory and the agents that read it. `key` is the
/// agent the directory's row is filed under (the first registered adapter
/// that reads it), which is the key project scans report as `agent`.
#[derive(Serialize, Clone, Debug)]
pub struct ProjectAgentGroup {
    pub key: String,
    pub shared_by: Vec<String>,
    pub skills_root: String,
    #[serde(skip)]
    pub disabled_root: Option<PathBuf>,
}

/// Resolve any agent key to the project directory it writes into. Warp, Cline
/// and GitLab Duo all read `.agents/skills`, and the row is filed under one of
/// them, so a caller naming another member must land on the same directory
/// instead of being told the agent is not available.
pub fn project_agent_group(
    store: &SkillStore,
    rec: &ProjectRecord,
    agent: &str,
) -> Option<ProjectAgentGroup> {
    if rec.workspace_type == "linked" {
        let key = linked_workspace_agent_key(rec);
        if key != agent {
            return None;
        }
        return Some(ProjectAgentGroup {
            shared_by: vec![key.clone()],
            key,
            skills_root: rec.path.clone(),
            disabled_root: rec.disabled_path.as_ref().map(PathBuf::from),
        });
    }

    let adapters = tool_adapters::all_tool_adapters(store);
    let dir = adapters
        .iter()
        .find(|adapter| adapter.key == agent)?
        .project_relative_skills_dir()
        .to_string();
    if dir.is_empty() {
        return None;
    }
    let key = agent_skill_configs(store)
        .into_iter()
        .find(|config| config.relative_skills_dir == dir)?
        .key;
    let shared_by = adapters
        .iter()
        .filter(|adapter| adapter.project_relative_skills_dir() == dir)
        .map(|adapter| adapter.key.clone())
        .collect();
    let (skills_root, disabled_root) = resolve_agent_skills_roots(store, rec, &key)?;
    Some(ProjectAgentGroup {
        key,
        shared_by,
        skills_root: skills_root.to_string_lossy().to_string(),
        disabled_root,
    })
}

/// Every directory group of a workspace, in the order its rows are listed.
pub fn project_agent_groups(store: &SkillStore, rec: &ProjectRecord) -> Vec<ProjectAgentGroup> {
    if rec.workspace_type == "linked" {
        return project_agent_group(store, rec, &linked_workspace_agent_key(rec))
            .into_iter()
            .collect();
    }
    agent_skill_configs(store)
        .into_iter()
        .filter_map(|config| project_agent_group(store, rec, &config.key))
        .collect()
}

fn map_to_group_key(store: &SkillStore, rec: &ProjectRecord, agent: String) -> String {
    project_agent_group(store, rec, &agent)
        .map(|group| group.key)
        .unwrap_or(agent)
}

/// The scanned copy of a skill directory for one directory group. Exact match
/// first; on a case-insensitive filesystem the directory may differ in case.
fn find_variant<'a>(
    variants: &'a [project_scanner::ProjectSkillInfo],
    agent: &str,
    relative_path: &str,
) -> Option<&'a project_scanner::ProjectSkillInfo> {
    variants
        .iter()
        .find(|v| v.agent == agent && v.relative_path == relative_path)
        .or_else(|| {
            variants
                .iter()
                .find(|v| v.agent == agent && v.relative_path.eq_ignore_ascii_case(relative_path))
        })
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDeployState {
    Create,
    AlreadyDeployed,
    Conflict,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProjectDeployTarget {
    pub skill_id: String,
    pub skill: String,
    pub agent: String,
    pub shared_by: Vec<String>,
    pub path: String,
    pub state: ProjectDeployState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_status: Option<String>,
    #[serde(skip)]
    source: PathBuf,
    #[serde(skip)]
    project_id: String,
    #[serde(skip)]
    version: Option<git_skill_versions::GitSkillVersion>,
}

impl ProjectDeployTarget {
    pub fn uses_git_copy(&self) -> bool {
        self.version.is_some()
    }
}

/// Decide, without writing, what deploying `skill_ids` to `agents` in a
/// project would do for each (skill, directory) pair: create it, find this
/// very skill already there, or find something else in the way.
///
/// Agents must be installed and enabled (any member of a shared directory
/// counts, as in the project's agent rows); linked workspaces accept only
/// their own key.
pub fn plan_project_deploy(
    store: &SkillStore,
    project_id: &str,
    skill_ids: &[String],
    agents: &[String],
) -> Result<Vec<ProjectDeployTarget>, AppError> {
    plan_project_deploy_with_commit(store, project_id, skill_ids, agents, None)
}

/// Plan a Git-pinned project Copy. A selected CommitId cannot represent a
/// Symlink deployment because that link tracks a mutable working directory.
pub fn plan_project_deploy_with_commit(
    store: &SkillStore,
    project_id: &str,
    skill_ids: &[String],
    agents: &[String],
    commit_oid: Option<&str>,
) -> Result<Vec<ProjectDeployTarget>, AppError> {
    let project = get_project_record(store, project_id)?;
    ensure_workspace_path_exists(&project)?;
    if agents.is_empty() {
        return Err(AppError::invalid_input("No agent selected"));
    }

    let rows = project_agent_targets_for_record(store, &project);
    let mut groups: Vec<ProjectAgentGroup> = Vec::new();
    for agent in agents {
        let group = project_agent_group(store, &project, agent)
            .ok_or_else(|| AppError::not_found(format!("Unknown agent: {}", agent)))?;
        if project.workspace_type != "linked" {
            let row = rows
                .iter()
                .find(|row| row.key == group.key)
                .ok_or_else(|| AppError::not_found(format!("Unknown agent: {}", agent)))?;
            if !row.installed {
                return Err(AppError::invalid_input(format!(
                    "Agent is not installed: {}",
                    agent
                )));
            }
            if !row.enabled {
                return Err(AppError::invalid_input(format!(
                    "Agent is disabled: {}",
                    agent
                )));
            }
        }
        if !groups.iter().any(|existing| existing.key == group.key) {
            groups.push(group);
        }
    }

    let configs = agent_skill_configs(store);
    let variants = read_workspace_skills(&project, &configs);
    let all_managed = store.get_all_skills().map_err(AppError::db)?;
    let configured_mode = store.get_setting("sync_mode").map_err(AppError::db)?;

    let mut plan = Vec::new();
    for skill_id in skill_ids {
        let skill = store
            .get_skill_by_id(skill_id)
            .map_err(AppError::db)?
            .ok_or_else(|| AppError::not_found("Skill not found"))?;
        let source = PathBuf::from(&skill.central_path);
        for group in &groups {
            let mode = sync_engine::sync_mode_for_tool(&group.key, configured_mode.as_deref());
            // A fixed Git Commit can remain deployable after its working-tree
            // directory was renamed/deleted. A Symlink cannot.
            if matches!(mode, sync_engine::SyncMode::Symlink) && !source.is_dir() {
                return Err(AppError::not_found(format!(
                    "Library copy of \"{}\" is missing: {}",
                    skill.name,
                    source.display(),
                )));
            }
            if commit_oid.is_some() && matches!(mode, sync_engine::SyncMode::Symlink) {
                return Err(AppError::invalid_input(
                    "A selected Git CommitId requires Copy mode; Symlinks track mutable content",
                ));
            }
            let version = if matches!(mode, sync_engine::SyncMode::Copy) {
                Some(
                    git_skill_versions::resolve_skill_version(
                        &central_repo::skills_dir(),
                        &skill.id,
                        commit_oid,
                    )
                    .map_err(AppError::io)?,
                )
            } else {
                None
            };
            let dir_name = version
                .as_ref()
                .and_then(|v| Path::new(&v.path_at_commit).file_name())
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| sync_engine::target_dir_name(&source, &skill.name));
            ensure_safe_skill_relative_path(&dir_name)?;
            let enabled_path = Path::new(&group.skills_root).join(&dir_name);
            let disabled_path = group
                .disabled_root
                .as_ref()
                .map(|root| root.join(&dir_name));
            // symlink_metadata, not exists(): a dangling link is in the way too.
            let occupied = std::iter::once(enabled_path.clone())
                .chain(disabled_path)
                .find(|path| std::fs::symlink_metadata(path).is_ok());

            let (state, sync_status, path) = match occupied {
                None => (ProjectDeployState::Create, None, enabled_path),
                Some(path) => {
                    let variant = find_variant(&variants, &group.key, &dir_name);
                    let matched = variant.and_then(|v| find_best_center_match(v, &all_managed));
                    if version.is_some() {
                        // Content equality is not a managed ownership record.
                        // An externally installed same-named Skill is a conflict.
                        let ownership = store
                            .get_project_skill_deployment(project_id, &path.to_string_lossy())
                            .map_err(AppError::db)?;
                        match ownership {
                            Some(row)
                                if row.skill_id == skill.id
                                    && row.mode == "copy"
                                    && row.management_state == "managed" =>
                            {
                                (
                                    ProjectDeployState::AlreadyDeployed,
                                    Some(managed_copy_status(&row, &path)),
                                    path,
                                )
                            }
                            _ => (ProjectDeployState::Conflict, None, path),
                        }
                    } else {
                        match (variant, matched) {
                            (Some(variant), Some(matched)) if matched.id == skill.id => (
                                ProjectDeployState::AlreadyDeployed,
                                Some(classify_sync_status(variant, Some(matched))),
                                path,
                            ),
                            _ => (ProjectDeployState::Conflict, None, path),
                        }
                    }
                }
            };
            plan.push(ProjectDeployTarget {
                skill_id: skill.id.clone(),
                skill: skill.name.clone(),
                agent: group.key.clone(),
                shared_by: group.shared_by.clone(),
                path: path.to_string_lossy().to_string(),
                state,
                sync_status,
                source: source.clone(),
                project_id: project_id.to_string(),
                version,
            });
        }
    }

    // Two library skills whose directories share a name (`a/demo`, `b/demo`)
    // would both be written to one project directory; only the first would
    // land. Refuse the batch instead of reporting both as created.
    let mut collisions: Vec<TargetConflictDetail> = Vec::new();
    for (index, target) in plan.iter().enumerate() {
        if target.state != ProjectDeployState::Create {
            continue;
        }
        if let Some(earlier) = plan[..index].iter().find(|earlier| {
            earlier.state == ProjectDeployState::Create
                && earlier.path == target.path
                && earlier.skill_id != target.skill_id
        }) {
            collisions.push(TargetConflictDetail {
                path: target.path.clone(),
                // Names alone can match (both frontmatter names may be
                // "demo"), so say which library directories collide.
                reason: format!(
                    "skills \"{}\" ({}) and \"{}\" ({}) would both be written here",
                    earlier.skill,
                    earlier.source.display(),
                    target.skill,
                    target.source.display()
                ),
            });
        }
    }
    if !collisions.is_empty() {
        return Err(AppError::target_conflict(
            format!(
                "Refusing to deploy: {} project target(s) would receive two different skills",
                collisions.len()
            ),
            collisions,
        ));
    }
    Ok(plan)
}

/// The refusal for a deploy plan that has something else in the way, in the
/// shape the CLI envelope and the frontend already read. `None` when clear.
pub fn project_deploy_conflicts(plan: &[ProjectDeployTarget]) -> Option<AppError> {
    let conflicts: Vec<TargetConflictDetail> = plan
        .iter()
        .filter(|target| target.state == ProjectDeployState::Conflict)
        .map(|target| TargetConflictDetail {
            path: target.path.clone(),
            reason: "exists and is not this library skill".to_string(),
        })
        .collect();
    if conflicts.is_empty() {
        return None;
    }
    Some(AppError::target_conflict(
        format!(
            "Refusing to deploy: {} of {} project target(s) already hold something else",
            conflicts.len(),
            plan.len()
        ),
        conflicts,
    ))
}

/// Write the `Create` targets of a plan. Callers decide first what to do about
/// conflicts and existing copies; NoClobber still refuses anything that
/// appeared since the plan was made.
pub fn apply_project_deploy(
    store: &SkillStore,
    plan: &[ProjectDeployTarget],
) -> Result<(), AppError> {
    let _lock = RepoLock::acquire_foreground("project Skill deploy").map_err(AppError::io)?;
    let configured_mode = store.get_setting("sync_mode").map_err(AppError::db)?;
    // Two agents can resolve to the same project skills root; write it once.
    let mut written: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for target in plan
        .iter()
        .filter(|target| target.state == ProjectDeployState::Create)
    {
        let target_dir = PathBuf::from(&target.path);
        if !written.insert(target_dir.clone()) {
            continue;
        }
        if let Some(parent) = target_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mode = sync_engine::sync_mode_for_tool(&target.agent, configured_mode.as_deref());
        if let Some(version) = target.version.as_ref() {
            if !matches!(mode, sync_engine::SyncMode::Copy) {
                return Err(AppError::invalid_input(
                    "Deployment mode changed during Copy planning",
                ));
            }
            // Keep an operation record before publishing any project bytes.
            // A crash in the rename→SQLite gap is discoverable on restart.
            let operation_id = uuid::Uuid::new_v4().to_string();
            let recovery_home = target_dir
                .parent()
                .expect("project Skill has a parent")
                .join(".atm-recovery");
            match std::fs::symlink_metadata(&recovery_home) {
                Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                    return Err(AppError::invalid_input(
                        "Project Skill recovery location is unsafe",
                    ));
                }
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&recovery_home)?;
                }
                Err(err) => return Err(AppError::io(err)),
            }
            let operation_root = recovery_home.join(&operation_id);
            let now = chrono::Utc::now().timestamp_millis();
            // Re-install a deleted previously managed target with the same
            // DeploymentId; never infer new ownership from identical files.
            let prior = store
                .get_project_skill_deployment(&target.project_id, &target.path)
                .map_err(AppError::db)?;
            if prior.as_ref().is_some_and(|row| {
                row.skill_id != target.skill_id
                    || row.management_state != "managed"
                    || row.mode != "copy"
            }) {
                return Err(AppError::invalid_input(
                    "Existing deployment is not owned by this Skill",
                ));
            }
            let record = ProjectSkillDeploymentRecord {
                id: prior
                    .as_ref()
                    .map(|row| row.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                project_id: target.project_id.clone(),
                skill_id: target.skill_id.clone(),
                target_path: target.path.clone(),
                agent: target.agent.clone(),
                mode: "copy".to_string(),
                management_state: "managed".to_string(),
                management_origin: "deployed_by_manager".to_string(),
                version_match: "exact".to_string(),
                base_commit_oid: Some(version.commit_oid.clone()),
                base_tree_oid: Some(version.tree_oid.clone()),
                path_at_commit: Some(version.path_at_commit.clone()),
                created_at: now,
                updated_at: now,

                symlink_target: None,
                symlink_identity: None,
            };
            // Pin before the first filesystem publication, so a later HEAD
            // rewrite or GC cannot orphan this project's committed baseline.
            git_skill_versions::pin_deployment_version(
                &central_repo::skills_dir(),
                &record.id,
                &version.commit_oid,
            )
            .map_err(AppError::io)?;
            store
                .insert_project_skill_operation(&ProjectSkillOperationRecord {
                    id: operation_id.clone(),
                    project_id: target.project_id.clone(),
                    skill_id: target.skill_id.clone(),
                    deployment_id: Some(record.id.clone()),
                    kind: "deploy".to_owned(),
                    phase: "preparing".to_owned(),
                    target_path: target.path.clone(),
                    recovery_root: operation_root.to_string_lossy().to_string(),
                    base_tree_oid: None,
                    new_tree_oid: Some(version.tree_oid.clone()),
                    created_at: now,
                    updated_at: now,

                    destination_path: None,
                    observed_identity: None,
                })
                .map_err(AppError::db)?;
            std::fs::create_dir(&operation_root)?;
            let staged = operation_root.join("new");
            git_skill_versions::materialize_skill_version(
                &central_repo::skills_dir(),
                version,
                &staged,
            )
            .map_err(AppError::io)?;
            store
                .transition_project_skill_operation(&operation_id, "preparing", "publishing")
                .map_err(AppError::db)?;
            if std::fs::symlink_metadata(&target_dir).is_ok() {
                return Err(AppError::invalid_input(
                    "Project Skill target became occupied; operation preserved for recovery",
                ));
            }
            rename_directory_no_replace(&staged, &target_dir)?;
            match store.commit_project_skill_deploy(&operation_id, &record) {
                Ok(()) => {
                    // Safe cleanup: the staged directory was moved, and this
                    // removes only an *empty* manager-owned operation directory.
                    let _ = std::fs::remove_dir(&operation_root);
                }
                Err(err) => {
                    // Never erase potentially edited bytes just to make an
                    // aborted deployment look atomic. Save them at a stable
                    // recovery path when the target is still the exact Tree.
                    let saved = operation_root.join("recovered");
                    let safe_to_move = git_skill_versions::matches_skill_tree(
                        &central_repo::skills_dir(),
                        &version.tree_oid,
                        &target_dir,
                    )
                    .unwrap_or(false);
                    if safe_to_move && rename_directory_no_replace(&target_dir, &saved).is_ok() {
                        let _ = store.transition_project_skill_operation(
                            &operation_id,
                            "publishing",
                            "recovery_available",
                        );
                        return Err(AppError::invalid_input(format!(
                            "Copy deployment database commit failed ({err});                              unpublished content preserved at {}",
                            saved.display(),
                        )));
                    }
                    let _ = store.transition_project_skill_operation(
                        &operation_id,
                        "publishing",
                        "needs_attention",
                    );
                    return Err(AppError::invalid_input(format!(
                        "Copy deployment database commit failed ({err});                          target was not claimed or deleted, inspect {} and {}",
                        target_dir.display(), operation_root.display(),
                    )));
                }
            }
        } else {
            sync_engine::sync_skill(
                &target.source,
                &target_dir,
                mode,
                sync_engine::ReplacePolicy::NoClobber,
            )
            .map_err(AppError::io)?;
            if matches!(mode, sync_engine::SyncMode::Symlink) {
                // Only register links actually created by this operation.
                // Existing historical/external links do not become owned
                // merely by pointing at the central Skill directory.
                let now = chrono::Utc::now().timestamp_millis();
                let record = ProjectSkillDeploymentRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    project_id: target.project_id.clone(),
                    skill_id: target.skill_id.clone(),
                    target_path: target.path.clone(),
                    agent: target.agent.clone(),
                    mode: "symlink".to_string(),
                    management_state: "managed".to_string(),
                    management_origin: "deployed_by_manager".to_string(),
                    version_match: "mutable_link".to_string(),
                    base_commit_oid: None,
                    base_tree_oid: None,
                    path_at_commit: None,
                    symlink_target: std::fs::read_link(&target_dir)
                        .ok()
                        .map(|link| link.to_string_lossy().to_string()),
                    symlink_identity: project_target_identity_string(&target_dir),
                    created_at: now,
                    updated_at: now,
                };
                if let Err(err) = store.mark_project_skill_symlink_managed(&record) {
                    // A write failure does not grant management. Remove only
                    // the exact manager-created link, never a replaced target.
                    if managed_symlink_matches(&record, &target_dir) {
                        let _ = std::fs::remove_file(&target_dir);
                    }
                    return Err(AppError::db(err));
                }
            }
        }
    }
    Ok(())
}

/// Bind a planned destructive action to the filesystem object that was
/// observed during explicit CLI preflight, not just its editable pathname.
#[cfg(unix)]
fn project_target_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).ok()?;
    Some((m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn project_target_identity(path: &Path) -> Option<(u64, u64)> {
    use std::time::UNIX_EPOCH;
    let m = std::fs::symlink_metadata(path).ok()?;
    let time = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((m.len(), time.as_nanos() as u64))
}

fn is_same_isolated_target(path: &Path, authorized: (u64, u64)) -> bool {
    project_target_identity(path) == Some(authorized)
}

/// Refuse symlinked recovery directories rather than following them into a
/// location outside the project. Caller holds the repository operation lock.
fn create_project_recovery_root(entry: &Path, operation_id: &str) -> Result<PathBuf, AppError> {
    let parent = entry
        .parent()
        .ok_or_else(|| AppError::invalid_input("Skill entry has no parent"))?;
    let recovery_home = parent.join(".atm-recovery");
    match std::fs::symlink_metadata(&recovery_home) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(AppError::invalid_input(
                "Unsafe project Skill recovery directory",
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&recovery_home)?
        }
        Err(error) => return Err(AppError::io(error)),
    };
    let operation_root = recovery_home.join(operation_id);
    if std::fs::symlink_metadata(&operation_root).is_ok() {
        return Err(AppError::invalid_input(
            "Project Skill recovery operation already exists",
        ));
    }
    Ok(operation_root)
}

fn project_target_identity_string(path: &Path) -> Option<String> {
    project_target_identity(path).map(|(dev, ino)| format!("{dev}:{ino}"))
}

/// A matching central path is never proof of management. Only a manager-owned
/// deployment row and its exact link text/object identity confer authority.
fn managed_symlink_matches(row: &ProjectSkillDeploymentRecord, path: &Path) -> bool {
    if row.management_state != "managed" || row.mode != "symlink" {
        return false;
    }
    let Some(expected_target) = row.symlink_target.as_deref() else {
        return false;
    };
    let Some(expected_identity) = row.symlink_identity.as_deref() else {
        return false;
    };
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
        && project_target_identity_string(path).as_deref() == Some(expected_identity)
        && std::fs::read_link(path)
            .ok()
            .is_some_and(|text| text.to_string_lossy() == expected_target)
}

#[derive(Serialize, Clone, Debug)]
pub struct ProjectRemovalTarget {
    pub skill_id: String,
    pub skill: String,
    pub agent: String,
    pub shared_by: Vec<String>,
    pub relative_path: String,
    pub path: String,
    pub sync_status: String,
    /// False when the copy may hold content the library does not have.
    pub safe: bool,
    #[serde(skip)]
    expected_target_identity: Option<(u64, u64)>,
}

/// Only a recorded manager-owned Copy can be removed without explicit force.
/// An externally installed equal-content directory has no management consent.
fn managed_removal_is_safe(
    store: &SkillStore,
    project_id: &str,
    variant: &project_scanner::ProjectSkillInfo,
    matched: Option<&SkillRecord>,
    _sync_status: &str,
) -> Result<bool, AppError> {
    let is_link =
        std::fs::symlink_metadata(&variant.path).is_ok_and(|m| m.file_type().is_symlink());
    if is_link {
        let row = store
            .get_project_skill_deployment(project_id, &variant.path)
            .map_err(AppError::db)?;
        return Ok(row.as_ref().is_some_and(|row| {
            matched.is_some_and(|managed| managed.id == row.skill_id)
                && managed_symlink_matches(row, Path::new(&variant.path))
        }));
    }
    let row = store
        .get_project_skill_deployment(project_id, &variant.path)
        .map_err(AppError::db)?;
    let Some(row) = row else {
        return Ok(false);
    };
    if row.mode != "copy"
        || row.management_state != "managed"
        || !matched.is_some_and(|m| m.id == row.skill_id)
    {
        return Ok(false);
    }
    let Some(tree) = row.base_tree_oid.as_deref() else {
        return Ok(false);
    };
    Ok(git_skill_versions::matches_skill_tree(
        &central_repo::skills_dir(),
        tree,
        Path::new(&variant.path),
    )
    .unwrap_or(false))
}

/// Why a copy is not safe to remove. An in-sync copy can only be unsafe
/// because it holds entries the hash does not see.
fn unsafe_removal_reason(sync_status: &str) -> String {
    if sync_status == "in_sync" {
        "has content not in the library (files the library copy lacks, such as .gitignore, .git or links)".to_string()
    } else {
        format!("has content not in the library (sync status: {sync_status})")
    }
}

/// The project copies of library skills `skill_ids` that an undeploy would
/// remove. Empty `agents` means every directory of the project. Agents that
/// are disabled or uninstalled are accepted, so stale copies can be cleaned.
/// Copies that match no library skill (`project_only`) are never listed.
pub fn plan_project_undeploy(
    store: &SkillStore,
    project_id: &str,
    skill_ids: &[String],
    agents: &[String],
) -> Result<Vec<ProjectRemovalTarget>, AppError> {
    let project = get_project_record(store, project_id)?;
    ensure_workspace_path_exists(&project)?;

    let groups = if agents.is_empty() {
        project_agent_groups(store, &project)
    } else {
        let mut groups: Vec<ProjectAgentGroup> = Vec::new();
        for agent in agents {
            let group = project_agent_group(store, &project, agent)
                .ok_or_else(|| AppError::not_found(format!("Unknown agent: {}", agent)))?;
            if !groups.iter().any(|existing| existing.key == group.key) {
                groups.push(group);
            }
        }
        groups
    };
    for skill_id in skill_ids {
        if store
            .get_skill_by_id(skill_id)
            .map_err(AppError::db)?
            .is_none()
        {
            return Err(AppError::not_found("Skill not found"));
        }
    }

    let configs = agent_skill_configs(store);
    let variants = read_workspace_skills(&project, &configs);
    let all_managed = store.get_all_skills().map_err(AppError::db)?;

    let mut plan = Vec::new();
    for variant in &variants {
        let Some(group) = groups.iter().find(|group| group.key == variant.agent) else {
            continue;
        };
        let Some(matched) = find_best_center_match(variant, &all_managed) else {
            continue;
        };
        if !skill_ids.contains(&matched.id) {
            continue;
        }
        let sync_status = classify_sync_status(variant, Some(matched));
        plan.push(ProjectRemovalTarget {
            skill_id: matched.id.clone(),
            skill: matched.name.clone(),
            agent: group.key.clone(),
            shared_by: group.shared_by.clone(),
            relative_path: variant.relative_path.clone(),
            path: variant.path.clone(),
            safe: managed_removal_is_safe(store, project_id, variant, Some(matched), &sync_status)?,
            sync_status,
            expected_target_identity: project_target_identity(Path::new(&variant.path)),
        });
    }
    Ok(plan)
}

/// The refusal for removals that could lose content. `None` when all are safe.
pub fn project_removal_conflicts(plan: &[ProjectRemovalTarget]) -> Option<AppError> {
    let conflicts: Vec<TargetConflictDetail> = plan
        .iter()
        .filter(|target| !target.safe)
        .map(|target| TargetConflictDetail {
            path: target.path.clone(),
            reason: unsafe_removal_reason(&target.sync_status),
        })
        .collect();
    if conflicts.is_empty() {
        return None;
    }
    Some(AppError::target_conflict(
        format!(
            "Refusing to remove {} project skill copy(ies) that may hold content not in the library",
            conflicts.len()
        ),
        conflicts,
    ))
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectImportAction {
    Create,
    Unchanged,
    Overwrite,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProjectImportResult {
    pub action: ProjectImportAction,
    pub skill_id: Option<String>,
    pub name: String,
    pub central_path: Option<String>,
    pub agent: String,
    pub relative_path: String,
    pub path: String,
}

/// Find the project copy an import reads and decide what it would do to the
/// library. Refuses when more than one copy of the skill in the project holds
/// content the library lacks: there is then no copy that is safe to push, the
/// rule pushSkillToCenterAndAlign (ProjectDetail.tsx) enforces in the GUI.
fn resolve_project_import(
    store: &SkillStore,
    project_id: &str,
    skill_relative_path: &str,
    agent: &str,
) -> Result<(ProjectImportResult, project_scanner::ProjectSkillInfo), AppError> {
    ensure_safe_skill_relative_path(skill_relative_path)?;
    let project = get_project_record(store, project_id)?;
    ensure_workspace_path_exists(&project)?;
    let group_key = map_to_group_key(store, &project, agent.to_string());

    let configs = agent_skill_configs(store);
    let variants = read_workspace_skills(&project, &configs);
    let skill = variants
        .iter()
        .find(|s| s.relative_path == skill_relative_path && s.agent == group_key)
        .cloned()
        .ok_or_else(|| AppError::not_found("Skill not found in workspace"))?;
    let all_managed = store.get_all_skills().map_err(AppError::db)?;

    let key = skill.relative_path.to_lowercase();
    let unsynced: Vec<&project_scanner::ProjectSkillInfo> = variants
        .iter()
        .filter(|v| v.relative_path.to_lowercase() == key)
        .filter(|v| classify_sync_status(v, find_best_center_match(v, &all_managed)) != "in_sync")
        .collect();
    if unsynced.len() > 1 {
        return Err(AppError::target_conflict(
            format!(
                "Refusing to import: {} copies of this skill in the project hold content not in the library, so no single copy is safe to push",
                unsynced.len()
            ),
            unsynced
                .iter()
                .map(|v| TargetConflictDetail {
                    path: v.path.clone(),
                    reason: "another copy of this skill also differs from the library".to_string(),
                })
                .collect(),
        ));
    }

    let matched = find_best_center_match(&skill, &all_managed);
    if matched.is_none() {
        // The same name check install_from_local applies to a new skill, so
        // a dry run refuses a name the import itself would refuse.
        installer::resolve_local_skill_name(Path::new(&skill.path), Some(&skill.name))
            .map_err(|e| AppError::invalid_input(e.to_string()))?;
    }
    let action = match matched {
        None => ProjectImportAction::Create,
        Some(managed) if classify_sync_status(&skill, Some(managed)) == "in_sync" => {
            ProjectImportAction::Unchanged
        }
        Some(_) => ProjectImportAction::Overwrite,
    };
    Ok((
        ProjectImportResult {
            action,
            skill_id: matched.map(|m| m.id.clone()),
            name: matched
                .map(|m| m.name.clone())
                .unwrap_or_else(|| skill.name.clone()),
            central_path: matched.map(|m| m.central_path.clone()),
            agent: group_key,
            relative_path: skill.relative_path.clone(),
            path: skill.path.clone(),
        },
        skill,
    ))
}

/// The refusal an import plan meets without `force`: overwriting a library
/// copy whose content differs. Shared so a dry run refuses what the run would.
pub fn project_import_refusal(plan: &ProjectImportResult, force: bool) -> Option<AppError> {
    if plan.action != ProjectImportAction::Overwrite || force {
        return None;
    }
    Some(AppError::target_conflict(
        format!(
            "Refusing to overwrite the library copy of \"{}\": its content differs from the project copy",
            plan.name
        ),
        vec![TargetConflictDetail {
            path: plan.central_path.clone().unwrap_or_default(),
            reason: "library copy differs from the project copy".to_string(),
        }],
    ))
}

/// What importing a project skill would do, without writing anything.
pub fn plan_project_import(
    store: &SkillStore,
    project_id: &str,
    skill_relative_path: &str,
    agent: &str,
) -> Result<ProjectImportResult, AppError> {
    resolve_project_import(store, project_id, skill_relative_path, agent).map(|(plan, _)| plan)
}

#[cfg(test)]
mod tests {
    use super::{
        classify_sync_status, ensure_distinct_linked_workspace_roots, find_best_center_match,
        project_to_dto, remove_workspace_skill_target, set_project_skill_enabled_state,
    };
    use crate::core::content_hash;
    use crate::core::error::ErrorKind;
    use crate::core::project_scanner::{AgentSkillConfig, ProjectSkillInfo};
    use crate::core::skill_store::{ProjectRecord, SkillRecord, SkillStore};
    use std::fs;
    use tempfile::tempdir;

    /// Several agents share `.agents/skills` in a project, and their row carries
    /// the first one's key. That agent being off must not take the directory
    /// away from Warp; only all of them being off does. Which agent comes first
    /// changes as adapters are added (#605 put Antigravity ahead of Cline), so
    /// the test reads it instead of naming it.
    #[test]
    fn a_shared_project_dir_stays_usable_while_another_agent_reads_it() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("skills.db")).unwrap();
        // A path override counts as installed, whatever is on this machine.
        let warp_path = tmp.path().join("warp-skills").to_string_lossy().to_string();
        store
            .set_setting(
                "custom_tool_paths",
                &serde_json::json!({ "warp": warp_path }).to_string(),
            )
            .unwrap();
        let record = ProjectRecord {
            id: "p".to_string(),
            name: "p".to_string(),
            path: tmp.path().to_string_lossy().to_string(),
            workspace_type: "project".to_string(),
            linked_agent_key: None,
            linked_agent_name: None,
            disabled_path: None,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        let shared_row = |store: &SkillStore| {
            super::project_agent_targets_for_record(store, &record)
                .into_iter()
                .find(|target| target.display_name.contains("Warp"))
                .expect("a row for the shared .agents/skills directory")
        };

        let disable = |store: &SkillStore, keys: &[String]| {
            store
                .set_setting("disabled_tools", &serde_json::to_string(keys).unwrap())
                .unwrap();
        };

        let key = shared_row(&store).key;
        assert_ne!(key, "warp", "the row must be keyed under another agent");
        disable(&store, &[key.clone()]);
        let row = shared_row(&store);
        assert_eq!(row.key, key);
        assert!(row.installed, "Warp is installed");
        assert!(row.enabled, "Warp still reads the directory");

        let sharing: Vec<String> = crate::core::tool_adapters::all_tool_adapters(&store)
            .into_iter()
            .filter(|adapter| adapter.project_relative_skills_dir() == ".agents/skills")
            .map(|adapter| adapter.key)
            .collect();
        disable(&store, &sharing);
        assert!(!shared_row(&store).enabled, "nobody left to read it");
    }

    fn sample_managed_skill(
        central_path: String,
        content_hash: Option<String>,
        updated_at: i64,
    ) -> SkillRecord {
        SkillRecord {
            id: "skill-1".to_string(),
            name: "Example Skill".to_string(),
            description: None,
            source_type: "local".to_string(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path,
            content_hash,
            enabled: true,
            created_at: 0,
            updated_at,
            status: "ok".to_string(),
            update_status: "local_only".to_string(),
            last_checked_at: None,
            last_check_error: None,
        }
    }

    /// Build a library skill with the given identity fields, so a test can
    /// assert the order the layers resolve in.
    fn managed_skill_with_identity(
        id: &str,
        name: &str,
        central_path: String,
        content_hash: Option<String>,
    ) -> SkillRecord {
        SkillRecord {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            source_type: "skillssh".to_string(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path,
            content_hash,
            enabled: true,
            created_at: 0,
            updated_at: 0,
            status: "ok".to_string(),
            update_status: "unknown".to_string(),
            last_checked_at: None,
            last_check_error: None,
        }
    }

    fn sample_project_skill(
        path: String,
        content_hash: Option<String>,
        last_modified_at: Option<i64>,
    ) -> ProjectSkillInfo {
        ProjectSkillInfo {
            name: "Example Skill".to_string(),
            dir_name: "example-skill".to_string(),
            relative_path: "example-skill".to_string(),
            description: None,
            path,
            files: vec!["SKILL.md".to_string()],
            enabled: true,
            agent: "claude_code".to_string(),
            agent_display_name: "Claude Code".to_string(),
            tags: Vec::new(),
            in_center: true,
            sync_status: "project_only".to_string(),
            center_skill_id: Some("skill-1".to_string()),
            last_modified_at,
            content_hash,
        }
    }

    /// Build a project skill with the given directory name and agent, to
    /// stand in for one per-agent copy.
    fn project_skill_with_dir(
        dir_name: &str,
        path: String,
        content_hash: Option<String>,
        agent: &str,
    ) -> ProjectSkillInfo {
        ProjectSkillInfo {
            name: dir_name.to_string(),
            dir_name: dir_name.to_string(),
            relative_path: dir_name.to_string(),
            description: None,
            path,
            files: vec!["SKILL.md".to_string()],
            enabled: true,
            agent: agent.to_string(),
            agent_display_name: agent.to_string(),
            tags: Vec::new(),
            in_center: false,
            sync_status: "project_only".to_string(),
            center_skill_id: None,
            last_modified_at: Some(1_000),
            content_hash,
        }
    }

    /// Directory identity must win over a content hash several skills share.
    #[test]
    fn find_best_center_match_prefers_directory_identity_over_shared_hash() {
        let shared_hash = Some("same-content-hash".to_string());
        let project = project_skill_with_dir(
            "adapt",
            "/tmp/project/.claude/skills/adapt".to_string(),
            shared_hash.clone(),
            "claude_code",
        );
        let all_managed = vec![
            managed_skill_with_identity(
                "adapt-id",
                "adapt",
                "/tmp/center/adapt".to_string(),
                shared_hash.clone(),
            ),
            managed_skill_with_identity(
                "polish-id",
                "polish",
                "/tmp/center/polish".to_string(),
                shared_hash,
            ),
        ];

        let matched = find_best_center_match(&project, &all_managed).unwrap();

        assert_eq!(matched.id, "adapt-id");
    }

    /// The central directory name must win over a frontmatter name that repeats.
    #[test]
    fn find_best_center_match_uses_central_directory_before_frontmatter_name() {
        let project = project_skill_with_dir(
            "adapt",
            "/tmp/project/.claude/skills/adapt".to_string(),
            None,
            "claude_code",
        );
        let all_managed = vec![
            managed_skill_with_identity(
                "adapt-id",
                "impeccable",
                "/tmp/center/adapt".to_string(),
                None,
            ),
            managed_skill_with_identity(
                "layout-id",
                "impeccable",
                "/tmp/center/layout".to_string(),
                None,
            ),
        ];

        let matched = find_best_center_match(&project, &all_managed).unwrap();

        assert_eq!(matched.id, "adapt-id");
    }

    /// A unique content hash outranks a directory name owned by a different
    /// skill. A library holding both "Code Review" and "code-review" exports
    /// the first under the slug `code-review`, which is the second one's
    /// library directory — matching on the directory binds the copy to the
    /// wrong row, and that row is also where an import writes back.
    #[test]
    fn find_best_center_match_prefers_a_unique_hash_over_another_skills_directory() {
        let exported_hash = Some("code-review-content".to_string());
        let project = project_skill_with_dir(
            "code-review",
            "/tmp/project/.claude/skills/code-review".to_string(),
            exported_hash.clone(),
            "claude_code",
        );
        let all_managed = vec![
            managed_skill_with_identity(
                "spaced-id",
                "Code Review",
                "/tmp/center/Code Review".to_string(),
                exported_hash,
            ),
            managed_skill_with_identity(
                "slug-id",
                "code-review",
                "/tmp/center/code-review".to_string(),
                Some("unrelated-content".to_string()),
            ),
        ];

        let matched = find_best_center_match(&project, &all_managed).unwrap();

        assert_eq!(matched.id, "spaced-id");
    }

    /// The sidebar project count dedupes by logical skill rather than adding
    /// up per-agent copies.
    #[test]
    fn project_to_dto_counts_logical_skills_not_agent_copies() {
        let tmp = tempdir().unwrap();
        let project_path = tmp.path().join("project");
        let claude_skill = project_path.join(".claude/skills/shared-skill");
        let codex_skill = project_path.join(".codex/skills/shared-skill");
        fs::create_dir_all(&claude_skill).unwrap();
        fs::create_dir_all(&codex_skill).unwrap();
        fs::write(claude_skill.join("SKILL.md"), "# Shared\n").unwrap();
        fs::write(codex_skill.join("SKILL.md"), "# Shared\n").unwrap();

        let record = ProjectRecord {
            id: "project-1".to_string(),
            name: "Project".to_string(),
            path: project_path.to_string_lossy().to_string(),
            workspace_type: "project".to_string(),
            linked_agent_key: None,
            linked_agent_name: None,
            disabled_path: None,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        let configs = vec![
            AgentSkillConfig {
                key: "claude_code".to_string(),
                display_name: "Claude Code".to_string(),
                relative_skills_dir: ".claude/skills".to_string(),
            },
            AgentSkillConfig {
                key: "codex".to_string(),
                display_name: "Codex".to_string(),
                relative_skills_dir: ".codex/skills".to_string(),
            },
        ];

        let store = SkillStore::new(&tmp.path().join("status.db")).unwrap();
        let dto = project_to_dto(&store, &record, &[], &configs).unwrap();

        assert_eq!(dto.skill_count, 1);
        assert_eq!(dto.sync_health.project_only, 1);
    }

    #[test]
    fn classify_sync_status_uses_live_center_hash_when_db_hash_is_stale() {
        let center_dir = tempdir().unwrap();
        fs::write(center_dir.path().join("SKILL.md"), "# Example\n").unwrap();
        let live_hash = content_hash::hash_directory(center_dir.path()).unwrap();

        let managed = sample_managed_skill(
            center_dir.path().to_string_lossy().to_string(),
            Some("stale-db-hash".to_string()),
            1_000,
        );
        let project = sample_project_skill(
            center_dir.path().to_string_lossy().to_string(),
            Some(live_hash),
            Some(5_000),
        );

        assert_eq!(classify_sync_status(&project, Some(&managed)), "in_sync");
    }

    /// Newest content mtime of a directory, the same figure the project side
    /// is built from, so both sides of the comparison use one ruler.
    fn center_mtime_ms(dir: &std::path::Path) -> i64 {
        content_hash::latest_modified_ms(&content_hash::list_content_files(dir)).unwrap()
    }

    /// `updated_at` is a database column, not a filesystem mtime. With the
    /// project copy genuinely newer on disk, a much later `updated_at` must not
    /// flip the answer to "center_newer" — that reading invited a pull and
    /// overwrote the edit the user had just made (#328).
    #[test]
    fn classify_sync_status_ignores_the_db_column_when_the_project_is_newer_on_disk() {
        let center_dir = tempdir().unwrap();
        fs::write(center_dir.path().join("SKILL.md"), "# Center\n").unwrap();
        let center_mtime = center_mtime_ms(center_dir.path());

        let project_dir = tempdir().unwrap();
        fs::write(project_dir.path().join("SKILL.md"), "# Project changed\n").unwrap();
        let project_hash = content_hash::hash_directory(project_dir.path()).unwrap();

        let managed = sample_managed_skill(
            center_dir.path().to_string_lossy().to_string(),
            Some("stale-db-hash".to_string()),
            center_mtime + 60_000,
        );
        let project = sample_project_skill(
            project_dir.path().to_string_lossy().to_string(),
            Some(project_hash),
            Some(center_mtime + 5_000),
        );

        assert_eq!(
            classify_sync_status(&project, Some(&managed)),
            "project_newer"
        );
    }

    /// The other direction, and the reason the fix is not simply "always say
    /// project_newer": a center that really is ahead still reports so, with an
    /// `updated_at` old enough that only the real mtime can produce it.
    #[test]
    fn classify_sync_status_reports_a_center_that_is_newer_on_disk() {
        let center_dir = tempdir().unwrap();
        fs::write(center_dir.path().join("SKILL.md"), "# Center\n").unwrap();
        let center_mtime = center_mtime_ms(center_dir.path());

        let project_dir = tempdir().unwrap();
        fs::write(project_dir.path().join("SKILL.md"), "# Project older\n").unwrap();
        let project_hash = content_hash::hash_directory(project_dir.path()).unwrap();

        let managed = sample_managed_skill(
            center_dir.path().to_string_lossy().to_string(),
            Some("stale-db-hash".to_string()),
            0,
        );
        let project = sample_project_skill(
            project_dir.path().to_string_lossy().to_string(),
            Some(project_hash),
            Some(center_mtime - 5_000),
        );

        assert_eq!(
            classify_sync_status(&project, Some(&managed)),
            "center_newer"
        );
    }

    #[test]
    fn linked_workspace_roots_reject_same_directory() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("skills");
        fs::create_dir_all(&root).unwrap();

        let err = ensure_distinct_linked_workspace_roots(&root, &root).unwrap_err();
        assert!(
            err.to_string().contains("must not overlap"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn linked_workspace_roots_reject_nested_directory() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("skills");
        let nested = root.join("disabled");
        fs::create_dir_all(&nested).unwrap();

        let err = ensure_distinct_linked_workspace_roots(&root, &nested).unwrap_err();
        assert!(
            err.to_string().contains("must not overlap"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn linked_workspace_roots_allow_distinct_directories() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("skills");
        let disabled = tmp.path().join("skills-disabled");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&disabled).unwrap();

        ensure_distinct_linked_workspace_roots(&root, &disabled).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn remove_workspace_skill_target_removes_symlink_without_touching_target() {
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real-skill");
        let link = tmp.path().join("linked-skill");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("SKILL.md"), "# hello").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        remove_workspace_skill_target(&link).unwrap();

        assert!(!link.exists());
        assert!(real.exists());
        assert!(real.join("SKILL.md").exists());
    }

    #[cfg(windows)]
    #[test]
    fn remove_workspace_skill_target_removes_directory_symlink_without_touching_target() {
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real-skill");
        let link = tmp.path().join("linked-skill");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("SKILL.md"), "# hello").unwrap();
        std::os::windows::fs::symlink_dir(&real, &link).unwrap();

        remove_workspace_skill_target(&link).unwrap();

        assert!(!link.exists());
        assert!(real.exists());
        assert!(real.join("SKILL.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn set_project_skill_enabled_state_disabling_cleans_duplicate_symlink_without_touching_target()
    {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().unwrap();
        let central_skill = tmp.path().join("central").join("understand-diff");
        let skills_root = tmp.path().join("skills");
        let disabled_root = tmp.path().join("skills-disabled");
        let relative_path = "understand-diff";

        fs::create_dir_all(&central_skill).unwrap();
        fs::write(
            central_skill.join("SKILL.md"),
            "---\nname: understand-diff\n---\n",
        )
        .unwrap();
        fs::create_dir_all(&skills_root).unwrap();
        fs::create_dir_all(&disabled_root).unwrap();

        symlink(&central_skill, skills_root.join(relative_path)).unwrap();
        symlink(&central_skill, disabled_root.join(relative_path)).unwrap();

        assert!(set_project_skill_enabled_state(
            &skills_root,
            &disabled_root,
            relative_path,
            false,
        )
        .is_err());

        assert!(skills_root.join(relative_path).exists());
        assert!(disabled_root.join(relative_path).exists());
        assert!(central_skill.exists());
        assert!(central_skill.join("SKILL.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn set_project_skill_enabled_state_enabling_cleans_duplicate_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().unwrap();
        let central_skill = tmp.path().join("central").join("understand-diff");
        let skills_root = tmp.path().join("skills");
        let disabled_root = tmp.path().join("skills-disabled");
        let relative_path = "understand-diff";

        fs::create_dir_all(&central_skill).unwrap();
        fs::write(
            central_skill.join("SKILL.md"),
            "---\nname: understand-diff\n---\n",
        )
        .unwrap();
        fs::create_dir_all(&skills_root).unwrap();
        fs::create_dir_all(&disabled_root).unwrap();

        symlink(&central_skill, skills_root.join(relative_path)).unwrap();
        symlink(&central_skill, disabled_root.join(relative_path)).unwrap();

        assert!(
            set_project_skill_enabled_state(&skills_root, &disabled_root, relative_path, true,)
                .is_err()
        );

        assert!(skills_root.join(relative_path).exists());
        assert!(disabled_root.join(relative_path).exists());
        assert!(central_skill.exists());
        assert!(central_skill.join("SKILL.md").is_file());
    }

    #[test]
    fn set_project_skill_enabled_state_enabling_removes_emptied_disabled_dir() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let disabled_root = tmp.path().join("skills-disabled");
        let relative_path = "my-skill";

        let real_disabled = disabled_root.join(relative_path);
        fs::create_dir_all(&skills_root).unwrap();
        fs::create_dir_all(&real_disabled).unwrap();
        fs::write(real_disabled.join("SKILL.md"), "---\nname: my-skill\n---\n").unwrap();

        set_project_skill_enabled_state(&skills_root, &disabled_root, relative_path, true).unwrap();

        assert!(skills_root.join(relative_path).join("SKILL.md").is_file());
        assert!(!disabled_root.exists());
    }

    #[test]
    fn set_project_skill_enabled_state_enabling_keeps_disabled_dir_when_other_skills_remain() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let disabled_root = tmp.path().join("skills-disabled");
        let relative_path = "skill-a";

        let real_disabled_a = disabled_root.join(relative_path);
        let real_disabled_b = disabled_root.join("skill-b");
        fs::create_dir_all(&skills_root).unwrap();
        fs::create_dir_all(&real_disabled_a).unwrap();
        fs::create_dir_all(&real_disabled_b).unwrap();
        fs::write(
            real_disabled_a.join("SKILL.md"),
            "---\nname: skill-a\n---\n",
        )
        .unwrap();
        fs::write(
            real_disabled_b.join("SKILL.md"),
            "---\nname: skill-b\n---\n",
        )
        .unwrap();

        set_project_skill_enabled_state(&skills_root, &disabled_root, relative_path, true).unwrap();

        assert!(skills_root.join(relative_path).join("SKILL.md").is_file());
        assert!(disabled_root.is_dir());
        assert!(real_disabled_b.join("SKILL.md").is_file());
    }

    #[test]
    fn set_project_skill_enabled_state_enabling_removes_empty_nested_disabled_dirs() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let disabled_root = tmp.path().join("skills-disabled");
        let relative_path = "category/sub/skill-a";

        let real_disabled = disabled_root.join(relative_path);
        fs::create_dir_all(&skills_root).unwrap();
        fs::create_dir_all(&real_disabled).unwrap();
        fs::write(real_disabled.join("SKILL.md"), "---\nname: skill-a\n---\n").unwrap();

        set_project_skill_enabled_state(&skills_root, &disabled_root, relative_path, true).unwrap();

        assert!(skills_root.join(relative_path).join("SKILL.md").is_file());
        assert!(!disabled_root.exists());
    }

    #[test]
    fn set_project_skill_enabled_state_rejects_real_dir_duplicate_on_enable() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let disabled_root = tmp.path().join("skills-disabled");
        let relative_path = "my-skill";

        let real_enabled = skills_root.join(relative_path);
        let real_disabled = disabled_root.join(relative_path);
        fs::create_dir_all(&real_enabled).unwrap();
        fs::write(real_enabled.join("SKILL.md"), "---\nname: my-skill\n---\n").unwrap();
        fs::create_dir_all(&real_disabled).unwrap();
        fs::write(real_disabled.join("SKILL.md"), "---\nname: my-skill\n---\n").unwrap();

        let err =
            set_project_skill_enabled_state(&skills_root, &disabled_root, relative_path, true)
                .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        // Both real dirs must still exist
        assert!(real_enabled.join("SKILL.md").exists());
        assert!(real_disabled.join("SKILL.md").exists());
    }

    #[test]
    fn set_project_skill_enabled_state_rejects_real_dir_duplicate_on_disable() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let disabled_root = tmp.path().join("skills-disabled");
        let relative_path = "my-skill";

        let real_enabled = skills_root.join(relative_path);
        let real_disabled = disabled_root.join(relative_path);
        fs::create_dir_all(&real_enabled).unwrap();
        fs::write(real_enabled.join("SKILL.md"), "---\nname: my-skill\n---\n").unwrap();
        fs::create_dir_all(&real_disabled).unwrap();
        fs::write(real_disabled.join("SKILL.md"), "---\nname: my-skill\n---\n").unwrap();

        let err =
            set_project_skill_enabled_state(&skills_root, &disabled_root, relative_path, false)
                .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(real_enabled.join("SKILL.md").exists());
        assert!(real_disabled.join("SKILL.md").exists());
    }
}

#[cfg(test)]
mod add_project_tests {
    use super::add_project_internal;
    use crate::core::skill_store::SkillStore;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn add_project_registers_without_creating_agent_directories() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path().join("repo");
        fs::create_dir(&project_dir).unwrap();
        let store = SkillStore::new(&tmp.path().join("skills.db")).unwrap();

        let dto = add_project_internal(&store, project_dir.to_string_lossy().to_string()).unwrap();

        let canonical = dunce::canonicalize(&project_dir).unwrap();
        assert_eq!(dto.path, canonical.to_string_lossy());
        assert_eq!(dto.name, "repo");
        assert_eq!(fs::read_dir(&project_dir).unwrap().count(), 0);
        for agent_dir in [".claude", ".codex", ".agents"] {
            assert!(
                !project_dir.join(agent_dir).exists(),
                "{agent_dir} must be opt-in"
            );
        }
        assert_eq!(store.get_all_projects().unwrap().len(), 1);
    }

    #[test]
    fn add_and_remove_project_preserve_existing_agent_directories() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path().join("repo");
        let existing = project_dir.join(".claude/skills/custom/SKILL.md");
        fs::create_dir_all(existing.parent().unwrap()).unwrap();
        fs::write(&existing, "user-owned skill").unwrap();
        let store = SkillStore::new(&tmp.path().join("skills.db")).unwrap();

        let dto = add_project_internal(&store, project_dir.to_string_lossy().to_string()).unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "user-owned skill");
        assert!(!project_dir.join(".claude/skills-disabled").exists());
        assert!(!project_dir.join(".codex").exists());

        let duplicate = add_project_internal(&store, format!("{}/", project_dir.display()));
        assert!(duplicate
            .unwrap_err()
            .to_string()
            .contains("already registered"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), "user-owned skill");
        assert!(!project_dir.join(".claude/skills-disabled").exists());

        super::remove_project_internal(&store, dto.id).unwrap();
        assert!(store.get_all_projects().unwrap().is_empty());
        assert_eq!(fs::read_to_string(&existing).unwrap(), "user-owned skill");
        assert!(!project_dir.join(".claude/skills-disabled").exists());
    }

    #[test]
    fn add_project_rejects_a_missing_directory_without_side_effects() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("skills.db")).unwrap();
        let missing = tmp.path().join("missing");

        let err = add_project_internal(&store, missing.to_string_lossy().to_string()).unwrap_err();

        assert!(err.to_string().contains("Directory does not exist"));
        assert!(!missing.exists());
        assert!(store.get_all_projects().unwrap().is_empty());
    }

    /// The stored path must not keep Windows' verbatim prefix: `/` in a
    /// project-relative dir like `.agents/skills` is not a separator there.
    /// dunce strips it from drive paths; it keeps UNC shares verbatim, so
    /// those fall back to the caller's own absolute spelling.
    #[test]
    fn stored_project_paths_never_keep_a_verbatim_prefix_dunce_left() {
        use super::stored_project_path as stored;
        use std::path::Path;
        assert_eq!(
            stored(
                Path::new(r"\\?\UNC\server\share\repo"),
                Path::new(r"\\server\share\repo")
            ),
            Path::new(r"\\server\share\repo")
        );
        assert_eq!(
            stored(Path::new("/private/tmp/repo"), Path::new("/tmp/repo")),
            Path::new("/private/tmp/repo"),
            "an ordinary canonical path is stored canonical"
        );
    }

    /// dunce's own rules, which only exist on Windows: a drive path loses the
    /// prefix; a name Windows would reinterpret keeps it.
    #[cfg(windows)]
    #[test]
    fn dunce_simplifies_only_what_keeps_its_meaning() {
        use std::path::Path;
        assert_eq!(
            dunce::simplified(Path::new(r"\\?\C:\Users\me\repo")),
            Path::new(r"C:\Users\me\repo")
        );
        assert_eq!(
            dunce::simplified(Path::new(r"\\?\C:\repo.")),
            Path::new(r"\\?\C:\repo.")
        );
        assert_eq!(
            dunce::simplified(Path::new(r"\\?\C:\CON\x")),
            Path::new(r"\\?\C:\CON\x")
        );
        assert_eq!(
            dunce::simplified(Path::new(r"\\?\UNC\server\share\repo")),
            Path::new(r"\\?\UNC\server\share\repo")
        );
    }

    /// `/a/b`, `/a/b/` and `/a/x/../b` are one project. The raw-string UNIQUE
    /// column let the spellings through as separate rows.
    #[test]
    fn add_project_rejects_another_spelling_of_a_registered_path() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path().join("repo");
        fs::create_dir(&project_dir).unwrap();
        let store = SkillStore::new(&tmp.path().join("skills.db")).unwrap();
        let first =
            add_project_internal(&store, project_dir.to_string_lossy().to_string()).unwrap();

        for spelling in [
            format!("{}/", project_dir.to_string_lossy()),
            project_dir
                .join("..")
                .join("repo")
                .to_string_lossy()
                .to_string(),
        ] {
            let err = add_project_internal(&store, spelling.clone()).unwrap_err();
            assert!(
                err.to_string().contains("already registered"),
                "{spelling}: {err}"
            );
            assert!(err.to_string().contains(&first.id), "{spelling}: {err}");
        }
        assert_eq!(store.get_all_projects().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod project_plan_tests {
    use super::*;
    use crate::core::{central_repo, content_hash};
    use std::fs;
    use tempfile::{tempdir, TempDir};

    struct Fixture {
        _guard: std::sync::MutexGuard<'static, ()>,
        _tmp: TempDir,
        store: SkillStore,
        project_id: String,
        repo: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            central_repo::set_test_base_dir_override(None);
        }
    }

    /// A store with one registered project. Path overrides make the named
    /// agents count as installed on any machine.
    fn fixture(installed_agents: &[&str]) -> Fixture {
        let guard = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        central_repo::set_test_base_dir_override(Some(tmp.path().join("center")));
        let store = SkillStore::new(&tmp.path().join("skills.db")).unwrap();
        let overrides: serde_json::Map<String, serde_json::Value> = installed_agents
            .iter()
            .map(|key| {
                let dir = tmp.path().join("global").join(key);
                (
                    key.to_string(),
                    serde_json::Value::String(dir.to_string_lossy().to_string()),
                )
            })
            .collect();
        store
            .set_setting(
                "custom_tool_paths",
                &serde_json::Value::Object(overrides).to_string(),
            )
            .unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let project_id = add_project_internal(&store, repo.to_string_lossy().to_string())
            .unwrap()
            .id;
        let repo = dunce::canonicalize(&repo).unwrap();
        Fixture {
            _guard: guard,
            _tmp: tmp,
            store,
            project_id,
            repo,
        }
    }

    fn write_skill(dir: &Path, name: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\n{body}\n"),
        )
        .unwrap();
    }

    /// A library skill in the (test) central skills directory.
    fn library_skill(fx: &Fixture, id: &str, dir: &str, body: &str) -> SkillRecord {
        let central = central_repo::skills_dir().join(dir);
        write_skill(&central, dir, body);
        let hash = content_hash::hash_entries(&content_hash::list_content_files(&central));
        let record = SkillRecord {
            id: id.to_string(),
            name: dir.to_string(),
            description: None,
            source_type: "local".to_string(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: central.to_string_lossy().to_string(),
            content_hash: Some(hash),
            enabled: true,
            created_at: 0,
            updated_at: 0,
            status: "ok".to_string(),
            update_status: "local_only".to_string(),
            last_checked_at: None,
            last_check_error: None,
        };
        fx.store.insert_skill(&record).unwrap();
        record
    }

    /// Commit the fixture's central Skills with their stable IDs. Versioned
    /// Copy deploys deliberately fail closed when there is no Git commit.
    fn commit_library(fx: &Fixture) {
        let root = central_repo::skills_dir();
        fs::create_dir_all(&root).unwrap();
        let repo = git2::Repository::open(&root)
            .unwrap_or_else(|_| git2::Repository::init(&root).unwrap());
        for skill in fx.store.get_all_skills().unwrap() {
            let rel = Path::new(&skill.central_path)
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let file = root.join(format!(".skills-manager/skills/{}.json", skill.id));
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            let value = serde_json::json!({
                "schema_version": 1, "skill_id": skill.id,
                "path": rel, "path_key": rel, "enabled": true, "tags": [],
                "source": {"type": "local", "ref": null, "subpath": null, "branch": null}
            });
            fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let identity = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        let head = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = head.iter().collect();
        repo.commit(
            Some("HEAD"),
            &identity,
            &identity,
            "fixture",
            &tree,
            &parents,
        )
        .unwrap();
    }

    fn ids(skills: &[&SkillRecord]) -> Vec<String> {
        skills.iter().map(|s| s.id.clone()).collect()
    }

    /// Compared as paths, not strings: on Windows a conflict path mixes `\` with
    /// the `/` of an agent directory such as `.claude/skills`.
    fn conflict_paths(err: &AppError) -> Vec<PathBuf> {
        assert_eq!(
            err.kind,
            crate::core::error::ErrorKind::TargetConflict,
            "{err}"
        );
        err.details
            .as_ref()
            .unwrap()
            .conflicts
            .iter()
            .map(|c| PathBuf::from(&c.path))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn late_writer_using_old_file_handle_updates_retained_backup_not_new_copy() {
        use std::io::{Seek, SeekFrom, Write};
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "old");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let mut late_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(target.join("SKILL.md"))
            .unwrap();

        let central = Path::new(&skill.central_path).join("SKILL.md");
        fs::write(&central, "---\nname: demo\n---\nnew committed copy\n").unwrap();
        commit_library(&fx);
        update_managed_project_copy(&fx.store, &fx.project_id, &target, None).unwrap();
        late_writer.seek(SeekFrom::Start(0)).unwrap();
        late_writer.write_all(b"AFTER-RENAME-WRITE").unwrap();
        late_writer.flush().unwrap();

        assert!(fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("new committed copy"));
        let ops = fx.store.pending_project_skill_operations().unwrap();
        let retained = ops
            .iter()
            .find(|op| op.kind == "update" && op.phase == "cleanup_pending")
            .unwrap();
        let previous = Path::new(&retained.recovery_root).join("previous/SKILL.md");
        assert!(fs::read_to_string(&previous)
            .unwrap()
            .contains("AFTER-RENAME-WRITE"));
    }

    #[test]
    fn copy_delete_db_failure_restores_directory_and_ownership() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "old");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let original = fs::read_to_string(target.join("SKILL.md")).unwrap();
        let db = rusqlite::Connection::open(fx._tmp.path().join("skills.db")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER deny_delete BEFORE DELETE ON project_skill_deployments
             BEGIN SELECT RAISE(ABORT, 'injected delete failure'); END;",
        )
        .unwrap();
        let result = delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            original
        );
        assert!(fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
            .unwrap()
            .is_some());
        let ops = fx.store.pending_project_skill_operations().unwrap();
        assert!(ops
            .iter()
            .any(|op| op.kind == "delete" && op.phase == "recovery_available"));
    }

    #[test]
    fn recovery_journal_visibility_is_project_scoped_and_does_not_change_files() {
        let fx = fixture(&["claude_code"]);
        let other_path = fx._tmp.path().join("other-project");
        fs::create_dir_all(&other_path).unwrap();
        let other_id = add_project_internal(&fx.store, other_path.to_string_lossy().to_string())
            .unwrap()
            .id;
        let own_target = fx.repo.join(".claude/skills/demo");
        fs::create_dir_all(own_target.parent().unwrap()).unwrap();
        let own_id = uuid::Uuid::new_v4().to_string();
        let other_op_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let create = |id: String, project: String, target: PathBuf| {
            let recovery_root = target.parent().unwrap().join(".atm-recovery").join(&id);
            ProjectSkillOperationRecord {
                id,
                project_id: project,
                skill_id: "s-demo".to_string(),
                deployment_id: None,
                kind: "deploy".to_string(),
                phase: "preparing".to_string(),
                target_path: target.to_string_lossy().to_string(),
                recovery_root: recovery_root.to_string_lossy().to_string(),
                base_tree_oid: None,
                new_tree_oid: None,
                created_at: now,
                updated_at: now,
                destination_path: None,
                observed_identity: None,
            }
        };
        fx.store
            .insert_project_skill_operation(&create(
                own_id.clone(),
                fx.project_id.clone(),
                own_target.clone(),
            ))
            .unwrap();
        fx.store
            .insert_project_skill_operation(&create(
                other_op_id,
                other_id.clone(),
                other_path.join(".claude/skills/foreign"),
            ))
            .unwrap();
        let before = fx.store.pending_project_skill_operations().unwrap();
        let visible = get_project_skill_recoveries_internal(&fx.store, &fx.project_id).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].operation_id, own_id);
        assert_eq!(visible[0].phase, "preparing");
        assert!(!visible[0].target_exists);
        assert!(!visible[0].recovery_root_exists);
        assert!(get_project_skill_recoveries_internal(&fx.store, "unknown-project",).is_err());
        let after = fx.store.pending_project_skill_operations().unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before[0].id, after[0].id);
        assert_eq!(before[0].phase, after[0].phase);
        assert!(!own_target.exists());
        assert!(
            !own_target.parent().unwrap().join(".atm-recovery").exists(),
            "viewing recoveries must never create or clean directories"
        );
    }

    #[test]
    fn crash_recovery_restores_old_copy_when_update_was_interrupted_after_move() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let record = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap();
        let prior = fs::read_to_string(target.join("SKILL.md")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let recovery = target.parent().unwrap().join(".atm-recovery").join(&id);
        fs::create_dir(&recovery).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        fx.store
            .insert_project_skill_operation(&ProjectSkillOperationRecord {
                id: id.clone(),
                project_id: fx.project_id.clone(),
                skill_id: skill.id.clone(),
                deployment_id: Some(record.id.clone()),
                kind: "update".into(),
                phase: "preparing".into(),
                target_path: target.to_string_lossy().to_string(),
                recovery_root: recovery.to_string_lossy().to_string(),
                base_tree_oid: record.base_tree_oid.clone(),
                new_tree_oid: record.base_tree_oid.clone(),
                created_at: now,
                updated_at: now,

                destination_path: None,
                observed_identity: None,
            })
            .unwrap();
        fx.store
            .transition_project_skill_operation(&id, "preparing", "publishing")
            .unwrap();
        rename_directory_no_replace(&target, &recovery.join("previous")).unwrap();
        assert!(!target.exists());

        let reopened = SkillStore::new(&fx._tmp.path().join("skills.db")).unwrap();
        let notices = recover_incomplete_project_skill_operations(&reopened).unwrap();
        assert!(notices
            .iter()
            .any(|msg| msg.contains(&id) && msg.contains("Restored")));
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), prior);
        assert_eq!(
            reopened
                .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
                .unwrap()
                .unwrap()
                .base_tree_oid,
            record.base_tree_oid
        );
        assert!(reopened
            .pending_project_skill_operations()
            .unwrap()
            .iter()
            .any(|op| op.id == id && op.phase == "recovery_available"));
        // Running startup recovery again has no destructive side effects.
        let second = recover_incomplete_project_skill_operations(&reopened).unwrap();
        assert!(second.iter().any(|msg| msg.contains(&id)));
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), prior);
    }

    #[test]
    fn crash_after_copy_publish_keeps_intent_but_does_not_auto_claim_target() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let version =
            git_skill_versions::resolve_skill_version(&central_repo::skills_dir(), &skill.id, None)
                .unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let recovery = target
            .parent()
            .unwrap()
            .join(".atm-recovery/simulated-crash");
        fs::create_dir_all(&recovery).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        fx.store
            .insert_project_skill_operation(&ProjectSkillOperationRecord {
                id: id.clone(),
                project_id: fx.project_id.clone(),
                skill_id: skill.id.clone(),
                deployment_id: Some(uuid::Uuid::new_v4().to_string()),
                kind: "deploy".into(),
                phase: "preparing".into(),
                target_path: target.to_string_lossy().to_string(),
                recovery_root: recovery.to_string_lossy().to_string(),
                base_tree_oid: None,
                new_tree_oid: Some(version.tree_oid.clone()),
                created_at: now,
                updated_at: now,

                destination_path: None,
                observed_identity: None,
            })
            .unwrap();
        git_skill_versions::materialize_skill_version(
            &central_repo::skills_dir(),
            &version,
            &recovery.join("new"),
        )
        .unwrap();
        fx.store
            .transition_project_skill_operation(&id, "preparing", "publishing")
            .unwrap();
        rename_directory_no_replace(&recovery.join("new"), &target).unwrap();
        // Simulate a restarted database reader. Content equality and a
        // pending intent must NOT grant management automatically.
        let restarted = SkillStore::new(&fx._tmp.path().join("skills.db")).unwrap();
        assert!(restarted
            .pending_project_skill_operations()
            .unwrap()
            .iter()
            .any(|op| op.id == id && op.phase == "publishing"));
        assert!(restarted
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
            .unwrap()
            .is_none());
        let plan = plan_project_deploy(
            &restarted,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(plan[0].state, ProjectDeployState::Conflict);
        assert!(target.join("SKILL.md").is_file());
    }

    #[test]
    fn first_copy_deploy_database_failure_retains_recovery_and_does_not_grant_ownership() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        let db_path = fx._tmp.path().join("skills.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER deny_first_deploy BEFORE INSERT ON project_skill_deployments
             BEGIN SELECT RAISE(ABORT, 'injected deploy commit failure'); END;",
        )
        .unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let err = apply_project_deploy(&fx.store, &plan).unwrap_err();
        assert!(err.to_string().contains("database commit failed"), "{err}");
        assert!(
            !target.exists(),
            "unmanaged directory must not remain at target"
        );
        assert!(
            fx.store
                .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
                .unwrap()
                .is_none(),
            "a failed DB transaction grants no ownership"
        );
        let pending = fx.store.pending_project_skill_operations().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, "deploy");
        assert_eq!(pending[0].phase, "recovery_available");
        assert_eq!(pending[0].deployment_id.is_some(), true);
        let recovered = Path::new(&pending[0].recovery_root).join("recovered");
        assert!(recovered.join("SKILL.md").exists());
        conn.execute_batch("DROP TRIGGER deny_first_deploy")
            .unwrap();

        // Retry can install a fresh committed Copy without claiming the old
        // recovery data, which stays available for manual inspection.
        let retry = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &retry).unwrap();
        assert!(target.join("SKILL.md").is_file());
        assert!(recovered.join("SKILL.md").is_file());
        assert_eq!(
            fx.store
                .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
                .unwrap()
                .unwrap()
                .management_state,
            "managed"
        );
    }

    #[test]
    fn post_isolation_identity_check_rejects_replaced_target() {
        let tmp = tempdir().unwrap();
        let owned = tmp.path().join("owned");
        let foreign = tmp.path().join("foreign");
        let backup = tmp.path().join("isolated");
        fs::create_dir(&owned).unwrap();
        fs::create_dir(&foreign).unwrap();
        fs::write(owned.join("owner"), "original").unwrap();
        fs::write(foreign.join("user"), "external").unwrap();
        let approved = project_target_identity(&owned).unwrap();
        let parked = tmp.path().join("original-parked");
        rename_directory_no_replace(&owned, &parked).unwrap();
        rename_directory_no_replace(&foreign, &owned).unwrap();
        rename_directory_no_replace(&owned, &backup).unwrap();
        assert!(!is_same_isolated_target(&backup, approved));
        assert!(backup.join("user").is_file());
        assert!(parked.join("owner").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn toggle_rejects_recovery_directory_symlink() {
        let fx = fixture(&["claude_code"]);
        let source = fx.repo.join(".claude/skills/demo");
        write_skill(&source, "demo", "external");
        let elsewhere = fx._tmp.path().join("outside");
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, source.parent().unwrap().join(".atm-recovery"))
            .unwrap();
        assert!(toggle_project_skill_internal(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            false,
        )
        .is_err());
        assert!(source.join("SKILL.md").is_file());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn replaced_manager_symlink_to_same_central_path_is_not_owned() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let row = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap();
        assert!(managed_symlink_matches(&row, &target));
        let parked = fx.repo.join("previous-manager-link");
        rename_directory_no_replace(&target, &parked).unwrap();
        std::os::unix::fs::symlink(&demo.central_path, &target).unwrap();
        assert!(!managed_symlink_matches(&row, &target));
        let plan = plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(!plan[0].safe);
        assert!(delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .is_err());
        assert!(std::fs::symlink_metadata(&target).is_ok());
        assert!(std::fs::symlink_metadata(&parked).is_ok());
        assert!(Path::new(&demo.central_path).join("SKILL.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_registration_database_failure_never_deletes_foreign_target() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let conn = rusqlite::Connection::open(fx._tmp.path().join("skills.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_managed_link
             BEFORE INSERT ON project_skill_deployments
             BEGIN SELECT RAISE(ABORT, 'injected registration failure'); END;",
        )
        .unwrap();
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert!(apply_project_deploy(&fx.store, &plan).is_err());
        let target = fx.repo.join(".claude/skills/demo");
        assert!(
            std::fs::symlink_metadata(&target).is_err(),
            "manager-created link rolls back on failed ownership registration"
        );
        assert!(
            Path::new(&demo.central_path).join("SKILL.md").is_file(),
            "never delete the central Skill target"
        );
        assert!(fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
            .unwrap()
            .is_none());
    }

    #[test]
    fn managed_copy_toggle_database_failure_restores_original_and_preserves_deployment() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let enabled = fx.repo.join(".claude/skills/demo");
        let disabled = fx.repo.join(".claude/skills-disabled/demo");
        let before = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &enabled.to_string_lossy())
            .unwrap()
            .unwrap();
        let conn = rusqlite::Connection::open(fx._tmp.path().join("skills.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_toggle BEFORE UPDATE OF target_path
             ON project_skill_deployments
             BEGIN SELECT RAISE(ABORT, 'injected toggle commit failure'); END;",
        )
        .unwrap();
        assert!(toggle_project_skill_internal(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            false,
        )
        .is_err());
        assert!(enabled.join("SKILL.md").is_file());
        assert!(std::fs::symlink_metadata(&disabled).is_err());
        assert!(fx
            .store
            .get_project_skill_deployment(&fx.project_id, &disabled.to_string_lossy(),)
            .unwrap()
            .is_none());
        assert_eq!(
            fx.store
                .get_project_skill_deployment(&fx.project_id, &enabled.to_string_lossy(),)
                .unwrap()
                .unwrap()
                .id,
            before.id
        );
        let ops = fx.store.pending_project_skill_operations().unwrap();
        assert!(ops
            .iter()
            .any(|op| op.kind == "toggle" && op.phase == "recovery_available"));

        conn.execute_batch("DROP TRIGGER refuse_toggle").unwrap();
        toggle_project_skill_internal(&fx.store, &fx.project_id, "demo", "claude_code", false)
            .unwrap();
        assert!(!enabled.exists());
        assert!(disabled.join("SKILL.md").exists());
        let updated = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &disabled.to_string_lossy())
            .unwrap()
            .unwrap();
        assert_eq!(updated.id, before.id);
        assert_eq!(updated.base_tree_oid, before.base_tree_oid);
        toggle_project_skill_internal(&fx.store, &fx.project_id, "demo", "claude_code", true)
            .unwrap();
        assert!(enabled.join("SKILL.md").exists());
        assert!(fx
            .store
            .get_project_skill_deployment(&fx.project_id, &enabled.to_string_lossy(),)
            .unwrap()
            .is_some());
    }

    /// Run by a separate Rust test subprocess. This worker performs a real
    /// filesystem rename, records that it crossed the crash window, then
    /// intentionally stays alive until the parent SIGKILLs it. Running without
    /// the sentinel environment variable is a no-op.
    #[cfg(unix)]
    #[test]
    fn subprocess_rename_crash_window_worker() {
        if std::env::var_os("ATM_ISSUE11_CRASH_TEST").is_none() {
            return;
        }
        let source = PathBuf::from(std::env::var_os("ATM_CRASH_SOURCE").unwrap());
        let destination = PathBuf::from(std::env::var_os("ATM_CRASH_DEST").unwrap());
        let marker = PathBuf::from(std::env::var_os("ATM_CRASH_MARKER").unwrap());
        rename_directory_no_replace(&source, &destination).unwrap();
        fs::write(&marker, "rename complete, waiting for SIGKILL").unwrap();
        std::thread::park();
        panic!("subprocess must be terminated by SIGKILL");
    }

    #[cfg(unix)]
    fn kill_child_after_rename(source: &Path, destination: &Path, tmp: &Path) {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let marker = tmp.join("child-renamed");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("commands::projects::project_plan_tests::subprocess_rename_crash_window_worker")
            .arg("--nocapture")
            .env("ATM_ISSUE11_CRASH_TEST", "1")
            .env("ATM_CRASH_SOURCE", source)
            .env("ATM_CRASH_DEST", destination)
            .env("ATM_CRASH_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        let reached = loop {
            if marker.is_file() {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            if child.try_wait().unwrap().is_some() {
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Always reap the spawned process, even if setup unexpectedly failed.
        child
            .kill()
            .expect("SIGKILL child still running after rename");
        let exit = child.wait().unwrap();
        assert!(reached, "child did not reach the publishing crash window");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            exit.signal(),
            Some(9),
            "child must be terminated by actual SIGKILL"
        );
        assert!(std::fs::symlink_metadata(source).is_err());
        assert!(std::fs::symlink_metadata(destination).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn real_sigkill_interrupted_toggle_recovers_without_adopting_external_skill() {
        let fx = fixture(&["claude_code"]);
        let source = fx.repo.join(".claude/skills/demo");
        let destination = fx.repo.join(".claude/skills-disabled/demo");
        write_skill(&source, "demo", "external data before crash");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let identity = project_target_identity_string(&source).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let recovery = source.parent().unwrap().join(".atm-recovery").join(&id);
        fs::create_dir_all(&recovery).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        fx.store
            .insert_project_skill_operation(&ProjectSkillOperationRecord {
                id: id.clone(),
                project_id: fx.project_id.clone(),
                skill_id: "external-unmanaged".to_string(),
                deployment_id: None,
                kind: "toggle".to_string(),
                phase: "preparing".to_string(),
                target_path: source.to_string_lossy().to_string(),
                recovery_root: recovery.to_string_lossy().to_string(),
                base_tree_oid: None,
                new_tree_oid: None,
                created_at: now,
                updated_at: now,
                destination_path: Some(destination.to_string_lossy().to_string()),
                observed_identity: Some(identity),
            })
            .unwrap();
        fx.store
            .transition_project_skill_operation(&id, "preparing", "publishing")
            .unwrap();

        kill_child_after_rename(&source, &destination, fx._tmp.path());
        let restarted = SkillStore::new(&fx._tmp.path().join("skills.db")).unwrap();
        let notices = recover_incomplete_project_skill_operations(&restarted).unwrap();
        assert!(notices
            .iter()
            .any(|notice| notice.contains(&id) && notice.contains("Restored")));
        assert!(fs::read_to_string(source.join("SKILL.md"))
            .unwrap()
            .contains("external data before crash"));
        assert!(!destination.exists());
        assert!(
            restarted
                .get_project_skill_deployment(&fx.project_id, &source.to_string_lossy(),)
                .unwrap()
                .is_none(),
            "external Copy must not become managed"
        );
        let ops = restarted.pending_project_skill_operations().unwrap();
        assert!(ops
            .iter()
            .any(|op| op.id == id && op.phase == "recovery_available"));
        recover_incomplete_project_skill_operations(&restarted).unwrap();
        assert!(source.join("SKILL.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn real_sigkill_recovery_never_overwrites_new_external_occupant() {
        let fx = fixture(&["claude_code"]);
        let target = fx.repo.join(".claude/skills/demo");
        write_skill(&target, "demo", "original to preserve");
        let id = uuid::Uuid::new_v4().to_string();
        let root = target.parent().unwrap().join(".atm-recovery").join(&id);
        fs::create_dir_all(&root).unwrap();
        let previous = root.join("previous");
        let now = chrono::Utc::now().timestamp_millis();
        fx.store
            .insert_project_skill_operation(&ProjectSkillOperationRecord {
                id: id.clone(),
                project_id: fx.project_id.clone(),
                skill_id: "external-unmanaged".to_string(),
                deployment_id: None,
                kind: "delete".to_string(),
                phase: "preparing".to_string(),
                target_path: target.to_string_lossy().to_string(),
                recovery_root: root.to_string_lossy().to_string(),
                base_tree_oid: None,
                new_tree_oid: None,
                created_at: now,
                updated_at: now,
                destination_path: None,
                observed_identity: None,
            })
            .unwrap();
        fx.store
            .transition_project_skill_operation(&id, "preparing", "publishing")
            .unwrap();
        kill_child_after_rename(&target, &previous, fx._tmp.path());
        // A different process creates a directory at the original path while
        // the publisher is dead. Recovery must keep BOTH copies intact.
        write_skill(&target, "demo", "new user's independently owned contents");
        let reopened = SkillStore::new(&fx._tmp.path().join("skills.db")).unwrap();
        let notices = recover_incomplete_project_skill_operations(&reopened).unwrap();
        assert!(
            notices
                .iter()
                .any(|msg| msg.contains(&id) && msg.contains("Incomplete")),
            "{notices:?}"
        );
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("independently owned"));
        assert!(fs::read_to_string(previous.join("SKILL.md"))
            .unwrap()
            .contains("original to preserve"));
        let op = reopened
            .pending_project_skill_operations()
            .unwrap()
            .into_iter()
            .find(|op| op.id == id)
            .unwrap();
        assert_eq!(op.phase, "publishing");
        assert!(reopened
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn real_sigkill_interrupted_delete_restores_managed_copy_and_db_record() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "managed original before crash");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let before = fs::read(target.join("SKILL.md")).unwrap();
        let old_row = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let recovery_root = target.parent().unwrap().join(".atm-recovery").join(&id);
        fs::create_dir_all(&recovery_root).unwrap();
        let previous = recovery_root.join("previous");
        let now = chrono::Utc::now().timestamp_millis();
        fx.store
            .insert_project_skill_operation(&ProjectSkillOperationRecord {
                id: id.clone(),
                project_id: fx.project_id.clone(),
                skill_id: demo.id.clone(),
                deployment_id: Some(old_row.id.clone()),
                kind: "delete".to_string(),
                phase: "preparing".to_string(),
                target_path: target.to_string_lossy().to_string(),
                recovery_root: recovery_root.to_string_lossy().to_string(),
                base_tree_oid: old_row.base_tree_oid.clone(),
                new_tree_oid: None,
                created_at: now,
                updated_at: now,
                destination_path: None,
                observed_identity: None,
            })
            .unwrap();
        fx.store
            .transition_project_skill_operation(&id, "preparing", "publishing")
            .unwrap();

        kill_child_after_rename(&target, &previous, fx._tmp.path());
        let restarted = SkillStore::new(&fx._tmp.path().join("skills.db")).unwrap();
        let notices = recover_incomplete_project_skill_operations(&restarted).unwrap();
        assert!(notices
            .iter()
            .any(|notice| notice.contains(&id) && notice.contains("Restored")));
        assert_eq!(fs::read(target.join("SKILL.md")).unwrap(), before);
        assert!(!previous.exists());
        assert_eq!(
            restarted
                .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
                .unwrap()
                .unwrap()
                .id,
            old_row.id
        );
        recover_incomplete_project_skill_operations(&restarted).unwrap();
        assert_eq!(fs::read(target.join("SKILL.md")).unwrap(), before);
    }

    #[test]
    fn interrupted_toggle_recovery_restores_original_without_claiming_management() {
        let fx = fixture(&["claude_code"]);
        let source = fx.repo.join(".claude/skills/demo");
        let destination = fx.repo.join(".claude/skills-disabled/demo");
        write_skill(&source, "demo", "external skill");
        let identity = project_target_identity_string(&source).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let recovery = source.parent().unwrap().join(".atm-recovery").join(&id);
        std::fs::create_dir_all(&recovery).unwrap();
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        fx.store
            .insert_project_skill_operation(&ProjectSkillOperationRecord {
                id: id.clone(),
                project_id: fx.project_id.clone(),
                skill_id: "external-unmanaged".to_string(),
                deployment_id: None,
                kind: "toggle".to_string(),
                phase: "preparing".to_string(),
                target_path: source.to_string_lossy().to_string(),
                recovery_root: recovery.to_string_lossy().to_string(),
                base_tree_oid: None,
                new_tree_oid: None,
                created_at: now,
                updated_at: now,
                destination_path: Some(destination.to_string_lossy().to_string()),
                observed_identity: Some(identity),
            })
            .unwrap();
        fx.store
            .transition_project_skill_operation(&id, "preparing", "publishing")
            .unwrap();
        rename_directory_no_replace(&source, &destination).unwrap();
        assert!(!source.exists());
        let restarted = SkillStore::new(&fx._tmp.path().join("skills.db")).unwrap();
        let messages = recover_incomplete_project_skill_operations(&restarted).unwrap();
        assert!(messages
            .iter()
            .any(|msg| msg.contains(&id) && msg.contains("Restored")));
        assert!(source.join("SKILL.md").is_file());
        assert!(!destination.exists());
        assert!(
            restarted
                .get_project_skill_deployment(&fx.project_id, &source.to_string_lossy(),)
                .unwrap()
                .is_none(),
            "moving an external folder must not grant ownership"
        );
        // Idempotent on a second scan.
        recover_incomplete_project_skill_operations(&restarted).unwrap();
        assert!(source.join("SKILL.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn unmanaged_symlink_is_never_deleted_automatically_by_toggle_or_remove() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let enabled = fx.repo.join(".claude/skills/demo");
        let disabled = fx.repo.join(".claude/skills-disabled/demo");
        fs::create_dir_all(enabled.parent().unwrap()).unwrap();
        fs::create_dir_all(disabled.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&demo.central_path, &enabled).unwrap();
        std::os::unix::fs::symlink(&demo.central_path, &disabled).unwrap();
        assert!(toggle_project_skill_internal(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            true,
        )
        .is_err());
        assert!(std::fs::symlink_metadata(&enabled).is_ok());
        assert!(std::fs::symlink_metadata(&disabled).is_ok());
        assert!(delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .is_err());
        assert!(std::fs::symlink_metadata(&enabled).is_ok());
        assert!(std::fs::symlink_metadata(&disabled).is_ok());
    }

    #[test]
    fn git_copy_deploys_committed_bytes_and_tracks_exact_version() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "committed");
        commit_library(&fx);
        let selected =
            git_skill_versions::resolve_skill_version(&central_repo::skills_dir(), &demo.id, None)
                .unwrap();
        fs::write(
            Path::new(&demo.central_path).join("SKILL.md"),
            "uncommitted",
        )
        .unwrap();
        let plan = plan_project_deploy_with_commit(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
            Some(&selected.commit_oid),
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("committed"));
        assert!(!fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("uncommitted"));
        let record = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap();
        assert_eq!(record.skill_id, demo.id);
        assert_eq!(
            record.base_commit_oid.as_deref(),
            Some(selected.commit_oid.as_str())
        );
        assert_eq!(
            record.base_tree_oid.as_deref(),
            Some(selected.tree_oid.as_str())
        );
        assert_eq!(record.management_origin, "deployed_by_manager");
        assert_eq!(record.management_state, "managed");
        git_skill_versions::verify_deployment_pin(
            &central_repo::skills_dir(),
            &record.id,
            record.base_commit_oid.as_deref().unwrap(),
        )
        .unwrap();

        let again = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(again[0].state, ProjectDeployState::AlreadyDeployed);
        assert_eq!(again[0].sync_status.as_deref(), Some("in_sync"));
    }

    #[test]
    fn same_skill_tree_update_pins_each_distinct_commit() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let path = fx.repo.join(".claude/skills/demo");
        let before = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &path.to_string_lossy())
            .unwrap()
            .unwrap();

        fs::write(
            central_repo::skills_dir().join("README.md"),
            "unrelated commit",
        )
        .unwrap();
        commit_library(&fx);
        let new_head =
            git_skill_versions::resolve_skill_version(&central_repo::skills_dir(), &skill.id, None)
                .unwrap();
        assert_ne!(
            before.base_commit_oid.as_deref(),
            Some(new_head.commit_oid.as_str())
        );
        assert_eq!(
            before.base_tree_oid.as_deref(),
            Some(new_head.tree_oid.as_str())
        );
        update_managed_project_copy(&fx.store, &fx.project_id, &path, None).unwrap();

        let after = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &path.to_string_lossy())
            .unwrap()
            .unwrap();
        assert_eq!(after.id, before.id);
        assert_eq!(
            after.base_commit_oid.as_deref(),
            Some(new_head.commit_oid.as_str())
        );
        assert_eq!(after.base_tree_oid, before.base_tree_oid);
        git_skill_versions::verify_deployment_pin(
            &central_repo::skills_dir(),
            &before.id,
            before.base_commit_oid.as_deref().unwrap(),
        )
        .unwrap();
        git_skill_versions::verify_deployment_pin(
            &central_repo::skills_dir(),
            &after.id,
            &new_head.commit_oid,
        )
        .unwrap();
    }

    #[test]
    fn project_list_health_and_detail_use_same_authoritative_copy_status() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "VERSION ONE");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let center = Path::new(&demo.central_path).join("SKILL.md");

        let check = |expected: &str| {
            let details = get_project_skills_internal(&fx.store, fx.project_id.clone()).unwrap();
            let detail = details.iter().find(|s| s.relative_path == "demo").unwrap();
            assert_eq!(detail.sync_status, expected, "detail sync status");
            let overview = get_projects_internal(&fx.store)
                .unwrap()
                .into_iter()
                .find(|p| p.id == fx.project_id)
                .unwrap();
            assert_eq!(overview.skill_count, 1);
            let health = overview.sync_health;
            let count = match expected {
                "in_sync" => health.in_sync,
                "center_newer" => health.center_newer,
                "project_newer" => health.project_newer,
                "diverged" => health.diverged,
                other => panic!("unexpected status {other}"),
            };
            assert_eq!(count, 1, "overview disagrees with detail: {expected}");
            assert_eq!(
                health.in_sync
                    + health.center_newer
                    + health.project_newer
                    + health.diverged
                    + health.project_only,
                1,
            );
        };
        check("in_sync");

        // Changing only central working-tree bytes does not change the
        // committed HEAD baseline. Neither view may say "center newer".
        fs::write(&center, "UNCOMMITTED CENTRAL MODIFICATION").unwrap();
        check("in_sync");
        commit_library(&fx);
        check("center_newer");

        fs::write(target.join("SKILL.md"), "LOCAL PROJECT CHANGE").unwrap();
        check("diverged");

        // HEAD returns to the *same committed Skill Tree* (new CommitId).
        // A locally modified project is newer than its trusted Base, but
        // the central Git tree now matches Base again.
        write_skill(Path::new(&demo.central_path), "demo", "VERSION ONE");
        commit_library(&fx);
        check("project_newer");
    }

    #[test]
    fn stale_cli_update_preview_cannot_mutate_an_advanced_base_commit() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "FIRST");
        commit_library(&fx);
        let targets = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &targets).unwrap();
        let preview = plan_managed_project_copy_update(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            None,
        )
        .unwrap();
        assert!(!preview.will_change_content);
        let target = Path::new(&preview.target_path);
        let before_bytes = fs::read(target.join("SKILL.md")).unwrap();

        // Another authorized operation advances just the CommitId, without
        // changing the Skill subtree (unrelated repository commit).
        fs::write(
            central_repo::skills_dir().join("unrelated.txt"),
            "unrelated",
        )
        .unwrap();
        commit_library(&fx);
        update_managed_project_copy(&fx.store, &fx.project_id, target, None).unwrap();
        let advanced = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &preview.target_path)
            .unwrap()
            .unwrap();
        assert_ne!(
            advanced.base_commit_oid.as_deref(),
            Some(preview.base_commit_oid.as_str())
        );
        assert_eq!(
            advanced.base_tree_oid.as_deref(),
            Some(preview.base_tree_oid.as_str())
        );

        let error = update_managed_project_copy_checked(
            &fx.store,
            &fx.project_id,
            target,
            Some(&preview.target_commit_oid),
            Some((
                &preview.deployment_id,
                &preview.base_tree_oid,
                &preview.base_commit_oid,
            )),
        )
        .unwrap_err();
        assert!(error.to_string().contains("baseline changed"));
        assert_eq!(fs::read(target.join("SKILL.md")).unwrap(), before_bytes);
        let after = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &preview.target_path)
            .unwrap()
            .unwrap();
        assert_eq!(after.base_commit_oid, advanced.base_commit_oid);
    }

    #[test]
    fn gui_copy_preview_applies_only_selected_historical_git_commit() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "V1");
        commit_library(&fx);
        let first =
            git_skill_versions::resolve_skill_version(&central_repo::skills_dir(), &skill.id, None)
                .unwrap();
        let deploy = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &deploy).unwrap();

        fs::write(
            Path::new(&skill.central_path).join("SKILL.md"),
            "---\nname: demo\n---\nV2 committed",
        )
        .unwrap();
        commit_library(&fx);
        fs::write(
            Path::new(&skill.central_path).join("SKILL.md"),
            "---\nname: demo\n---\nUNCOMMITTED CENTRAL EDIT",
        )
        .unwrap();

        let preview = plan_managed_project_copy_update(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            None,
        )
        .unwrap();
        assert!(preview.will_change_content);
        assert!(preview.will_change_version);
        assert!(preview.uncommitted_central_changes_excluded);
        assert_eq!(preview.base_commit_oid, first.commit_oid);
        assert_eq!(preview.skill_relative_path, "demo");

        let target = Path::new(&preview.target_path);
        let original = fs::read_to_string(target.join("SKILL.md")).unwrap();
        assert!(original.contains("V1"));
        assert!(
            apply_managed_project_copy_update_plan(
                &fx.store,
                &ManagedCopyUpdatePlan {
                    target_path: fx
                        .repo
                        .join(".claude/skills/foreign")
                        .to_string_lossy()
                        .to_string(),
                    ..preview.clone()
                }
            )
            .is_err(),
            "client-supplied path cannot redirect a checked update"
        );
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            original
        );

        apply_managed_project_copy_update_plan(&fx.store, &preview).unwrap();
        let new_bytes = fs::read_to_string(target.join("SKILL.md")).unwrap();
        assert!(new_bytes.contains("V2 committed"), "{new_bytes}");
        assert!(!new_bytes.contains("UNCOMMITTED"));
        let committed = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &preview.target_path)
            .unwrap()
            .unwrap();
        assert_eq!(committed.id, preview.deployment_id);
        assert_eq!(
            committed.base_commit_oid.as_deref(),
            Some(preview.target_commit_oid.as_str())
        );
        assert_eq!(
            committed.base_tree_oid.as_deref(),
            Some(preview.target_tree_oid.as_str())
        );
        git_skill_versions::verify_deployment_pin(
            &central_repo::skills_dir(),
            &committed.id,
            &preview.target_commit_oid,
        )
        .unwrap();
        assert!(fx
            .store
            .pending_project_skill_operations()
            .unwrap()
            .iter()
            .any(|op| op.kind == "update"
                && op.phase == "cleanup_pending"
                && Path::new(&op.recovery_root)
                    .join("previous/SKILL.md")
                    .is_file()));
    }

    #[test]
    fn gui_copy_preview_refuses_stale_baseline_and_local_project_edits() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "V1");
        commit_library(&fx);
        let deploy = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &deploy).unwrap();
        fs::write(Path::new(&skill.central_path).join("SKILL.md"), "V2").unwrap();
        commit_library(&fx);

        let preview = plan_managed_project_copy_update(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            None,
        )
        .unwrap();
        // Another legitimately authorized update advances the baseline
        // between the GUI's preview and its "Apply" button.
        update_managed_project_copy(
            &fx.store,
            &fx.project_id,
            Path::new(&preview.target_path),
            None,
        )
        .unwrap();
        let err = apply_managed_project_copy_update_plan(&fx.store, &preview).unwrap_err();
        assert!(err.to_string().contains("stale"));

        let target = Path::new(&preview.target_path);
        fs::write(target.join("SKILL.md"), "PROJECT USER EDIT").unwrap();
        let err = plan_managed_project_copy_update(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("local edits"));
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "PROJECT USER EDIT"
        );
    }

    #[test]
    fn gui_copy_preview_refuses_unmanaged_copy_and_symlink() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let skill = library_skill(&fx, "s-demo", "demo", "V1");
        commit_library(&fx);
        let target = fx.repo.join(".claude/skills/demo");
        write_skill(&target, "demo", "UNMANAGED EXTERNAL");
        assert!(plan_managed_project_copy_update(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("not managed"));
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("UNMANAGED EXTERNAL"));

        fs::remove_dir_all(&target).unwrap();
        fx.store.set_setting("sync_mode", "symlink").unwrap();
        let deploy = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skill]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &deploy).unwrap();
        let err = plan_managed_project_copy_update(
            &fx.store,
            &fx.project_id,
            "demo",
            "claude_code",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Only an explicitly managed Copy"));
        assert!(std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn previous_managed_copy_versions_can_be_backfilled_without_changing_head() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let path = fx.repo.join(".claude/skills/demo");
        let row = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &path.to_string_lossy())
            .unwrap()
            .unwrap();
        let oid = row.base_commit_oid.unwrap();
        let root = central_repo::skills_dir();
        let git = git2::Repository::open(&root).unwrap();
        let name = git_skill_versions::deployment_pin_ref(&row.id, &oid).unwrap();
        git.find_reference(&name).unwrap().delete().unwrap();
        assert!(git_skill_versions::verify_deployment_pin(&root, &row.id, &oid).is_err());
        let head = git.head().unwrap().peel_to_commit().unwrap().id();
        let problems = git_skill_versions::reconcile_deployment_pins(&root, &fx.store).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        git_skill_versions::verify_deployment_pin(&root, &row.id, &oid).unwrap();
        assert_eq!(git.head().unwrap().peel_to_commit().unwrap().id(), head);

        // A bogus historical CommitId is never replaced with HEAD.
        let db = rusqlite::Connection::open(fx._tmp.path().join("skills.db")).unwrap();
        db.execute(
            "UPDATE project_skill_deployments SET base_commit_oid=?2 WHERE id=?1",
            rusqlite::params![row.id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        )
        .unwrap();
        let issues = git_skill_versions::reconcile_deployment_pins(&root, &fx.store).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("cannot retain"));
        assert_eq!(git.head().unwrap().peel_to_commit().unwrap().id(), head);

        // Even if the Git commit exists, a forged/mismatched stored subtree
        // must not be promoted to a trusted, newly pinned baseline.
        git.find_reference(&name).unwrap().delete().unwrap();
        db.execute(
            "UPDATE project_skill_deployments
             SET base_commit_oid=?2, base_tree_oid=?3 WHERE id=?1",
            rusqlite::params![row.id, oid, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",],
        )
        .unwrap();
        let mismatches = git_skill_versions::reconcile_deployment_pins(&root, &fx.store).unwrap();
        assert_eq!(mismatches.len(), 1);
        assert!(mismatches[0].contains("do not match"));
        assert!(git.find_reference(&name).is_err());
    }

    #[test]
    fn project_copy_update_uses_trusted_tree_not_mtime_and_preserves_local_edits() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let original = fs::read_to_string(target.join("SKILL.md")).unwrap();
        let base = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap()
            .base_tree_oid
            .unwrap();

        let central = Path::new(&demo.central_path).join("SKILL.md");
        fs::write(&central, "not yet committed").unwrap();
        update_managed_project_copy(&fx.store, &fx.project_id, &target, None).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            original
        );
        fs::write(&central, "---\nname: demo\ndescription: test\n---\nv2\n").unwrap();
        commit_library(&fx);
        update_managed_project_copy(&fx.store, &fx.project_id, &target, None).unwrap();
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("v2"));
        assert_ne!(
            fx.store
                .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy(),)
                .unwrap()
                .unwrap()
                .base_tree_oid
                .as_deref(),
            Some(base.as_str())
        );

        fs::write(target.join(".custom"), "my local edit").unwrap();
        fs::write(&central, "---\nname: demo\ndescription: test\n---\nv3\n").unwrap();
        commit_library(&fx);
        let err =
            update_managed_project_copy(&fx.store, &fx.project_id, &target, None).unwrap_err();
        assert!(err.to_string().contains("local changes"));
        assert!(target.join(".custom").is_file());
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("v2"));
        let status = get_project_skills_internal(&fx.store, fx.project_id.clone()).unwrap();
        assert!(status
            .iter()
            .any(|v| v.relative_path == "demo" && v.sync_status == "diverged"));
    }

    #[test]
    fn update_database_failure_rolls_back_the_original_copy_and_baseline() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let target = fx.repo.join(".claude/skills/demo");
        let before = fs::read_to_string(target.join("SKILL.md")).unwrap();
        let base = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap()
            .base_tree_oid;

        fs::write(
            Path::new(&demo.central_path).join("SKILL.md"),
            "---\\nname: demo\\ndescription: test\\n---\\nv2\\n",
        )
        .unwrap();
        commit_library(&fx);
        let conn = rusqlite::Connection::open(fx._tmp.path().join("skills.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_base_update BEFORE UPDATE OF base_tree_oid
             ON project_skill_deployments BEGIN
               SELECT RAISE(ABORT, 'injected database failure');
             END;",
        )
        .unwrap();
        assert!(update_managed_project_copy(&fx.store, &fx.project_id, &target, None).is_err());
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), before);
        let after = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap();
        assert_eq!(after.base_tree_oid, base);
    }

    #[test]
    fn publish_no_replace_never_overwrites_an_existing_directory() {
        let root = tempdir().unwrap();
        let staged = root.path().join("staged");
        let occupied = root.path().join("occupied");
        fs::create_dir(&staged).unwrap();
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("mine"), "original").unwrap();
        assert!(rename_directory_no_replace(&staged, &occupied).is_err());
        assert_eq!(
            fs::read_to_string(occupied.join("mine")).unwrap(),
            "original"
        );
        assert!(staged.exists());
    }

    #[test]
    fn matching_unmanaged_copy_is_not_automatically_adopted_or_updated() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "v1");
        commit_library(&fx);
        let external = fx.repo.join(".claude/skills/demo");
        write_skill(&external, "demo", "v1");
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(plan[0].state, ProjectDeployState::Conflict);
        assert!(update_managed_project_copy(&fx.store, &fx.project_id, &external, None).is_err());
        assert!(external.join("SKILL.md").exists());
    }

    #[test]
    fn gui_explicit_historical_commit_uses_exact_committed_bytes_not_dirty_center() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "FIRST COMMITTED");
        commit_library(&fx);
        let older =
            git_skill_versions::resolve_skill_version(&central_repo::skills_dir(), &demo.id, None)
                .unwrap();
        fs::write(
            Path::new(&demo.central_path).join("SKILL.md"),
            "# SECOND COMMITTED",
        )
        .unwrap();
        commit_library(&fx);
        let head =
            git_skill_versions::resolve_skill_version(&central_repo::skills_dir(), &demo.id, None)
                .unwrap();
        assert_ne!(older.commit_oid, head.commit_oid);
        fs::write(
            Path::new(&demo.central_path).join("SKILL.md"),
            "# DIRTY UNCOMMITTED",
        )
        .unwrap();

        let history =
            git_skill_versions::list_skill_history(&central_repo::skills_dir(), &demo.id, 25)
                .unwrap();
        assert!(history.uncommitted_central_changes_excluded);
        assert!(history
            .versions
            .iter()
            .any(|v| v.commit_oid == older.commit_oid));

        let result = export_skill_to_project_with_report_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            Some(vec!["claude_code".into()]),
            Some(&older.commit_oid),
        )
        .unwrap();
        assert!(result.ok);
        assert_eq!(result.targets[0].result, "created");
        let target = fx.repo.join(".claude/skills/demo");
        let bytes = fs::read_to_string(target.join("SKILL.md")).unwrap();
        assert!(bytes.contains("FIRST COMMITTED"), "{bytes}");
        assert!(!bytes.contains("DIRTY"));
        let owned = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &target.to_string_lossy())
            .unwrap()
            .unwrap();
        assert_eq!(
            owned.base_commit_oid.as_deref(),
            Some(older.commit_oid.as_str())
        );
        assert_eq!(
            owned.base_tree_oid.as_deref(),
            Some(older.tree_oid.as_str())
        );
        git_skill_versions::verify_deployment_pin(
            &central_repo::skills_dir(),
            &owned.id,
            &older.commit_oid,
        )
        .unwrap();
    }

    #[test]
    fn gui_multi_agent_export_reports_partial_success_with_recovery_path() {
        let fx = fixture(&["claude_code", "codex"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        commit_library(&fx);
        let db = rusqlite::Connection::open(fx._tmp.path().join("skills.db")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER refuse_second_gui_deploy
             BEFORE INSERT ON project_skill_deployments
             WHEN NEW.target_path LIKE '%/.codex/skills/demo'
             BEGIN SELECT RAISE(ABORT, 'injected GUI codex failure'); END;",
        )
        .unwrap();
        let result = export_skill_to_project_with_report_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            Some(vec!["claude_code".into(), "codex".into()]),
            None,
        )
        .unwrap();
        assert!(!result.ok);
        assert_eq!(result.targets.len(), 2);
        assert_eq!(result.targets[0].result, "created");
        assert_eq!(result.targets[1].result, "failed");
        assert!(result.targets[1]
            .error
            .as_ref()
            .unwrap()
            .contains("database commit failed"));
        assert_eq!(result.targets[1].recovery.len(), 1);
        let recovery = &result.targets[1].recovery[0];
        assert_eq!(recovery.phase, "recovery_available");
        assert!(Path::new(&recovery.recovery_root)
            .join("recovered/SKILL.md")
            .is_file());
        assert!(fx.repo.join(".claude/skills/demo/SKILL.md").is_file());
        assert!(!fx.repo.join(".codex/skills/demo").exists());
        assert!(fx
            .store
            .get_project_skill_deployment(
                &fx.project_id,
                &fx.repo.join(".claude/skills/demo").to_string_lossy(),
            )
            .unwrap()
            .is_some());
        assert!(fx
            .store
            .get_project_skill_deployment(
                &fx.project_id,
                &fx.repo.join(".codex/skills/demo").to_string_lossy(),
            )
            .unwrap()
            .is_none());
        db.execute_batch("DROP TRIGGER refuse_second_gui_deploy")
            .unwrap();
        let retry = export_skill_to_project_with_report_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            Some(vec!["codex".into()]),
            None,
        )
        .unwrap();
        assert!(retry.ok);
        assert_eq!(retry.targets[0].result, "created");
        assert!(fx.repo.join(".codex/skills/demo/SKILL.md").is_file());
        assert!(Path::new(&recovery.recovery_root)
            .join("recovered/SKILL.md")
            .is_file());
    }

    #[test]
    fn gui_multi_agent_export_stops_after_first_failed_target() {
        let fx = fixture(&["claude_code", "codex"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        commit_library(&fx);
        let db = rusqlite::Connection::open(fx._tmp.path().join("skills.db")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER refuse_first_gui_deploy
             BEFORE INSERT ON project_skill_deployments
             WHEN NEW.target_path LIKE '%/.claude/skills/demo'
             BEGIN SELECT RAISE(ABORT, 'injected GUI claude failure'); END;",
        )
        .unwrap();
        let report = export_skill_to_project_with_report_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            Some(vec!["claude_code".into(), "codex".into()]),
            None,
        )
        .unwrap();
        assert!(!report.ok);
        assert_eq!(report.targets.len(), 2);
        assert_eq!(report.targets[0].result, "failed");
        assert_eq!(report.targets[1].result, "not_attempted");
        assert!(report.targets[1].error.is_none());
        assert!(report.targets[1].recovery.is_empty());
        assert!(!fx.repo.join(".codex/skills/demo").exists());
        assert!(!fx.repo.join(".claude/skills/demo").exists());
        let pending = get_project_skill_recoveries_internal(&fx.store, &fx.project_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].recovery_root_exists);
    }

    #[test]
    fn gui_multi_agent_export_keeps_legacy_all_targets_preflight_refusal() {
        let fx = fixture(&["claude_code", "codex"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let blocked = fx.repo.join(".codex/skills/demo");
        write_skill(&blocked, "demo", "# external");
        let before = fs::read(blocked.join("SKILL.md")).unwrap();
        let err = export_skill_to_project_with_report_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            Some(vec!["claude_code".into(), "codex".into()]),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert!(!fx.repo.join(".claude/skills/demo").exists());
        assert_eq!(fs::read(blocked.join("SKILL.md")).unwrap(), before);
    }

    #[test]
    fn first_deploy_creates_only_selected_agent_directories() {
        let fx = fixture(&["claude_code", "codex"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        assert_eq!(
            fs::read_dir(&fx.repo).unwrap().count(),
            0,
            "registration is metadata-only"
        );

        let claude = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(claude[0].state, ProjectDeployState::Create);
        assert!(
            !fx.repo.join(".claude").exists(),
            "planning must not create directories"
        );
        apply_project_deploy(&fx.store, &claude).unwrap();
        assert!(fx.repo.join(".claude/skills/demo/SKILL.md").is_file());
        assert!(!fx.repo.join(".claude/skills-disabled").exists());
        assert!(
            !fx.repo.join(".codex").exists(),
            "unselected agent must remain untouched"
        );

        let codex =
            plan_project_deploy(&fx.store, &fx.project_id, &ids(&[&demo]), &["codex".into()])
                .unwrap();
        assert_eq!(codex[0].state, ProjectDeployState::Create);
        assert!(
            !fx.repo.join(".codex").exists(),
            "planning must not create directories"
        );
        apply_project_deploy(&fx.store, &codex).unwrap();
        assert!(fx.repo.join(".codex/skills/demo/SKILL.md").is_file());
        assert!(!fx.repo.join(".codex/skills-disabled").exists());
        assert!(!fx.repo.join(".agents").exists());
    }

    #[test]
    fn deploy_plan_creates_then_recognizes_the_same_skill_without_writing_on_plan() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let target = fx.repo.join(".claude/skills/demo");

        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].state, ProjectDeployState::Create);
        assert!(
            fs::symlink_metadata(&target).is_err(),
            "planning must not write"
        );

        apply_project_deploy(&fx.store, &plan).unwrap();
        assert!(target.join("SKILL.md").is_file());

        let again = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(again[0].state, ProjectDeployState::AlreadyDeployed);
        assert_eq!(again[0].sync_status.as_deref(), Some("in_sync"));

        // The GUI path keeps refusing anything already there, as before.
        let gui = export_skill_to_project_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            None,
        );
        assert!(gui.unwrap_err().to_string().contains("already exists"));
    }

    /// The directory name is taken, but by another library skill's content:
    /// deploying must not report it as done, and must not touch it.
    #[test]
    fn deploy_refuses_a_same_named_directory_that_belongs_to_another_skill() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let other = library_skill(&fx, "s-other", "other", "# Other");
        let squatter = fx.repo.join(".claude/skills/demo");
        fs::create_dir_all(squatter.parent().unwrap()).unwrap();
        // Byte-identical to `other`, so its unique hash names `other`.
        fs::create_dir_all(&squatter).unwrap();
        fs::copy(
            Path::new(&other.central_path).join("SKILL.md"),
            squatter.join("SKILL.md"),
        )
        .unwrap();
        let before = fs::read_to_string(squatter.join("SKILL.md")).unwrap();

        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(plan[0].state, ProjectDeployState::Conflict);
        let err = project_deploy_conflicts(&plan).expect("a conflict");
        assert_eq!(conflict_paths(&err), vec![squatter.clone()]);

        // The GUI path refuses as well, and the directory is untouched.
        let gui = export_skill_to_project_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            Some(vec!["claude_code".into()]),
        );
        assert!(gui.unwrap_err().to_string().contains("already exists"));
        assert_eq!(
            fs::read_to_string(squatter.join("SKILL.md")).unwrap(),
            before
        );
    }

    /// Review finding 5: two library skills whose directories share a name
    /// (`a/demo`, `b/demo`) would both land in `<project>/.claude/skills/demo`.
    /// The batch is refused before anything is written.
    #[test]
    fn deploy_refuses_two_skills_that_land_on_the_same_directory() {
        let fx = fixture(&["claude_code"]);
        let mut skills = Vec::new();
        for (id, group, body) in [("s-a", "a", "# A's demo"), ("s-b", "b", "# B's demo")] {
            let central = central_repo::skills_dir().join(group).join("demo");
            write_skill(&central, "demo", body);
            let mut record =
                library_skill(&fx, &format!("{id}-tmp"), &format!("{group}-tmp"), body);
            fs::remove_dir_all(&record.central_path).unwrap();
            fx.store.delete_skill(&record.id).unwrap();
            record.id = id.to_string();
            record.name = format!("{group}-demo");
            record.central_path = central.to_string_lossy().to_string();
            fx.store.insert_skill(&record).unwrap();
            skills.push(record);
        }

        let err = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&skills[0], &skills[1]]),
            &["claude_code".into()],
        )
        .expect_err("two skills on one directory");
        let target = fx.repo.join(".claude/skills/demo");
        assert_eq!(conflict_paths(&err), vec![target.clone()]);
        let reason = &err.details.as_ref().unwrap().conflicts[0].reason;
        assert!(
            reason.contains("a-demo") && reason.contains("b-demo"),
            "{reason}"
        );
        assert!(
            reason.contains(&skills[0].central_path) && reason.contains(&skills[1].central_path),
            "names can coincide, so the library directories are named: {reason}"
        );
        assert!(fs::symlink_metadata(&target).is_err());
    }

    #[test]
    fn deploy_refuses_a_directory_that_is_not_a_skill() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let squatter = fx.repo.join(".claude/skills/demo");
        fs::create_dir_all(&squatter).unwrap();
        fs::write(squatter.join("notes.txt"), "mine").unwrap();

        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        assert_eq!(plan[0].state, ProjectDeployState::Conflict);
        assert!(project_deploy_conflicts(&plan).is_some());
    }

    /// Warp reads `.agents/skills`, whose row is filed under another agent.
    /// Naming Warp must reach that directory, from the CLI and the GUI path.
    #[test]
    fn any_agent_reading_a_shared_directory_can_deploy_into_it() {
        let fx = fixture(&["warp"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let rep = project_agent_group(
            &fx.store,
            &get_project_record(&fx.store, &fx.project_id).unwrap(),
            "warp",
        )
        .unwrap();
        assert_ne!(
            rep.key, "warp",
            "the row must be filed under another agent for this test to mean anything"
        );
        assert!(rep.shared_by.contains(&"warp".to_string()));

        let plan = plan_project_deploy(&fx.store, &fx.project_id, &ids(&[&demo]), &["warp".into()])
            .unwrap();
        assert_eq!(plan[0].agent, rep.key);
        assert_eq!(
            PathBuf::from(&plan[0].path),
            fx.repo.join(".agents/skills/demo")
        );

        export_skill_to_project_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            Some(vec!["warp".into()]),
        )
        .unwrap();
        assert!(fx.repo.join(".agents/skills/demo/SKILL.md").is_file());
    }

    #[test]
    fn a_missing_project_folder_is_refused_and_not_recreated() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        fs::remove_dir_all(&fx.repo).unwrap();

        let err = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Project path does not exist"),
            "{err}"
        );
        let gui = export_skill_to_project_internal(
            &fx.store,
            demo.id.clone(),
            fx.project_id.clone(),
            None,
        );
        assert!(gui
            .unwrap_err()
            .to_string()
            .contains("Project path does not exist"));
        assert!(
            !fx.repo.exists(),
            "the project folder must not be recreated"
        );
        assert!(plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).is_err());
        assert!(plan_project_import(&fx.store, &fx.project_id, "demo", "claude_code").is_err());
    }

    /// A copied deployment the user edited holds content the library lacks.
    #[test]
    fn undeploy_refuses_a_copy_that_is_not_in_sync_unless_forced() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        let copy = fx.repo.join(".claude/skills/demo");
        fs::write(
            copy.join("SKILL.md"),
            "---\nname: demo\n---\n# Edited in the project\n",
        )
        .unwrap();

        let removal =
            plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
        assert_eq!(removal.len(), 1);
        assert!(!removal[0].safe, "status {}", removal[0].sync_status);
        assert_eq!(
            conflict_paths(&project_removal_conflicts(&removal).unwrap()),
            vec![copy.clone()]
        );

        let err = delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .unwrap_err();
        assert_eq!(conflict_paths(&err).len(), 1);
        assert!(
            copy.join("SKILL.md").is_file(),
            "a refused removal deletes nothing"
        );

        delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            true,
        )
        .unwrap();
        assert!(!copy.exists());
    }

    #[test]
    fn undeploy_removes_an_in_sync_copy_and_only_this_skills_copies() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let other = library_skill(&fx, "s-other", "other", "# Other");
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo, &other]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        // A skill written in the project, matching nothing in the library.
        write_skill(&fx.repo.join(".claude/skills/mine"), "mine", "# Mine");

        let removal =
            plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
        assert_eq!(removal.len(), 1);
        assert_eq!(removal[0].relative_path, "demo");
        assert!(removal[0].safe);
        assert!(
            fx.repo.join(".claude/skills/demo").exists(),
            "planning must not delete"
        );

        delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .unwrap();
        assert!(!fx.repo.join(".claude/skills/demo").exists());
        assert!(fx.repo.join(".claude/skills/other").exists());
        assert!(fx.repo.join(".claude/skills/mine").exists());
    }

    /// Without force, a project-only skill (nothing in the library holds its
    /// content) cannot be deleted through the shared path.
    #[test]
    fn delete_without_force_refuses_a_project_only_skill() {
        let fx = fixture(&["claude_code"]);
        write_skill(&fx.repo.join(".claude/skills/mine"), "mine", "# Mine");
        let err = delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "mine".into(),
            "claude_code".into(),
            false,
        )
        .unwrap_err();
        assert_eq!(conflict_paths(&err).len(), 1);
        assert!(fx.repo.join(".claude/skills/mine/SKILL.md").is_file());
    }

    /// A directory the scanner does not recognize as a skill (no SKILL.md)
    /// cannot be judged, so without force it is not deleted either.
    #[test]
    fn delete_without_force_refuses_a_directory_that_is_not_a_skill() {
        let fx = fixture(&["claude_code"]);
        let junk = fx.repo.join(".claude/skills/junk");
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("notes.txt"), "mine").unwrap();
        let err = delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "junk".into(),
            "claude_code".into(),
            false,
        )
        .unwrap_err();
        assert_eq!(conflict_paths(&err).len(), 1);
        assert!(junk.join("notes.txt").is_file());
    }

    /// The enabled slot holds the user's unrelated files; the disabled slot
    /// holds an in-sync copy. Whatever passes the check must be what is
    /// removed: the unrelated files are never touched without force.
    #[test]
    fn a_removal_deletes_exactly_the_copy_it_checked() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let disabled = fx.repo.join(".claude/skills-disabled/demo");
        fs::create_dir_all(&disabled).unwrap();
        fs::copy(
            Path::new(&demo.central_path).join("SKILL.md"),
            disabled.join("SKILL.md"),
        )
        .unwrap();
        let junk = fx.repo.join(".claude/skills/demo");
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("notes.txt"), "mine").unwrap();

        // By relative path: the slot that would be deleted is the junk one,
        // which is not a recognizable skill, so it is refused.
        let err = delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .unwrap_err();
        assert_eq!(conflict_paths(&err), vec![junk.clone()]);
        assert!(junk.join("notes.txt").is_file());
    }

    /// A hand-installed equal-content Copy is not automatically owned. It
    /// needs explicit --force even though the other slot is unrelated junk.
    #[test]
    fn a_planned_removal_preserves_unmanaged_copy_until_explicit_force() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let disabled = fx.repo.join(".claude/skills-disabled/demo");
        fs::create_dir_all(&disabled).unwrap();
        fs::copy(
            Path::new(&demo.central_path).join("SKILL.md"),
            disabled.join("SKILL.md"),
        )
        .unwrap();
        let junk = fx.repo.join(".claude/skills/demo");
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("notes.txt"), "mine").unwrap();

        let plan = plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(PathBuf::from(&plan[0].path), disabled);
        assert!(!plan[0].safe, "unmanaged Copy is never implicitly owned");
        assert!(remove_planned_project_copy(&fx.store, &fx.project_id, &plan[0], false).is_err());
        assert!(disabled.exists());
        remove_planned_project_copy(&fx.store, &fx.project_id, &plan[0], true).unwrap();
        assert!(!disabled.exists());
        assert!(junk.join("notes.txt").is_file());
    }

    #[test]
    fn force_undeploy_refuses_a_target_replaced_after_the_plan() {
        let fx = fixture(&["claude_code"]);
        let skill = library_skill(&fx, "s-demo", "demo", "# Demo");
        let target = fx.repo.join(".claude/skills/demo");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("SKILL.md"), "---\nname: demo\n---\n# Demo\n").unwrap();
        let plan = plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&skill]), &[]).unwrap();
        assert_eq!(plan.len(), 1);
        let parked = target.with_file_name("original-parked");
        fs::rename(&target, &parked).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("external"), "do not touch").unwrap();
        assert!(remove_planned_project_copy(&fx.store, &fx.project_id, &plan[0], true,).is_err());
        assert!(target.join("external").exists());
        assert!(parked.join("SKILL.md").exists());
    }

    /// A link the user made to somewhere else is their configuration, even
    /// when its target's content happens to equal the library's. Only a link
    /// that resolves to the matched library skill's own directory is ours.
    #[cfg(unix)]
    #[test]
    fn undeploy_refuses_a_symlink_that_does_not_point_at_the_library_copy() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        let skills = fx.repo.join(".claude/skills");
        fs::create_dir_all(&skills).unwrap();
        let link = skills.join("demo");

        for (label, body) in [
            ("different", "# Someone else's demo"),
            ("identical", "# Demo"),
        ] {
            let elsewhere = fx._tmp.path().join(format!("elsewhere-{label}/demo"));
            write_skill(&elsewhere, "demo", body);
            if label == "identical" {
                fs::copy(
                    Path::new(&demo.central_path).join("SKILL.md"),
                    elsewhere.join("SKILL.md"),
                )
                .unwrap();
            }
            let _ = fs::remove_file(&link);
            std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

            let plan =
                plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
            assert_eq!(plan.len(), 1, "{label}");
            assert!(!plan[0].safe, "{label}: status {}", plan[0].sync_status);
            assert!(
                delete_project_skill_internal(
                    &fx.store,
                    fx.project_id.clone(),
                    "demo".into(),
                    "claude_code".into(),
                    false
                )
                .is_err(),
                "{label}"
            );
            assert!(
                fs::symlink_metadata(&link).is_ok(),
                "{label}: the link stays"
            );
        }

        // A manually created link is external even when it points at the
        // exact central Skill. Only an actually manager-created link is owned.
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&demo.central_path, &link).unwrap();
        let plan = plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
        assert!(!plan[0].safe);
        assert!(delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .is_err());
        assert!(fs::symlink_metadata(&link).is_ok());
        fs::remove_file(&link).unwrap();
        let deploy = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &deploy).unwrap();
        let record = fx
            .store
            .get_project_skill_deployment(&fx.project_id, &link.to_string_lossy())
            .unwrap()
            .unwrap();
        assert_eq!(record.mode, "symlink");
        assert_eq!(record.management_origin, "deployed_by_manager");
        assert!(managed_symlink_matches(&record, &link));
        let plan = plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
        assert!(plan[0].safe);
        delete_project_skill_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .unwrap();
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(
            Path::new(&demo.central_path).join("SKILL.md").is_file(),
            "the library copy survives"
        );
    }

    /// Equal hashes do not prove a copy holds nothing of its own: the hash
    /// skips `.gitignore`, `.git` and links (#593). Anything in the copy that
    /// the library copy lacks makes the removal unsafe.
    #[cfg(unix)]
    #[test]
    fn undeploy_refuses_an_in_sync_copy_holding_entries_the_library_lacks() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        commit_library(&fx);
        let copy = fx.repo.join(".claude/skills/demo");
        let extras: [(&str, fn(&Path)); 3] = [
            (".gitignore", |copy| {
                fs::write(copy.join(".gitignore"), "secrets/\n").unwrap()
            }),
            (".git", |copy| {
                fs::create_dir_all(copy.join(".git")).unwrap();
                fs::write(copy.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
            }),
            ("link", |copy| {
                std::os::unix::fs::symlink("/tmp", copy.join("link")).unwrap()
            }),
        ];

        for (label, add_extra) in extras {
            let _ = fs::remove_dir_all(&copy);
            let plan = plan_project_deploy(
                &fx.store,
                &fx.project_id,
                &ids(&[&demo]),
                &["claude_code".into()],
            )
            .unwrap();
            apply_project_deploy(&fx.store, &plan).unwrap();
            add_extra(&copy);

            let removal =
                plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap();
            assert_eq!(
                removal[0].sync_status, "in_sync",
                "{label}: the hash cannot see it"
            );
            assert!(!removal[0].safe, "{label}: must not be safe");
            assert!(
                delete_project_skill_internal(
                    &fx.store,
                    fx.project_id.clone(),
                    "demo".into(),
                    "claude_code".into(),
                    false
                )
                .is_err(),
                "{label}"
            );
            assert!(copy.exists(), "{label}: kept");
        }

        // A plain copy with nothing extra is still removable.
        fs::remove_dir_all(&copy).unwrap();
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        assert!(
            plan_project_undeploy(&fx.store, &fx.project_id, &ids(&[&demo]), &[]).unwrap()[0].safe
        );
    }

    /// Review finding 4: whether an import may overwrite is decided under the
    /// repo lock. Here another writer installs a differing "mine" while the
    /// import waits for the lock; without --force it must be refused, not
    /// overwritten on the strength of a plan made before the lock.
    #[test]
    fn import_decides_whether_it_overwrites_under_the_lock() {
        let fx = fixture(&["claude_code"]);
        write_skill(
            &fx.repo.join(".claude/skills/mine"),
            "mine",
            "# Project version",
        );
        let held = RepoLock::acquire_foreground("test holds the lock").unwrap();

        let result = std::thread::scope(|scope| {
            let import = scope.spawn(|| {
                import_project_skill_to_center_internal(
                    &fx.store,
                    fx.project_id.clone(),
                    "mine".into(),
                    "claude_code".into(),
                    false,
                )
            });
            // Long enough for the import to plan and block on the lock.
            std::thread::sleep(std::time::Duration::from_millis(500));
            library_skill(&fx, "s-mine", "mine", "# Someone else's version");
            drop(held);
            import.join().unwrap()
        });

        let err = result.expect_err("an overwrite without --force must be refused");
        assert_eq!(conflict_paths(&err).len(), 1);
        let library = central_repo::skills_dir().join("mine/SKILL.md");
        assert!(fs::read_to_string(library)
            .unwrap()
            .contains("Someone else's version"));
    }

    /// Review finding 7: a dry run must fail where the run certainly will.
    /// A library skill whose own directory is gone cannot be deployed.
    #[test]
    fn deploy_plan_refuses_a_library_skill_whose_directory_is_missing() {
        let fx = fixture(&["claude_code"]);
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        fs::remove_dir_all(&demo.central_path).unwrap();

        let err = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .expect_err("the run would fail, so the plan must");
        assert!(err.to_string().contains(&demo.central_path), "{err}");
        assert!(fs::symlink_metadata(fx.repo.join(".claude/skills/demo")).is_err());
    }

    /// A skill name the installer rejects (`...` is empty once trailing dots
    /// go) fails the import plan as it fails the import.
    #[test]
    fn import_plan_refuses_a_name_the_installer_rejects() {
        let fx = fixture(&["claude_code"]);
        write_skill(&fx.repo.join(".claude/skills/dots"), "...", "# Dots");

        let real = import_project_skill_to_center_internal(
            &fx.store,
            fx.project_id.clone(),
            "dots".into(),
            "claude_code".into(),
            false,
        );
        assert!(real.is_err(), "the installer rejects the name");
        let err = plan_project_import(&fx.store, &fx.project_id, "dots", "claude_code")
            .expect_err("the plan must refuse what the run refuses");
        assert!(err.to_string().contains("Invalid skill name"), "{err}");
        assert!(fx.store.get_all_skills().unwrap().is_empty());
    }

    #[test]
    fn import_creates_a_library_skill_with_metadata_and_audit() {
        let fx = fixture(&["claude_code"]);
        write_skill(&fx.repo.join(".claude/skills/mine"), "mine", "# Mine");
        assert!(!sync_metadata::has_complete_skill_snapshot());

        let plan = plan_project_import(&fx.store, &fx.project_id, "mine", "claude_code").unwrap();
        assert_eq!(plan.action, ProjectImportAction::Create);
        assert!(
            fx.store.get_all_skills().unwrap().is_empty(),
            "planning must not write the library"
        );

        let done = import_project_skill_to_center_internal(
            &fx.store,
            fx.project_id.clone(),
            "mine".into(),
            "claude_code".into(),
            false,
        )
        .unwrap();
        assert_eq!(done.action, ProjectImportAction::Create);
        let skills = fx.store.get_all_skills().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(Some(skills[0].id.clone()), done.skill_id);
        assert!(Path::new(&skills[0].central_path)
            .join("SKILL.md")
            .is_file());
        assert!(fx
            .store
            .list_audit(None)
            .unwrap()
            .iter()
            .any(|e| e.action == "import"));
        assert!(
            sync_metadata::has_complete_skill_snapshot(),
            "sync metadata must be written"
        );

        let again = plan_project_import(&fx.store, &fx.project_id, "mine", "claude_code").unwrap();
        assert_eq!(again.action, ProjectImportAction::Unchanged);
    }

    #[test]
    fn import_needs_force_to_overwrite_a_differing_library_copy() {
        let fx = fixture(&["claude_code"]);
        fx.store.set_setting("sync_mode", "copy").unwrap();
        let demo = library_skill(&fx, "s-demo", "demo", "# Demo");
        commit_library(&fx);
        let plan = plan_project_deploy(
            &fx.store,
            &fx.project_id,
            &ids(&[&demo]),
            &["claude_code".into()],
        )
        .unwrap();
        apply_project_deploy(&fx.store, &plan).unwrap();
        fs::write(
            fx.repo.join(".claude/skills/demo/SKILL.md"),
            "---\nname: demo\n---\n# Better\n",
        )
        .unwrap();
        let library_file = Path::new(&demo.central_path).join("SKILL.md");
        let before = fs::read_to_string(&library_file).unwrap();

        assert_eq!(
            plan_project_import(&fx.store, &fx.project_id, "demo", "claude_code")
                .unwrap()
                .action,
            ProjectImportAction::Overwrite
        );
        let err = import_project_skill_to_center_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            false,
        )
        .unwrap_err();
        assert_eq!(
            conflict_paths(&err),
            vec![PathBuf::from(&demo.central_path)]
        );
        assert_eq!(
            fs::read_to_string(&library_file).unwrap(),
            before,
            "refused import must not write"
        );

        let done = import_project_skill_to_center_internal(
            &fx.store,
            fx.project_id.clone(),
            "demo".into(),
            "claude_code".into(),
            true,
        )
        .unwrap();
        assert_eq!(done.action, ProjectImportAction::Overwrite);
        assert!(fs::read_to_string(&library_file)
            .unwrap()
            .contains("# Better"));
    }

    /// Two agents' copies both differ from the library: pushing either would
    /// make the other look stale and invite overwriting it.
    #[test]
    fn import_refuses_when_several_copies_differ_even_with_force() {
        let fx = fixture(&["claude_code", "cursor"]);
        write_skill(
            &fx.repo.join(".claude/skills/mine"),
            "mine",
            "# Claude edit",
        );
        write_skill(
            &fx.repo.join(".cursor/skills/mine"),
            "mine",
            "# Cursor edit",
        );

        for force in [false, true] {
            let err = import_project_skill_to_center_internal(
                &fx.store,
                fx.project_id.clone(),
                "mine".into(),
                "claude_code".into(),
                force,
            )
            .unwrap_err();
            assert_eq!(conflict_paths(&err).len(), 2, "force={force}");
        }
        assert!(plan_project_import(&fx.store, &fx.project_id, "mine", "cursor").is_err());
        assert!(fx.store.get_all_skills().unwrap().is_empty());
    }
}
