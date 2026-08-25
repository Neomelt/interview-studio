use std::path::PathBuf;

use chrono::{DateTime, Local};

const SUBDIR: &str = "InterviewStudio";

pub fn recordings_dir() -> PathBuf {
    music_dir().join(SUBDIR)
}

// xdg-user-dir 会跟着用户的语言设置走（可能是 ~/音乐 而不是 ~/Music），
// 所以优先问它，问不到再退回 $HOME/Music
#[cfg(unix)]
fn music_dir() -> PathBuf {
    if let Some(p) = xdg_music().filter(|p| p.is_absolute()) {
        return p;
    }
    home().join("Music")
}

#[cfg(unix)]
fn xdg_music() -> Option<PathBuf> {
    let out = std::process::Command::new("xdg-user-dir")
        .arg("MUSIC")
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

// 同理：Windows 的「音乐」是 known folder，用户可以把它搬到别的盘，
// 拼 %USERPROFILE%\Music 会写到一个用户根本不看的地方。
#[cfg(windows)]
fn music_dir() -> PathBuf {
    if let Some(p) = known_folder_music().filter(|p| p.is_absolute()) {
        return p;
    }
    home().join("Music")
}

#[cfg(windows)]
fn known_folder_music() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Music, SHGetKnownFolderPath};

    let mut raw = std::ptr::null_mut();
    // Safety: rfid 指向静态 GUID，ppszpath 是栈上出参。成功时 raw 指向一枚 COM
    // 分配的 NUL 结尾宽字符串，读完立刻 CoTaskMemFree，符合该 API 的所有权约定。
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_Music, 0, std::ptr::null_mut(), &mut raw) };
    if hr < 0 || raw.is_null() {
        return None;
    }
    let mut len = 0usize;
    // Safety: 上面确认 raw 非空且 API 保证以 NUL 结尾
    while unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    // Safety: len 是刚数出来的、不含结尾 NUL 的长度
    let s = std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(raw, len) });
    // Safety: raw 由 SHGetKnownFolderPath 用 COM 分配器分配，此处正好释放一次
    unsafe { CoTaskMemFree(raw.cast()) };
    Some(PathBuf::from(s))
}

fn home() -> PathBuf {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(key).map(PathBuf::from).unwrap_or_default()
}

pub fn filename_for_now() -> String {
    filename_at(Local::now())
}

fn filename_at(t: DateTime<Local>) -> String {
    format!("interview_{}.mkv", t.format("%Y-%m-%d_%H%M%S"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn recordings_live_under_the_music_directory() {
        let d = recordings_dir();
        assert!(d.is_absolute(), "{d:?}");
        assert_eq!(d.file_name().unwrap(), SUBDIR);
        assert_eq!(d.parent().unwrap(), music_dir());
    }

    // 上面那条在 known folder 查询失败时也会通过——会退回 %USERPROFILE%\Music，
    // 一样是绝对路径。区分不出这段 unsafe FFI 到底有没有在干活，单独断言一次。
    #[cfg(windows)]
    #[test]
    fn known_folder_lookup_actually_works() {
        let p = known_folder_music().expect("SHGetKnownFolderPath(FOLDERID_Music) 应当成功");
        assert!(p.is_absolute(), "{p:?}");
        eprintln!("known folder Music = {}", p.display());
    }

    #[test]
    fn filename_is_timestamped_mkv() {
        let n = filename_for_now();
        assert!(n.starts_with("interview_"), "{n}");
        assert!(n.ends_with(".mkv"), "{n}");
        // interview_YYYY-MM-DD_HHMMSS.mkv
        assert_eq!(n.len(), "interview_2026-08-25_010203.mkv".len(), "{n}");
    }

    #[test]
    fn timestamp_is_local_time_not_utc() {
        // 用 UTC 命名会让 UTC+8 的用户看到一个对不上的时间。
        let t = Local::now();
        assert!(filename_at(t).contains(&t.format("_%H%M%S").to_string()));
    }

    #[test]
    fn two_recordings_in_the_same_second_would_collide() {
        // 已知取舍：秒级时间戳。手速再快也点不出两次，真撞了 ffmpeg 会拒绝覆盖。
        // 拿固定时刻断言，避免两次 now() 跨秒边界时假失败。
        let t = Local.with_ymd_and_hms(2026, 8, 25, 1, 2, 3).unwrap();
        assert_eq!(filename_at(t), "interview_2026-08-25_010203.mkv");
        assert_eq!(
            filename_at(t),
            filename_at(t + chrono::TimeDelta::milliseconds(999))
        );
    }
}
