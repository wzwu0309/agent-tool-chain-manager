//! A strict fingerprint of a deployed project Copy tree. Unlike the ordinary
//! skill content hash, this includes ignored/hidden entries, symlinks and empty
//! directories. A missing/unreadable entry must never certify a safe overwrite.
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use walkdir::WalkDir;

pub fn fingerprint(root: &Path) -> Result<String> {
    let root_meta = std::fs::symlink_metadata(root)?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        bail!(
            "Project Copy is no longer a real directory: {}",
            root.display()
        );
    }
    let mut entries = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("Cannot scan project copy {}", root.display()))?;
    entries.sort_by(|a, b| a.path().cmp(b.path()));
    let mut hash = Sha256::new();
    hash.update(b"agent-toolchain-project-copy-v1\0");
    for entry in entries.into_iter().filter(|e| e.depth() > 0) {
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        // Do not collapse distinct non-UTF-8 filenames through lossy decoding.
        let bytes = relative.as_os_str().as_encoded_bytes();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("Cannot stat {}", path.display()))?;
        let kind = metadata.file_type();
        if kind.is_symlink() {
            hash.update(b"L");
            let link = std::fs::read_link(path)?;
            let target = link.as_os_str().as_encoded_bytes();
            hash.update((target.len() as u64).to_le_bytes());
            hash.update(target);
        } else if kind.is_dir() {
            hash.update(b"D");
        } else if kind.is_file() {
            hash.update(b"F");
            let content =
                std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
            hash.update((content.len() as u64).to_le_bytes());
            hash.update(content);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                hash.update((metadata.permissions().mode() & 0o111).to_le_bytes());
            }
        } else {
            bail!("Unsupported entry in project copy: {}", path.display());
        }
    }
    Ok(hex::encode(hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_hidden_extra_deleted_renamed_and_content_edits() {
        let root = tempdir().unwrap();
        let skill = root.path();
        fs::write(skill.join("SKILL.md"), "original").unwrap();
        let base = fingerprint(skill).unwrap();
        fs::write(skill.join("SKILL.md"), "changed").unwrap();
        assert_ne!(base, fingerprint(skill).unwrap());
        fs::write(skill.join("SKILL.md"), "original").unwrap();
        fs::write(skill.join(".gitignore"), "ignore").unwrap();
        assert_ne!(base, fingerprint(skill).unwrap());
        fs::remove_file(skill.join(".gitignore")).unwrap();
        fs::rename(skill.join("SKILL.md"), skill.join("renamed.md")).unwrap();
        assert_ne!(base, fingerprint(skill).unwrap());
        fs::remove_file(skill.join("renamed.md")).unwrap();
        assert_ne!(base, fingerprint(skill).unwrap());
        fs::write(skill.join("SKILL.md"), "original").unwrap();
        fs::create_dir(skill.join("empty")).unwrap();
        assert_ne!(base, fingerprint(skill).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn detects_symlink_addition_and_rejects_linked_root() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("SKILL.md"), "original").unwrap();
        let base = fingerprint(root.path()).unwrap();
        std::os::unix::fs::symlink("SKILL.md", root.path().join("alias")).unwrap();
        assert_ne!(base, fingerprint(root.path()).unwrap());
        std::os::unix::fs::symlink(root.path(), root.path().join("linked-root")).unwrap();
        assert!(fingerprint(&root.path().join("linked-root")).is_err());
    }
}
