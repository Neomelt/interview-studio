use std::path::{Path, PathBuf};

use is_audio::LoopbackSource;

use crate::{Error, Result, TITLE_MIC, TITLE_SYS};

#[derive(Debug, Clone)]
pub struct RecordConfig {
    pub mic: String,
    pub loopback: LoopbackSource,
    pub output: PathBuf,
}

// 两个平台的产物必须一致：MKV + 两条 FLAC 轨 + 轨道标题。所以即使 Windows 上
// 采集是自己做的，编码与封装仍然交给同一个 ffmpeg——下游 probe/mix 那条链才
// 不用分平台，产物也才真的一样。
fn encode_args(inputs: &[Input], out: &Path) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for i in inputs {
        a.extend(i.args());
    }
    a.extend(
        [
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-c:a",
            "flac",
            "-sample_fmt",
            "s16",
            "-metadata:s:a:0",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    a.push(format!("title={TITLE_MIC}"));
    a.push("-metadata:s:a:1".into());
    a.push(format!("title={TITLE_SYS}"));
    a.push(out.to_string_lossy().into_owned());
    a
}

// 一路输入：Linux 是 pulse 设备名，Windows 是我们自己录下来的裸 PCM 文件。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Input {
    // 两个变体各只在一个平台上被构造，但两边都要能测「参数拼得对不对」——
    // 「两个平台编码段逐字一致」那条测试正是靠它们比出来的。
    #[cfg_attr(not(unix), allow(dead_code))]
    Pulse(String),
    #[cfg_attr(not(windows), allow(dead_code))]
    RawPcm {
        path: PathBuf,
        rate: u32,
        channels: u16,
    },
}

impl Input {
    fn args(&self) -> Vec<String> {
        match self {
            Self::Pulse(name) => vec!["-f".into(), "pulse".into(), "-i".into(), name.clone()],
            Self::RawPcm {
                path,
                rate,
                channels,
            } => vec![
                "-f".into(),
                "s16le".into(),
                "-ar".into(),
                rate.to_string(),
                "-ac".into(),
                channels.to_string(),
                "-i".into(),
                path.to_string_lossy().into_owned(),
            ],
        }
    }
}

fn verify_output(path: &Path, detail_when_missing: String) -> Result<PathBuf> {
    if !path.exists() {
        return Err(Error::Tool {
            what: "录音".into(),
            detail: detail_when_missing,
        });
    }
    if std::fs::metadata(path)?.len() == 0 {
        return Err(Error::Tool {
            what: "录音".into(),
            detail: "文件是空的".into(),
        });
    }
    Ok(path.to_path_buf())
}

// ---- Linux：ffmpeg 直接采 pulse ----

#[cfg(unix)]
mod backend {
    use std::process::{Child, Stdio};
    use std::time::Duration;

    use super::*;

    // 采集源打不开是启动之后才暴露的，不确认的话用户会对着一个假的录音界面
    const STARTUP_GRACE: Duration = Duration::from_millis(1500);

    pub struct Recorder {
        child: Child,
        path: PathBuf,
    }

    impl Recorder {
        pub fn start(cfg: &RecordConfig) -> Result<Self> {
            let LoopbackSource::PulseMonitor(monitor) = &cfg.loopback else {
                return Err(Error::ToolMissing(format!(
                    "这个平台起不了 {:?} 的录音",
                    cfg.loopback
                )));
            };

            if let Some(dir) = cfg.output.parent() {
                std::fs::create_dir_all(dir)?;
            }

            let args = encode_args(
                &[Input::Pulse(cfg.mic.clone()), Input::Pulse(monitor.clone())],
                &cfg.output,
            );
            let child = crate::tool::command("ffmpeg")
                .args(args)
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
                // Safety: 只是给一个已知 pid 发信号，不解引用任何指针
                unsafe {
                    libc::kill(self.child.id() as libc::pid_t, libc::SIGINT);
                }
            }
            let status = self.child.wait()?;
            verify_output(
                &self.path,
                format!("没有产生文件（ffmpeg 退出码 {status}）"),
            )
        }
    }
}

// ---- Windows：自采 WASAPI，停止时交给 ffmpeg 编码封装 ----
//
// ffmpeg 在 Windows 上没有环回采集设备（只有 dshow，要求驱动暴露 Stereo Mix，
// 现代机器基本没有），所以采集必须自己做。落盘先写裸 PCM 而不是 WAV：没有
// 4GB 头部限制，不需要回头改写文件头，崩了也还是一段可解的音频。

#[cfg(windows)]
mod backend {
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::sync::{Arc, Mutex};

    use is_audio::wasapi::{Capture, CaptureFormat};

    use super::*;

    struct Track {
        capture: Capture,
        path: PathBuf,
        sink: Arc<Mutex<Option<BufWriter<File>>>>,
        write_error: Arc<Mutex<Option<String>>>,
    }

