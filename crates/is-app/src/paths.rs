use std::path::PathBuf;

const SUBDIR: &str = "InterviewStudio";

pub fn recordings_dir() -> PathBuf {
    music_dir().join(SUBDIR)
}

// xdg-user-dir 会跟着用户的语言设置走（可能是 ~/音乐 而不是 ~/Music），
// 所以优先问它，问不到再退回 $HOME/Music
fn music_dir() -> PathBuf {
    if let Some(p) = xdg_music().filter(|p| p.is_absolute()) {
        return p;
    }
    home().join("Music")
}

fn xdg_music() -> Option<PathBuf> {
    let out = std::process::Command::new("xdg-user-dir")
        .arg("MUSIC")
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn filename_for_now() -> String {
    let stamp = std::process::Command::new("date")
        .arg("+%Y-%m-%d_%H%M%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    format!("interview_{stamp}.mkv")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recordings_live_under_the_music_directory() {
        let d = recordings_dir();
        assert!(d.is_absolute(), "{d:?}");
        assert_eq!(d.file_name().unwrap(), SUBDIR);
        assert_eq!(d.parent().unwrap(), music_dir());
    }

    #[test]
    fn filename_is_timestamped_mkv() {
        let n = filename_for_now();
        assert!(n.starts_with("interview_"), "{n}");
        assert!(n.ends_with(".mkv"), "{n}");
        assert_ne!(n, "interview_unknown.mkv", "date 没跑起来");
        // interview_YYYY-MM-DD_HHMMSS.mkv
        assert_eq!(n.len(), "interview_2026-08-25_010203.mkv".len(), "{n}");
    }

    #[test]
    fn two_recordings_in_the_same_second_would_collide() {
        // 已知取舍：秒级时间戳。手速再快也点不出两次，真撞了 ffmpeg 会拒绝覆盖。
        assert_eq!(filename_for_now(), filename_for_now());
    }
}
