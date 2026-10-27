//! In Docker, Loski starts as root just long enough to hand its data folder
//! to its own user, then carries on as that user. So a data folder Docker
//! created as root (plain `docker compose up` on Linux) still works, and
//! `docker exec loski loski selftest` can't leave root-owned files behind.
//!
//! The user is LOSKI_UID / LOSKI_GID: 10001 (the image's `loski`) unless
//! loski.sh says it's you.

use std::{
    os::unix::fs::{MetadataExt, lchown},
    path::Path,
};

/// From root to Loski's own user. Does nothing when not started as root.
pub fn become_loski(data_dir: &Path) {
    // SAFETY: no threads exist yet; these are plain libc calls.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let id = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(10001)
    };
    let (uid, gid) = (id("LOSKI_UID"), id("LOSKI_GID"));
    if uid == 0 {
        // Asked to stay root.
        return;
    }
    let _ = std::fs::create_dir_all(data_dir);
    hand_over(data_dir, uid, gid);
    // SAFETY: still single-threaded; groups first, then the group, then the user.
    let switched = unsafe {
        libc::setgroups(0, std::ptr::null()) == 0
            && libc::setgid(gid) == 0
            && libc::setuid(uid) == 0
    };
    if !switched {
        eprintln!(
            "Loski couldn't switch to user {uid} ({}), so it runs as root.",
            std::io::Error::last_os_error()
        );
    }
}

/// Gives everything in `path` not already theirs to `uid`. Folders on
/// Windows or macOS through Docker Desktop may refuse, and work anyway.
/// Links are handed over themselves, never what they point at, so a link
/// in the folder can't give root's files away.
fn hand_over(path: &Path, uid: u32, gid: u32) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.uid() != uid || meta.gid() != gid {
        let _ = lchown(path, Some(uid), Some(gid));
    }
    if meta.is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.flatten() {
            hand_over(&entry.path(), uid, gid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_over_a_whole_folder() {
        // Not root here, so chown to ourselves: it must walk without failing.
        let dir = std::env::temp_dir().join(format!("loski-user-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs/a.log"), b"x").unwrap();
        let meta = std::fs::metadata(&dir).unwrap();
        hand_over(&dir, meta.uid(), meta.gid());
        assert_eq!(
            std::fs::metadata(dir.join("logs/a.log")).unwrap().uid(),
            meta.uid()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn leaves_what_links_point_at() {
        // Only root can give files away, as in the dev container.
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let base = std::env::temp_dir().join(format!("loski-link-{}", std::process::id()));
        let (dir, outside) = (base.join("data"), base.join("secret"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&outside, b"x").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        hand_over(&dir, 12345, 12345);
        assert_eq!(std::fs::metadata(&outside).unwrap().uid(), 0);
        let link = std::fs::symlink_metadata(dir.join("link")).unwrap();
        assert_eq!(link.uid(), 12345);
        let _ = std::fs::remove_dir_all(base);
    }
}
