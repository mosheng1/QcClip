use std::{fs, io, path::Path};

/// The file exists and is reachable.
pub(crate) fn path_is_certainly_present(path: &Path) -> bool {
    is_probeable_external_path(path) && fs::metadata(path).is_ok()
}

/// The file cannot exist: it is absent and some ancestor directory is still
/// reachable. Unavailable shares and unmounted volumes yield `NotFound` for the
/// whole ancestor chain and stay unknown, so their entries are not hidden.
pub(crate) fn path_is_certainly_missing(path: &Path) -> bool {
    if !is_probeable_external_path(path) {
        return false;
    }
    if !matches!(fs::metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound) {
        return false;
    }
    if !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
    {
        return false;
    }
    for ancestor in path.ancestors().skip(1) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if is_reparse_point(&metadata) => return false,
            Ok(_) => return true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return false,
        }
    }
    false
}

fn is_probeable_external_path(path: &Path) -> bool {
    path.is_absolute() && !path.to_string_lossy().starts_with("\\\\")
}

#[cfg(target_os = "windows")]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(target_os = "windows"))]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_paths_require_a_reachable_ancestor() {
        let directory = tempfile::tempdir().unwrap();
        let present = directory.path().join("present.txt");
        fs::write(&present, b"present").unwrap();
        assert!(path_is_certainly_present(&present));
        assert!(path_is_certainly_missing(
            &directory.path().join("gone.txt")
        ));
        assert!(
            path_is_certainly_missing(&directory.path().join("gone").join("file")),
            "a deleted parent directory is still a confirmed missing path"
        );
        assert!(!path_is_certainly_missing(Path::new("relative-file")));
        assert!(!path_is_certainly_missing(Path::new(
            "\\\\unavailable\\share\\file"
        )));
        assert!(!path_is_certainly_present(Path::new("relative-file")));
    }
}
