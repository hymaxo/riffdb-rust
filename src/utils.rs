// Port of Utils.h / Utils.c

use libc::c_char;

use crate::xmalloc::xmalloc;

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

pub unsafe fn xstrdup(str: *const c_char) -> *mut c_char {
    let len = libc::strlen(str) + 1;
    let copy = xmalloc(len) as *mut c_char;
    std::ptr::copy_nonoverlapping(str, copy, len);
    copy
}
