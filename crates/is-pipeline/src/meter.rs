// 电平表和录音是两条独立的采集：同进程方案里电平走 FIFO，读端一消失 ffmpeg
// 就阻塞在写入上，进程还活着但录音停了。两个平台都已确认一个源可被多个客户端
// 并发采集（Linux 是多进程读 monitor，Windows 是共享模式多客户端）。

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use is_audio::LoopbackSource;

use crate::Result;

// parec 会替我们重采样到这个速率；Windows 那边直接用设备速率，不需要这两个常量。
#[cfg(unix)]
const RATE: u32 = 16_000;
#[cfg(unix)]
const WINDOW_SAMPLES: usize = (RATE as usize) / 10;

pub const FLOOR_DB: f32 = -90.0;

pub fn rms_dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return FLOOR_DB;
    }
    // f64 累加：i16 平方和在长窗口下会溢出 i32
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    let rms = (sum / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return FLOOR_DB;
    }
    let db = 20.0 * (rms / f64::from(i16::MAX)).log10();
    (db as f32).max(FLOOR_DB)
}

// 原子变量存不了 f32，用 dB×100 的定点整数
fn db_to_raw(db: f32) -> i32 {
    (db * 100.0) as i32
}

fn raw_to_db(raw: i32) -> f32 {
    raw as f32 / 100.0
}

// ---- Linux：parec 子进程 ----

#[cfg(unix)]
mod backend {
    use std::io::Read;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::Error;

    pub struct Meter {
        child: Child,
        level: Arc<AtomicI32>,
        stop: Arc<AtomicBool>,
    }

    impl Meter {
        pub fn open(source: &str) -> Result<Self> {
            let mut child = Command::new("parec")
                .args([
                    "--rate",
                    &RATE.to_string(),
                    "--channels=1",
                    "--format=s16le",
                    "--raw",
                    "--latency-msec=50",
                    "--device",
                    source,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Error::ToolMissing("parec（在 pulseaudio-utils 包里）".into())
                    } else {
                        Error::Io(e)
                    }
                })?;

            let mut stdout = child.stdout.take().ok_or_else(|| Error::Tool {
                what: "parec".into(),
                detail: "拿不到 stdout".into(),
            })?;

            let level = Arc::new(AtomicI32::new(db_to_raw(FLOOR_DB)));
            let stop = Arc::new(AtomicBool::new(false));
            let (l, s) = (Arc::clone(&level), Arc::clone(&stop));

            std::thread::Builder::new()
                .name(format!("meter:{source}"))
                .spawn(move || {
                    let mut buf = vec![0u8; WINDOW_SAMPLES * 2];
                    let mut samples = vec![0i16; WINDOW_SAMPLES];
                    while !s.load(Ordering::Relaxed) {
                        // 不足一整窗就不算，避免短读导致电平乱跳
                        if stdout.read_exact(&mut buf).is_err() {
                            break;
                        }
                        for (i, c) in buf.as_chunks::<2>().0.iter().enumerate() {
                            samples[i] = i16::from_le_bytes(*c);
                        }
                        l.store(db_to_raw(rms_dbfs(&samples)), Ordering::Relaxed);
                    }
                    l.store(db_to_raw(FLOOR_DB), Ordering::Relaxed);
                })
                .map_err(Error::Io)?;

            Ok(Self { child, level, stop })
        }

        pub fn level_db(&self) -> f32 {
            raw_to_db(self.level.load(Ordering::Relaxed))
        }

