/// Creates `path` (mode 0755 on Unix) unless it already exists.
pub fn mkdir_if_not_exists(path: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o755).create(path)
    };
    #[cfg(not(unix))]
    let result = std::fs::create_dir(path);

    match result {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}
