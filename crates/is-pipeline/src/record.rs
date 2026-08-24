use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use is_audio::LoopbackSource;

use crate::{Error, Result, TITLE_MIC, TITLE_SYS};

// 采集源打不开是启动之后才暴露的，不确认的话用户会对着一个假的录音界面
const STARTUP_GRACE: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub struct RecordConfig {
    pub mic: String,
    pub loopback: LoopbackSource,
    pub output: PathBuf,
}

// 不实现 Drop 自动停止：录音是有价值的数据，不该因变量离开作用域被隐式结束
pub struct Recording {
    child: Child,
    path: PathBuf,
}

impl Recording {
    pub fn start(cfg: &RecordConfig) -> Result<Self> {
        let loopback = match &cfg.loopback {
            LoopbackSource::PulseMonitor(name) => name.clone(),
            LoopbackSource::WasapiLoopback(_) => {
                return Err(Error::ToolMissing("Windows 录音后端尚未实现".into()));
            }
        };

        if let Some(dir) = cfg.output.parent() {
            std::fs::create_dir_all(dir)?;
        }

        let child = Command::new("ffmpeg")
            .args(build_args(&cfg.mic, &loopback, &cfg.output))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::ToolMissing("ffmpeg".into())
                } else {
                    Error::Io(e)
                }
            })?;

        let mut rec = Self {
            child,
            path: cfg.output.clone(),
        };
        std::thread::sleep(STARTUP_GRACE);
        if !rec.is_alive() {
            return Err(Error::Tool {
                what: "ffmpeg 启动".into(),
                detail: "进程立刻退出了，多半是采集源打不开".into(),
            });
        }
        Ok(rec)
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // 必须 SIGINT 而非 kill：ffmpeg 收到 SIGINT 才写完容器尾，SIGKILL 留下损坏文件
    pub fn stop(mut self) -> Result<PathBuf> {
        if self.is_alive() {
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGINT);
            }
        }
        let status = self.child.wait()?;

        if !self.path.exists() {
            return Err(Error::Tool {
                what: "录音".into(),
                detail: format!("没有产生文件（ffmpeg 退出码 {status}）"),
            });
        }
        if std::fs::metadata(&self.path)?.len() == 0 {
            return Err(Error::Tool {
                what: "录音".into(),
                detail: "文件是空的".into(),
            });
        }
        Ok(self.path.clone())
    }
}

fn build_args(mic: &str, loopback: &str, out: &Path) -> Vec<String> {
    [
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "pulse",
        "-i",
        mic,
        "-f",
        "pulse",
        "-i",
        loopback,
        "-map",
        "0:a",
        "-map",
        "1:a",
        "-c:a",
        "flac",
        "-sample_fmt",
        "s16",
        "-metadata:s:a:0",
        &format!("title={TITLE_MIC}"),
        "-metadata:s:a:1",
        &format!("title={TITLE_SYS}"),
        &out.to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_map_both_inputs_to_separate_tracks() {
        let a = build_args("mic_src", "sink.monitor", Path::new("/tmp/x.mkv"));
        let joined = a.join(" ");
        assert!(joined.contains("-i mic_src"));
        assert!(joined.contains("-i sink.monitor"));
        assert_eq!(a.iter().filter(|s| *s == "-map").count(), 2);
        assert!(joined.contains("0:a"));
        assert!(joined.contains("1:a"));
    }

    #[test]
    fn tracks_are_titled_so_players_and_tools_can_tell_them_apart() {
        let joined = build_args("m", "s", Path::new("/tmp/x.mkv")).join(" ");
        assert!(joined.contains(TITLE_MIC));
        assert!(joined.contains(TITLE_SYS));
    }

    #[test]
    fn windows_backend_is_rejected_with_a_clear_message() {
        let cfg = RecordConfig {
            mic: "m".into(),
            loopback: LoopbackSource::WasapiLoopback("dev".into()),
            output: "/tmp/should-not-exist.mkv".into(),
        };
        match Recording::start(&cfg) {
            Err(Error::ToolMissing(m)) => assert!(m.contains("Windows"), "{m}"),
            other => panic!("应当明确拒绝: {other:?}"),
        }
        assert!(!Path::new("/tmp/should-not-exist.mkv").exists());
    }
}