        pub fn is_alive(&mut self) -> bool {
            matches!(self.child.try_wait(), Ok(None))
        }
    }

    impl Drop for Meter {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// ---- Windows：WASAPI 共享模式采集 ----

#[cfg(windows)]
mod backend {
    use is_audio::wasapi::Capture;

    use super::*;
    use crate::Error;

    pub struct Meter {
        capture: Capture,
        level: Arc<AtomicI32>,
    }

    impl Meter {
        pub fn open(endpoint_id: &str, loopback: bool) -> Result<Self> {
            let level = Arc::new(AtomicI32::new(db_to_raw(FLOOR_DB)));
            let l = Arc::clone(&level);

            let capture = Capture::start(endpoint_id, loopback, move |fmt| {
                // 攒够 100ms 再算，和 parec 那条路径的时间常数保持一致。
                // 窗口按设备实际速率算，不强行重采样。
                let target = (fmt.rate as usize / 10).max(1) * fmt.channels.max(1) as usize;
                let mut window: Vec<i16> = Vec::new();
                move |frames: &[i16]| {
                    window.extend_from_slice(frames);
                    while window.len() >= target {
                        l.store(db_to_raw(rms_dbfs(&window[..target])), Ordering::Relaxed);
                        window.drain(..target);
                    }
                }
            })
            .map_err(|e| Error::Tool {
                what: "WASAPI 电平采集".into(),
                detail: e.to_string(),
            })?;

            Ok(Self { capture, level })
        }

        pub fn level_db(&self) -> f32 {
            raw_to_db(self.level.load(Ordering::Relaxed))
        }

        pub fn is_alive(&mut self) -> bool {
            self.capture.is_running() && self.capture.failure().is_none()
        }
    }
}

pub struct Meter(backend::Meter);

impl Meter {
    /// 麦克风：直接采这个输入端点。
    pub fn mic(device_id: &str) -> Result<Self> {
        #[cfg(unix)]
        return Ok(Self(backend::Meter::open(device_id)?));
        #[cfg(windows)]
        return Ok(Self(backend::Meter::open(device_id, false)?));
    }

    /// 系统输出：Linux 采 sink 的 monitor 源，Windows 对渲染端点做环回采集。
    /// 分成两个入口而不是一个字符串，是因为 Windows 上「采哪个设备」和
    /// 「要不要环回」是两件事，光看名字分不出来。
    pub fn system(loopback: &LoopbackSource) -> Result<Self> {
        match loopback {
            #[cfg(unix)]
            LoopbackSource::PulseMonitor(name) => Ok(Self(backend::Meter::open(name)?)),
            #[cfg(windows)]
            LoopbackSource::WasapiLoopback(id) => Ok(Self(backend::Meter::open(id, true)?)),
            other => Err(crate::Error::ToolMissing(format!(
                "这个平台起不了 {other:?} 的电平表"
            ))),
        }
    }

    pub fn level_db(&self) -> f32 {
        self.0.level_db()
    }

    pub fn is_alive(&mut self) -> bool {
        self.0.is_alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_reads_floor() {
        assert_eq!(rms_dbfs(&[0; 1000]), FLOOR_DB);
        assert_eq!(rms_dbfs(&[]), FLOOR_DB);
    }

    #[test]
    fn full_scale_reads_about_zero() {
        let sq: Vec<i16> = (0..1000)
            .map(|i| if i % 2 == 0 { i16::MAX } else { -i16::MAX })
            .collect();
        let db = rms_dbfs(&sq);
        assert!(db.abs() < 0.1, "满幅应当接近 0 dBFS，实际 {db}");
    }

    #[test]
    fn halving_amplitude_drops_six_db() {
        let loud: Vec<i16> = (0..1000)
            .map(|i| if i % 2 == 0 { 10000 } else { -10000 })
            .collect();
        let quiet: Vec<i16> = loud.iter().map(|s| s / 2).collect();
        let delta = rms_dbfs(&loud) - rms_dbfs(&quiet);
        assert!(
            (delta - 6.02).abs() < 0.05,
            "应当降约 6.02 dB，实际 {delta}"
        );
    }

    #[test]
    fn long_window_does_not_overflow() {
        let db = rms_dbfs(&vec![i16::MAX; 100_000]);
        assert!(db.abs() < 0.1, "长窗口算错了: {db}");
    }

    #[test]
    fn fixed_point_roundtrip_keeps_enough_precision() {
        for db in [-90.0f32, -60.5, -23.4, -0.1, 0.0] {
            let back = raw_to_db(db_to_raw(db));
            assert!((back - db).abs() < 0.02, "{db} -> {back}");
        }
    }

    // 另一个平台的 loopback 变体要被明确拒绝，而不是静默起一个测不到声音的表
    #[test]
    fn foreign_loopback_variant_is_rejected() {
        #[cfg(unix)]
        let foreign = LoopbackSource::WasapiLoopback("dev".into());
        #[cfg(windows)]
        let foreign = LoopbackSource::PulseMonitor("x.monitor".into());
        assert!(Meter::system(&foreign).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn starts_on_a_real_source() {
        use std::process::Command;
        let Ok(out) = Command::new("pactl").arg("info").output() else {
            eprintln!("跳过：没有 pactl");
            return;
        };
        if !out.status.success() {
            eprintln!("跳过：PipeWire 没跑");
            return;
        }
        let sink = String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
            l.strip_prefix("Default Sink:")
                .map(|s| s.trim().to_string())
        });
        let Some(sink) = sink else { return };

        match Meter::system(&LoopbackSource::PulseMonitor(format!("{sink}.monitor"))) {
            Ok(mut m) => {
                std::thread::sleep(std::time::Duration::from_millis(400));
                assert!(m.is_alive(), "parec 立刻退出了");
                let db = m.level_db();
                assert!((FLOOR_DB..=0.5).contains(&db), "电平超出合理范围: {db}");
                eprintln!("实测电平 {db:.1} dBFS");
            }
            Err(crate::Error::ToolMissing(_)) => eprintln!("跳过：没装 parec"),
            Err(e) => panic!("{e}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn starts_on_a_real_endpoint() {
        use is_audio::Backend;
        let Ok(b) = is_audio::wasapi::WasapiBackend::new() else {
            eprintln!("跳过：拿不到 WASAPI 枚举器");
            return;
        };
        let Ok(sink) = b.default_sink() else {
            eprintln!("跳过：本机没有默认输出端点");
            return;
        };
        let mut m = Meter::system(&b.loopback_source(&sink).expect("环回源")).expect("起电平表");
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(m.is_alive(), "采集线程立刻退出了");
        let db = m.level_db();
        assert!((FLOOR_DB..=0.5).contains(&db), "电平超出合理范围: {db}");
        eprintln!("实测电平 {db:.1} dBFS");
    }
}

#[cfg(all(test, unix))]
mod live_tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    #[ignore = "会外放声音"]
    fn responds_to_actual_audio() {
        let Ok(out) = Command::new("pactl").arg("info").output() else {
            return;
        };
        let sink = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| {
                l.strip_prefix("Default Sink:")
                    .map(|s| s.trim().to_string())
            })
            .expect("默认输出");

        let mut meter = Meter::system(&LoopbackSource::PulseMonitor(format!("{sink}.monitor")))
            .expect("起电平表");
        std::thread::sleep(Duration::from_millis(300));
        let quiet = meter.level_db();

        let mut tone = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                // -re 必须有：lavfi 以最快速度生成，ffmpeg 灌完就退出，流还没播出去就断了
                "-re",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2:sample_rate=48000",
                "-af",
                "volume=-20dB",
                "-f",
                "pulse",
                "-device",
                &sink,
                "meter-test",
            ])
            .spawn()
            .expect("放音");

        std::thread::sleep(Duration::from_millis(1200));
        let loud = meter.level_db();
        let _ = tone.wait();

        eprintln!(
            "静默 {quiet:.1} dBFS → 放音 {loud:.1} dBFS（差 {:.1} dB）",
            loud - quiet
        );
        assert!(meter.is_alive(), "采集进程中途死了");
        assert!(
            loud > quiet + 20.0,
            "放音时电平没有明显上升：{quiet:.1} -> {loud:.1}"
        );
        assert!(loud > -60.0, "放音时电平仍然过低: {loud:.1}");
    }
}