    impl Track {
        fn start(endpoint_id: &str, loopback: bool, path: PathBuf) -> Result<Self> {
            let file = File::create(&path)?;
            let sink = Arc::new(Mutex::new(Some(BufWriter::with_capacity(1 << 20, file))));
            let write_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let (s, e) = (Arc::clone(&sink), Arc::clone(&write_error));

            let capture = Capture::start(endpoint_id, loopback, move |_fmt| {
                let mut bytes: Vec<u8> = Vec::new();
                move |frames: &[i16]| {
                    // 磁盘写满这类错误必须留痕：静默丢样本会得到一段比实际
                    // 短的音频，而界面上一切正常。
                    if e.lock().unwrap().is_some() {
                        return;
                    }
                    bytes.clear();
                    bytes.reserve(frames.len() * 2);
                    for f in frames {
                        bytes.extend_from_slice(&f.to_le_bytes());
                    }
                    if let Some(w) = s.lock().unwrap().as_mut()
                        && let Err(err) = w.write_all(&bytes)
                    {
                        *e.lock().unwrap() = Some(err.to_string());
                    }
                }
            })
            .map_err(|err| Error::Tool {
                what: "WASAPI 采集".into(),
                detail: err.to_string(),
            })?;

            Ok(Self {
                capture,
                path,
                sink,
                write_error,
            })
        }

        fn trouble(&self) -> Option<String> {
            self.capture
                .failure()
                .or_else(|| self.write_error.lock().unwrap().clone())
        }

        fn is_alive(&self) -> bool {
            self.capture.is_running() && self.trouble().is_none()
        }

        // 先停采集线程（Drop 里 join），再冲盘，顺序反了会丢掉最后一段
        fn finish(self) -> Result<(PathBuf, CaptureFormat)> {
            let fmt = self.capture.format();
            let trouble = self.trouble();
            drop(self.capture);
            if let Some(w) = self.sink.lock().unwrap().take() {
                w.into_inner()
                    .map_err(|e| Error::Io(e.into()))?
                    .sync_all()?;
            }
            match trouble {
                Some(detail) => Err(Error::Tool {
                    what: "录音".into(),
                    detail,
                }),
                None => Ok((self.path, fmt)),
            }
        }
    }

    pub struct Recorder {
        mic: Option<Track>,
        sys: Option<Track>,
        scratch: PathBuf,
        path: PathBuf,
    }

    impl Recorder {
        pub fn start(cfg: &RecordConfig) -> Result<Self> {
            let LoopbackSource::WasapiLoopback(sink_id) = &cfg.loopback else {
                return Err(Error::ToolMissing(format!(
                    "这个平台起不了 {:?} 的录音",
                    cfg.loopback
                )));
            };

            let dir = cfg.output.parent().ok_or_else(|| Error::Tool {
                what: "录音".into(),
                detail: format!("输出路径没有目录部分: {}", cfg.output.display()),
            })?;
            std::fs::create_dir_all(dir)?;

            // 临时文件放在最终输出的同一个目录：跨卷改名会失败，而且用户看到的
            // 剩余空间就是这一处的空间。
            let scratch = dir.join(format!(".is-rec-{}", std::process::id()));
            std::fs::create_dir_all(&scratch)?;

            let mic = Track::start(&cfg.mic, false, scratch.join("mic.pcm"))?;
            let sys = Track::start(sink_id, true, scratch.join("sys.pcm"))?;

            Ok(Self {
                mic: Some(mic),
                sys: Some(sys),
                scratch,
                path: cfg.output.clone(),
            })
        }

        pub fn is_alive(&mut self) -> bool {
            self.mic.as_ref().is_some_and(Track::is_alive)
                && self.sys.as_ref().is_some_and(Track::is_alive)
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        pub fn stop(mut self) -> Result<PathBuf> {
            let mic = self.mic.take().expect("stop 只会被调用一次").finish();
            let sys = self.sys.take().expect("stop 只会被调用一次").finish();
            let out = self.encode(mic?, sys?);
            let _ = std::fs::remove_dir_all(&self.scratch);
            out
        }

        fn encode(
            &self,
            (mic_path, mic_fmt): (PathBuf, CaptureFormat),
            (sys_path, sys_fmt): (PathBuf, CaptureFormat),
        ) -> Result<PathBuf> {
            let inputs = [
                Input::RawPcm {
                    path: mic_path,
                    rate: mic_fmt.rate,
                    channels: mic_fmt.channels,
                },
                Input::RawPcm {
                    path: sys_path,
                    rate: sys_fmt.rate,
                    channels: sys_fmt.channels,
                },
            ];
            let mut args = encode_args(&inputs, &self.path);
            // 覆盖：走到这里说明是我们自己刚生成的临时目录之外的新文件名，
            // 但同一秒内重录会撞名，明确覆盖比让 ffmpeg 卡在交互提问上强。
            args.insert(0, "-y".into());

            let status = crate::tool::command("ffmpeg")
                .args(args)
                .stdin(std::process::Stdio::null())
                .status()
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Error::ToolMissing("ffmpeg".into())
                    } else {
                        Error::Io(e)
                    }
                })?;
            if !status.success() {
                return Err(Error::Tool {
                    what: "ffmpeg 编码".into(),
                    detail: format!("退出码 {status}"),
                });
            }
            verify_output(&self.path, "ffmpeg 没有产生文件".into())
        }
    }

    impl Drop for Recorder {
        fn drop(&mut self) {
            // stop() 会自己清理；这里兜住「没调 stop 就丢掉」的路径，
            // 免得留下几百 MB 的裸 PCM。
            if self.mic.is_some() || self.sys.is_some() {
                self.mic.take();
                self.sys.take();
                let _ = std::fs::remove_dir_all(&self.scratch);
            }
        }
    }
}

