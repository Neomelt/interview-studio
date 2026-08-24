//! 实时电平表。
//!
//! 独立进程读采集源，**刻意不挂在录音那个 ffmpeg 上**。同进程方案试过：
//! 电平走 FIFO，读端一消失 ffmpeg 就阻塞在写入上，进程还活着但录音停了——
//! 比直接崩还阴险。录音这条路径不能有任何额外的失败面。
//!
//! 前提是同一个源可以被多个进程并发读取。已验证：两个读者在同一个 monitor 上
//! 各自拿到分毫不差的数据，同时 ffmpeg 还在录。

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::{Error, Result};

/// 采样率。电平表不需要高保真，16k 足够且省 CPU。
const RATE: u32 = 16_000;
/// 每次计算用多长的音频。100ms 在「跟手」和「不抖」之间比较平衡。
const WINDOW_SAMPLES: usize = (RATE as usize) / 10;

/// 静默时的读数。真正的数字静默是负无穷 dB，用这个值代表。
pub const FLOOR_DB: f32 = -90.0;

/// 一路实时电平。
///
/// 和 [`crate::Recording`] 一样不实现 `Drop` 自动停止——但理由相反：
/// 电平表是可丢弃的，所以这里**实现** `Drop` 来收进程，避免留下孤儿 parec。
pub struct Meter {
    child: Child,
    level: Arc<AtomicI32>,
    stop: Arc<AtomicBool>,
}

impl Meter {
    /// 在指定采集源上起一路电平表。`source` 是 PulseAudio 的 source 名
    /// （麦克风或某个 sink 的 `.monitor`）。
    pub fn start(source: &str) -> Result<Self> {
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
                    // read_exact：不足一整窗就不算，避免短读导致电平乱跳
                    if stdout.read_exact(&mut buf).is_err() {
                        break;
                    }
                    for (i, c) in buf.chunks_exact(2).enumerate() {
                        samples[i] = i16::from_le_bytes([c[0], c[1]]);
                    }
                    l.store(db_to_raw(rms_dbfs(&samples)), Ordering::Relaxed);
                }
                // 进程没了就归零，别让界面停在最后一个读数上骗人
                l.store(db_to_raw(FLOOR_DB), Ordering::Relaxed);
            })
            .map_err(Error::Io)?;

        Ok(Self { child, level, stop })
    }

    /// 最近一个窗口的电平（dBFS）。
    pub fn level_db(&self) -> f32 {
        raw_to_db(self.level.load(Ordering::Relaxed))
    }

    /// 采集进程还活着吗。死了说明源被拔了或者被抢占了。
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

/// 一窗采样的 RMS，转成 dBFS。全零返回 [`FLOOR_DB`]。
pub fn rms_dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return FLOOR_DB;
    }
    // 用 f64 累加：i16 平方和在长窗口下会溢出 i32
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    let rms = (sum / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return FLOOR_DB;
    }
    let db = 20.0 * (rms / f64::from(i16::MAX)).log10();
    (db as f32).max(FLOOR_DB)
}

// 原子变量存不了 f32，用 dB×100 的定点整数代替。
fn db_to_raw(db: f32) -> i32 {
    (db * 100.0) as i32
}

fn raw_to_db(raw: i32) -> f32 {
    raw as f32 / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_reads_floor() {
        assert_eq!(rms_dbfs(&[0; 1000]), FLOOR_DB);
        assert_eq!(rms_dbfs(&[]), FLOOR_DB);
    }

    /// 满幅方波的 RMS 就是满刻度，应当接近 0 dBFS。
    #[test]
    fn full_scale_reads_about_zero() {
        let sq: Vec<i16> = (0..1000)
            .map(|i| if i % 2 == 0 { i16::MAX } else { -i16::MAX })
            .collect();
        let db = rms_dbfs(&sq);
        assert!(db.abs() < 0.1, "满幅应当接近 0 dBFS，实际 {db}");
    }

    /// 幅度减半应当正好降 6 dB —— 这是 dB 定义决定的，可以精确验证。
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

    /// 长窗口下平方和会超过 i32，必须用更宽的类型累加。
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

    /// 真实设备上起一路电平表，确认能读出数且进程活着。
    #[test]
    fn starts_on_a_real_source() {
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

        match Meter::start(&format!("{sink}.monitor")) {
            Ok(mut m) => {
                std::thread::sleep(std::time::Duration::from_millis(400));
                assert!(m.is_alive(), "parec 立刻退出了");
                let db = m.level_db();
                assert!((FLOOR_DB..=0.5).contains(&db), "电平超出合理范围: {db}");
                eprintln!("实测电平 {db:.1} dBFS");
            }
            Err(Error::ToolMissing(_)) => eprintln!("跳过：没装 parec"),
            Err(e) => panic!("{e}"),
        }
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use std::time::Duration;

    /// 会真的放出声音，所以默认不跑：
    ///   cargo test -p is-pipeline -- --ignored --nocapture
    ///
    /// 上面那个 starts_on_a_real_source 只能证明电平表「起得来」——
    /// 它读到 -90 时，坏掉的实现也会读到 -90。这个测试放一段已知的音进去，
    /// 证明读数确实跟着声音走。
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

        let mut meter = Meter::start(&format!("{sink}.monitor")).expect("起电平表");
        std::thread::sleep(Duration::from_millis(300));
        let quiet = meter.level_db();

        // 放 2 秒 -20dB 的正弦，音量不大但足够越过噪声
        let mut tone = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                // -re 必须有：lavfi 会以最快速度生成，ffmpeg 灌完就退出，
                // 流还没播出去就断了，电平表什么都读不到（这个坑踩过）
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
