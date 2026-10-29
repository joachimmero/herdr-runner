use std::path::{Path, PathBuf};

/// The nearest directory at or above `start` that holds a Unity project.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join("Assets").is_dir() && dir.join("ProjectSettings").is_dir())
        .map(Path::to_path_buf)
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_project_above_a_nested_directory() {
        let root =
            std::env::temp_dir().join(format!("herdr-runner-project-{}", std::process::id()));
        let nested = root.join("Assets/Source/Tests");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(root.join("ProjectSettings")).unwrap();
        assert_eq!(find_root(&nested), Some(root.clone()));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
