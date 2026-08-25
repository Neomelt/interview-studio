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

    pub fn stop(mut self) -> Result<PathBuf> {
        if self.is_alive() {
            request_graceful_stop(&mut self.child);
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

// 必须 SIGINT 而非 kill：ffmpeg 收到 SIGINT 才写完容器尾，SIGKILL 留下损坏文件
#[cfg(unix)]
fn request_graceful_stop(child: &mut Child) {
    // Safety: 只是给一个已知 pid 发信号，不解引用任何指针
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
}

// Windows 没有能发给子进程的 SIGINT，ffmpeg 又是以 stdin=null 起的，也没法写
// 'q' 让它自己收尾。这条路径在 Windows 上不该被走到：ffmpeg 在 Windows 上没有
// loopback 采集设备，录音由原生 WASAPI 后端负责，不起 ffmpeg 子进程。真被走到
// 了，kill 会留下尾部不完整的容器——但比让 wait() 永远挂住强。
#[cfg(windows)]
fn request_graceful_stop(child: &mut Child) {
    let _ = child.kill();
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
        let out = std::env::temp_dir().join("is-should-not-exist.mkv");
        let cfg = RecordConfig {
            mic: "m".into(),
            loopback: LoopbackSource::WasapiLoopback("dev".into()),
            output: out.clone(),
        };
        match Recording::start(&cfg) {
            Err(Error::ToolMissing(m)) => assert!(m.contains("Windows"), "{m}"),
            other => panic!("应当明确拒绝: {other:?}"),
        }
        // 拒绝要发生在建目录/起进程之前，不能留下半个文件
        assert!(!out.exists(), "{out:?}");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::{TITLE_MIX, mix_in_place, probe};

    // 会录 3 秒真实音频（跑完即删）：
    //   cargo test -p is-pipeline -- --ignored --nocapture
    // 单元测试只覆盖参数构造，录音->停止->混音这条链只有真机能验。
    #[test]
    #[ignore = "会录真实音频"]
    fn records_stops_and_mixes_on_real_devices() {
        let Ok(info) = Command::new("pactl").arg("info").output() else {
            return;
        };
        let text = String::from_utf8_lossy(&info.stdout);
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()))
        };
        let (Some(mic), Some(sink)) = (field("Default Source:"), field("Default Sink:")) else {
            return;
        };

        let dir = std::env::temp_dir().join(format!("is-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.mkv");

        let rec = Recording::start(&RecordConfig {
            mic,
            loopback: LoopbackSource::PulseMonitor(format!("{sink}.monitor")),
            output: out.clone(),
        })
        .expect("起录音");
        std::thread::sleep(Duration::from_secs(3));

        let path = rec.stop().expect("停止");
        assert_eq!(
            probe::audio_track_count(&path).unwrap(),
            2,
            "停止后应当是双轨"
        );
        let dur = probe::duration_secs(&path).unwrap();
        assert!(dur > 1.5, "录了 3 秒却只有 {dur:.1} 秒");

        let report = mix_in_place(&path).expect("混音");
        assert_eq!(probe::audio_track_count(&path).unwrap(), 3);
        assert_eq!(probe::track_titles(&path).unwrap()[0], TITLE_MIX);
        assert_eq!(
            probe::default_track_flags(&path).unwrap(),
            vec![true, false, false]
        );
        assert!(
            report.mix_peak_db <= -0.5,
            "混音削顶了: {}",
            report.mix_peak_db
        );

        eprintln!(
            "端到端通过：{dur:.1}s，混音峰值 {:.1} dB",
            report.mix_peak_db
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
