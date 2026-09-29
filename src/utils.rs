// Port of Utils.h / Utils.c
//
// XStrdup is gone: nothing needs NUL-terminated copies any more.

pub fn mkdir_if_not_exists(path: &str) -> Result<(), std::io::Error> {
    // mkdir(Path, 0755)
    #[cfg(unix)]
    let rc = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o755).create(path)
    };
    #[cfg(not(unix))]
    let rc = std::fs::create_dir(path);

    match rc {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}