// 不实现 Drop 自动停止：录音是有价值的数据，不该因变量离开作用域被隐式结束。
// 装箱是为了两个平台上的大小一致——Windows 那个后端要拿着两路采集的状态，
// 直接内联会让调用方的状态机枚举被它撑大。
pub struct Recording(Box<backend::Recorder>);

impl Recording {
    pub fn start(cfg: &RecordConfig) -> Result<Self> {
        Ok(Self(Box::new(backend::Recorder::start(cfg)?)))
    }

    pub fn is_alive(&mut self) -> bool {
        self.0.is_alive()
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }

    pub fn stop(self) -> Result<PathBuf> {
        self.0.stop()
    }
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("path", &self.path())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out() -> PathBuf {
        std::env::temp_dir().join("is-args.mkv")
    }

    #[test]
    fn args_map_both_inputs_to_separate_tracks() {
        let a = encode_args(
            &[
                Input::Pulse("mic_src".into()),
                Input::Pulse("sink.monitor".into()),
            ],
            &out(),
        );
        let joined = a.join(" ");
        assert!(joined.contains("-i mic_src"));
        assert!(joined.contains("-i sink.monitor"));
        assert_eq!(a.iter().filter(|s| *s == "-map").count(), 2);
        assert!(joined.contains("0:a"));
        assert!(joined.contains("1:a"));
    }

    #[test]
    fn tracks_are_titled_so_players_and_tools_can_tell_them_apart() {
        let joined = encode_args(
            &[Input::Pulse("m".into()), Input::Pulse("s".into())],
            &out(),
        )
        .join(" ");
        assert!(joined.contains(TITLE_MIC));
        assert!(joined.contains(TITLE_SYS));
    }

    // 两个平台的编码段必须逐字一致，否则产物不可能真的一样。
    #[test]
    fn both_platforms_encode_identically_after_the_inputs() {
        let pulse = encode_args(
            &[Input::Pulse("m".into()), Input::Pulse("s".into())],
            &out(),
        );
        let raw = encode_args(
            &[
                Input::RawPcm {
                    path: "m.pcm".into(),
                    rate: 48_000,
                    channels: 2,
                },
                Input::RawPcm {
                    path: "s.pcm".into(),
                    rate: 44_100,
                    channels: 1,
                },
            ],
            &out(),
        );
        let tail = |v: &[String]| {
            let i = v.iter().position(|s| s == "-map").unwrap();
            v[i..].to_vec()
        };
        assert_eq!(tail(&pulse), tail(&raw));
    }

    #[test]
    fn raw_pcm_inputs_carry_their_own_rate_and_channels() {
        let joined = Input::RawPcm {
            path: "x.pcm".into(),
            rate: 44_100,
            channels: 1,
        }
        .args()
        .join(" ");
        assert!(joined.contains("-f s16le"), "{joined}");
        assert!(joined.contains("-ar 44100"), "{joined}");
        assert!(joined.contains("-ac 1"), "{joined}");
    }

    // 另一个平台的 loopback 变体要被明确拒绝，而且不能留下半个文件
    #[test]
    fn foreign_loopback_variant_is_rejected_without_side_effects() {
        let out = std::env::temp_dir().join("is-should-not-exist.mkv");
        #[cfg(unix)]
        let foreign = LoopbackSource::WasapiLoopback("dev".into());
        #[cfg(windows)]
        let foreign = LoopbackSource::PulseMonitor("x.monitor".into());

        let cfg = RecordConfig {
            mic: "m".into(),
            loopback: foreign,
            output: out.clone(),
        };
        match Recording::start(&cfg) {
            Err(Error::ToolMissing(m)) => assert!(!m.is_empty(), "错误要说得出原因"),
            other => panic!("应当明确拒绝: {other:?}"),
        }
        assert!(!out.exists(), "{out:?}");
    }
}

#[cfg(all(test, unix))]
mod live_tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    // 会录 3 秒真实音频（跑完即删）：
    //   cargo test -p is-pipeline -- --ignored --nocapture
    // 单元测试只覆盖参数构造，录音->停止->混音这条链只有真机能验。
    #[test]
    #[ignore = "会录真实音频"]
    fn records_stops_and_mixes_on_real_devices() {
        use crate::{TITLE_MIX, mix_in_place, probe};

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
