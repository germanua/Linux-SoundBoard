use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn current_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

fn validate_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.uid() != current_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe runtime directory",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

pub fn directory() -> io::Result<PathBuf> {
    let base = if let Some(value) = std::env::var_os("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(value);
        validate_directory(&path)?;
        path
    } else {
        let path =
            std::env::temp_dir().join(format!("{}-{}", crate::app_meta::APP_BINARY, current_uid()));
        match fs::symlink_metadata(&path) {
            Ok(_) => validate_directory(&path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&path)?;
                validate_directory(&path)?;
            }
            Err(error) => return Err(error),
        }
        path
    };
    let path = base.join(crate::app_meta::APP_BINARY);
    match fs::symlink_metadata(&path) {
        Ok(_) => validate_directory(&path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&path)?;
            validate_directory(&path)?;
        }
        Err(error) => return Err(error),
    }
    Ok(path)
}

fn validate_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.uid() != current_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe runtime file",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

pub fn open_append(path: &Path) -> io::Result<File> {
    if path.exists() || fs::symlink_metadata(path).is_ok() {
        validate_file(path)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    validate_file(path)?;
    Ok(file)
}

pub fn open_new(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    validate_file(path)?;
    Ok(file)
}

pub fn open_truncate(path: &Path) -> io::Result<File> {
    if path.exists() || fs::symlink_metadata(path).is_ok() {
        validate_file(path)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    validate_file(path)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn private_file_mode_is_enforced() {
        let dir =
            std::env::temp_dir().join(format!("lsb-private-runtime-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).expect("create dir");
        let path = dir.join("file");
        drop(open_append(&path).expect("open file"));
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn symlink_target_is_not_followed() {
        let dir =
            std::env::temp_dir().join(format!("lsb-private-symlink-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).expect("create dir");
        let target = dir.join("target");
        let link = dir.join("link");
        fs::write(&target, b"unchanged").expect("write target");
        symlink(&target, &link).expect("create symlink");
        assert!(open_append(&link).is_err());
        assert_eq!(fs::read(&target).expect("read target"), b"unchanged");
        fs::remove_dir_all(dir).expect("cleanup");
    }
}
