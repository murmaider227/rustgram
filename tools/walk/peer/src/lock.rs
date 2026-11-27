//! One login = one running process. Every login directory gets a `login.lock` file that the
//! process using it keeps open with no sharing; a second process (this tool, the walk runner, or
//! another agent's copy of the tool) cannot open it and refuses to use the login. Windows drops
//! the lock when the process ends, so a crash leaves no stale lock.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

pub struct LoginLock {
    _file: File,
}

pub fn acquire(dir: &Path) -> Result<LoginLock, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join("login.lock");
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.share_mode(0);
    }
    let mut file = opts.open(&path).map_err(|_| {
        format!(
            "login dir {} is in use by another process (login.lock is held); one login must never run twice at once",
            dir.display()
        )
    })?;
    let _ = writeln!(file, "{}", std::process::id());
    Ok(LoginLock { _file: file })
}
