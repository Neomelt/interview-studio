use std::path::Path;

// 用 statvfs 而不是调 df：录音界面每秒都要刷
pub fn free_bytes(path: &Path) -> Option<u64> {
    // 目标目录可能还没建，往上找一个存在的
    let mut p = path;
    loop {
        if p.exists() {
            break;
        }
        p = p.parent()?;
    }

    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // Safety: c 是有效的 NUL 结尾路径，st 是栈上结构
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
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
        let n = free_bytes(Path::new("/tmp")).expect("/tmp 应当能读到");
        assert!(n > 0);
    }

    #[test]
    fn walks_up_to_an_existing_parent() {
        let p = std::env::temp_dir().join("is-nope-a/is-nope-b/rec.mkv");
        assert!(free_bytes(&p).is_some());
    }

    #[test]
    fn absolute_path_resolves_via_existing_ancestor() {
        assert!(free_bytes(Path::new("/is-definitely-not-here-xyz")).is_some());
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
