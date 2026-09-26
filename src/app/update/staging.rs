use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{downloader, state, UpdateError, UpdateInfo};

fn uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

fn ensure_directory(path: &Path) -> Result<(), UpdateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && metadata.uid() == uid() => {}
        Ok(_) => return Err(UpdateError::State("unsafe update staging directory".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| UpdateError::State(error.to_string()))?;
        }
        Err(error) => return Err(UpdateError::State(error.to_string())),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| UpdateError::State(error.to_string()))
}

fn prune_old_staging(updates: &Path, keep: &semver::Version) -> Result<(), UpdateError> {
    let keep = keep.to_string();
    for entry in fs::read_dir(updates).map_err(|error| UpdateError::State(error.to_string()))? {
        let entry = entry.map_err(|error| UpdateError::State(error.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "backups" || name == keep || semver::Version::parse(name).is_err() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| UpdateError::State(error.to_string()))?;
        if metadata.file_type().is_dir() && metadata.uid() == uid() {
            fs::remove_dir_all(entry.path())
                .map_err(|error| UpdateError::State(error.to_string()))?;
        }
    }
    Ok(())
}

fn staging_directory(version: &semver::Version) -> Result<PathBuf, UpdateError> {
    let state_path = state::state_path()?;
    let app_root = state_path
        .parent()
        .ok_or_else(|| UpdateError::State("missing updater state parent".into()))?;
    let updates = app_root.join("updates");
    if !updates.exists() {
        fs::create_dir(&updates).map_err(|error| UpdateError::State(error.to_string()))?;
    }
    ensure_directory(&updates)?;
    prune_old_staging(&updates, version)?;
    let version_dir = updates.join(version.to_string());
    if !version_dir.exists() {
        fs::create_dir(&version_dir).map_err(|error| UpdateError::State(error.to_string()))?;
    }
    ensure_directory(&version_dir)?;
    Ok(version_dir)
}

fn remove_owned_file(path: &Path) -> Result<(), UpdateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.uid() == uid() => {
            fs::remove_file(path).map_err(|error| UpdateError::State(error.to_string()))
        }
        Ok(_) => Err(UpdateError::State(format!(
            "refusing to replace unsafe staging path '{}'",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(UpdateError::State(error.to_string())),
    }
}

fn write_private(path: &Path, data: &[u8]) -> Result<(), UpdateError> {
    remove_owned_file(path)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    file.write_all(data)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    file.sync_all()
        .map_err(|error| UpdateError::State(error.to_string()))
}

fn create_partial(path: &Path) -> Result<File, UpdateError> {
    remove_owned_file(path)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| UpdateError::State(error.to_string()))
}

fn validate_appimage(path: &Path) -> Result<(), UpdateError> {
    let mut file = File::open(path).map_err(|error| UpdateError::State(error.to_string()))?;
    let mut header = [0u8; 11];
    file.read_exact(&mut header)
        .map_err(|error| UpdateError::Verification(error.to_string()))?;
    if header[0..4] != [0x7f, b'E', b'L', b'F'] || header[8..11] != [b'A', b'I', 0x02] {
        return Err(UpdateError::Verification(
            "downloaded artifact is not a type-2 AppImage".into(),
        ));
    }
    Ok(())
}

pub fn stage<F>(
    info: &UpdateInfo,
    cancelled: &AtomicBool,
    on_progress: F,
) -> Result<PathBuf, UpdateError>
where
    F: Fn(downloader::DownloadProgress),
{
    let dir = staging_directory(&info.metadata.version)?;
    write_private(&dir.join("SHA256SUMS.txt"), &info.manifest)?;
    write_private(
        &dir.join("SHA256SUMS.txt.minisig"),
        info.signature.as_bytes(),
    )?;
    write_private(&dir.join("update.json"), &info.metadata_json)?;

    let partial = dir.join(format!("{}.partial", info.metadata.appimage.name));
    let final_path = dir.join(&info.metadata.appimage.name);
    remove_owned_file(&final_path)?;
    let mut file = create_partial(&partial)?;
    let download = downloader::download_artifact(
        &info.appimage_url,
        info.metadata.appimage.size,
        &mut file,
        cancelled,
        &on_progress,
    );
    let (_, hash) = match download {
        Ok(result) => result,
        Err(error) => {
            drop(file);
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
    };
    if cancelled.load(Ordering::Relaxed) {
        drop(file);
        let _ = fs::remove_file(&partial);
        return Err(UpdateError::Cancelled);
    }
    if hash != info.appimage_sha256 {
        drop(file);
        let _ = fs::remove_file(&partial);
        return Err(UpdateError::Verification(
            "downloaded AppImage SHA-256 mismatch".into(),
        ));
    }
    drop(file);
    validate_appimage(&partial)?;
    fs::rename(&partial, &final_path).map_err(|error| UpdateError::State(error.to_string()))?;
    fs::set_permissions(&final_path, fs::Permissions::from_mode(0o755))
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let mut persisted = state::load()?;
    persisted.staged_version = Some(info.metadata.version.clone());
    state::save(&persisted)?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn staging_refuses_symlink_file_replacement() {
        let root = std::env::temp_dir().join(format!("lsb-stage-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target");
        let link = root.join("link");
        fs::write(&target, b"unchanged").unwrap();
        symlink(&target, &link).unwrap();
        assert!(write_private(&link, b"changed").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unchanged");
        fs::remove_dir_all(root).unwrap();
    }
}
