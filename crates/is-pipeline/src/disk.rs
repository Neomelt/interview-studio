use std::path::Path;

// 目标目录可能还没建，往上找一个存在的
fn existing_ancestor(path: &Path) -> Option<&Path> {
    let mut p = path;
    loop {
        if p.exists() {
            return Some(p);
        }
        p = p.parent()?;
    }
}

// 用 statvfs 而不是调 df：录音界面每秒都要刷
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let p = existing_ancestor(path)?;

    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // Safety: c 是有效的 NUL 结尾路径，st 是栈上结构
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    // 这两个字段在我们支持的目标上都是 u64。若将来加 32 位目标，
    // 这里需要显式加宽，否则大磁盘会溢出。
    Some(st.f_bavail * st.f_frsize)
}

// 取 lpFreeBytesAvailableToCaller 而不是 lpTotalNumberOfFreeBytes：前者扣掉了
// 磁盘配额，是当前用户真正写得进去的量，语义对应 Unix 的 f_bavail。
#[cfg(windows)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    let p = existing_ancestor(path)?;
    let mut wide: Vec<u16> = p.as_os_str().encode_wide().collect();
    wide.push(0);

    let mut avail: u64 = 0;
    // Safety: wide 是有效的 NUL 结尾宽字符串，avail 在栈上；后两个出参传 null
    // 表示不需要，这是该 API 允许的。
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut avail,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(avail)
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_on_a_real_path() {
        let tmp = std::env::temp_dir();
        let n = free_bytes(&tmp).unwrap_or_else(|| panic!("{tmp:?} 应当能读到"));
        assert!(n > 0);
    }

    #[test]
    fn walks_up_to_an_existing_parent() {
        let p = std::env::temp_dir().join("is-nope-a/is-nope-b/rec.mkv");
        assert!(free_bytes(&p).is_some());
    }

    // 不存在的绝对路径要能靠祖先解析出来。Windows 上根是盘符，直接拼 "/xxx"
    // 会落到当前盘的根，语义不同，所以两边各取各的根。
    #[test]
    fn absolute_path_resolves_via_existing_ancestor() {
        let root = if cfg!(windows) {
            std::env::temp_dir()
                .ancestors()
                .last()
                .unwrap()
                .to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        };
        let p = root.join("is-definitely-not-here-xyz");
        assert!(free_bytes(&p).is_some(), "{p:?}");
    }

    #[test]
    fn relative_path_without_ancestor_returns_none() {
        assert!(free_bytes(Path::new("relative-nope")).is_none());
        assert!(free_bytes(Path::new("")).is_none());
    }

    #[test]
    fn human_readable_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
        assert!(human_bytes(40 * 1024u64.pow(3)).ends_with("GB"));
    }
}
