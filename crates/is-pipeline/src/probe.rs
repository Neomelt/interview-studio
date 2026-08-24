//! ffprobe / ffmpeg 探测：轨数、时长、标题、电平。

use std::path::Path;
use std::process::Command;

use crate::{Error, Result};

/// 一条音轨的电平。低于 [`Levels::SILENT_DB`] 基本等于没录到东西。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub mean_db: f32,
    pub peak_db: f32,
}

impl Levels {
    /// volumedetect 对全静音返回 -91 dB，留点余量当阈值。
    pub const SILENT_DB: f32 = -80.0;
    /// 低于这个值虽然有信号，但小到基本听不清。
    pub const WEAK_DB: f32 = -50.0;

    pub fn is_silent(&self) -> bool {
        self.mean_db < Self::SILENT_DB
    }

    pub fn is_weak(&self) -> bool {
        !self.is_silent() && self.mean_db < Self::WEAK_DB
    }

    /// 峰值撞到满刻度就是削顶了。
    pub fn is_clipping(&self) -> bool {
        self.peak_db >= -0.05
    }
}

fn run(bin: &str, args: &[String]) -> Result<std::process::Output> {
    Command::new(bin).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ToolMissing(bin.to_string())
        } else {
            Error::Io(e)
        }
    })
}

fn ffprobe(args: &[&str]) -> Result<String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = run("ffprobe", &args)?;
    if !out.status.success() {
        return Err(Error::Tool {
            what: "ffprobe".into(),
            detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 文件里有几条音轨。
pub fn audio_track_count(path: &Path) -> Result<usize> {
    let out = ffprobe(&[
        "-v",
        "error",
        "-select_streams",
        "a",
        "-show_entries",
        "stream=index",
        "-of",
        "csv=p=0",
        &path.to_string_lossy(),
    ])?;
    Ok(out.lines().filter(|l| !l.trim().is_empty()).count())
}

/// 总时长（秒）。
pub fn duration_secs(path: &Path) -> Result<f64> {
    let out = ffprobe(&[
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=nk=1:nw=1",
        &path.to_string_lossy(),
    ])?;
    out.trim()
        .parse()
        .map_err(|_| Error::Parse(format!("读不出时长: {out:?}")))
}

/// 各音轨的标题，顺序与轨号一致。没有标题的位置是空串。
pub fn track_titles(path: &Path) -> Result<Vec<String>> {
    let out = ffprobe(&[
        "-v",
        "error",
        "-select_streams",
        "a",
        "-show_entries",
        "stream_tags=title",
        "-of",
        "default=nk=1:nw=1",
        &path.to_string_lossy(),
    ])?;
    Ok(out.lines().map(|l| l.trim().to_string()).collect())
}

/// 哪几条轨带 default 标志。播放器默认播的就是第一条带标志的。
pub fn default_track_flags(path: &Path) -> Result<Vec<bool>> {
    let out = ffprobe(&[
        "-v",
        "error",
        "-select_streams",
        "a",
        "-show_entries",
        "stream_disposition=default",
        "-of",
        "default=nk=1:nw=1",
        &path.to_string_lossy(),
    ])?;
    Ok(out.lines().map(|l| l.trim() == "1").collect())
}

/// 量一条音轨的电平。
///
/// 注意不能给 ffmpeg 加 `-v error`：volumedetect 的结果是 info 级日志，
/// 压掉日志级别就什么都读不到了（这个坑踩过）。
pub fn track_levels(path: &Path, track: usize) -> Result<Levels> {
    let args: Vec<String> = [
        "-hide_banner",
        "-nostdin",
        "-i",
        &path.to_string_lossy(),
        "-map",
        &format!("0:a:{track}"),
        "-af",
        "volumedetect",
        "-f",
        "null",
        "-",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let out = run("ffmpeg", &args)?;
    let log = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(Error::Tool {
            what: format!("ffmpeg volumedetect (轨 {track})"),
            detail: log.lines().last().unwrap_or("").to_string(),
        });
    }
    Ok(Levels {
        mean_db: grab_db(&log, "mean_volume:")?,
        peak_db: grab_db(&log, "max_volume:")?,
    })
}

fn grab_db(log: &str, key: &str) -> Result<f32> {
    log.lines()
        .find_map(|l| l.split(key).nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Error::Parse(format!("日志里没有 {key}")))
}

/// 解码后音频数据的 MD5。用来证明「无损复制」不是嘴上说说——
/// 比文件大小或码率靠谱得多，它是逐采样的。
pub fn track_audio_md5(path: &Path, track: usize) -> Result<String> {
    let args: Vec<String> = [
        "-v",
        "error",
        "-nostdin",
        "-i",
        &path.to_string_lossy(),
        "-map",
        &format!("0:a:{track}"),
        "-f",
        "md5",
        "-",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let out = run("ffmpeg", &args)?;
    if !out.status.success() {
        return Err(Error::Tool {
            what: format!("ffmpeg md5 (轨 {track})"),
            detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .strip_prefix("MD5=")
        .map(str::to_string)
        .ok_or_else(|| Error::Parse("ffmpeg 没有输出 MD5=".into()))
}

/// ffmpeg 和 ffprobe 都在不在。
pub fn tools_available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|b| {
        Command::new(b)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}
