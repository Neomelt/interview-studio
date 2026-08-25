use std::path::{Path, PathBuf};
use std::process::Command;

use crate::probe::{self, Levels};
use crate::{Error, MixReport, Result, TITLE_MIC, TITLE_MIX, TITLE_SYS};

// 直接相加会削顶：实测 39 分钟录音有 486 个采样撞满刻度。-1 dBFS 限幅后
// 峰值 -1.0 dB，平均电平不变。
const LIMIT_LINEAR: &str = "0.891";

const DURATION_TOLERANCE: f64 = 2.0;

// 播放器默认只播第一条轨（原来是麦克风），所以双击只听得到自己。
// 录完再混而不是实时混：录音这条路径不能失败，合成挂了原件还在。
pub fn mix_in_place(path: &Path) -> Result<MixReport> {
    let tracks = probe::audio_track_count(path)?;
    if tracks == 3 {
        return Ok(MixReport {
            path: path.to_path_buf(),
            mix_peak_db: probe::track_levels(path, 0)?.peak_db,
            skipped: true,
        });
    }
    if tracks != 2 {
        return Err(Error::Verify(format!(
            "需要恰好 2 条音轨，实际 {tracks} 条"
        )));
    }

    let tmp = tmp_path(path);
    let _guard = Cleanup(tmp.clone());

    let status = Command::new("ffmpeg")
        .args(build_args(path, &tmp))
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
            what: "ffmpeg 混音".into(),
            detail: format!("退出码 {status}"),
        });
    }

    verify(path, &tmp)?;
    let levels = probe::track_levels(&tmp, 0)?;
    if levels.is_clipping() {
        return Err(Error::Verify(format!(
            "混音轨削顶了（峰值 {:.1} dB），限幅没生效",
            levels.peak_db
        )));
    }

    std::fs::rename(&tmp, path)?;
    Ok(MixReport {
        path: path.to_path_buf(),
        mix_peak_db: levels.peak_db,
        skipped: false,
    })
}

fn build_args(input: &Path, output: &Path) -> Vec<String> {
    let filter = format!(
        "[0:a:0][0:a:1]amix=inputs=2:duration=longest:normalize=0,\
         alimiter=limit={LIMIT_LINEAR}:level=disabled[mix]"
    );
    [
        "-nostdin",
        "-hide_banner",
        "-v",
        "error",
        "-y",
        "-i",
        &input.to_string_lossy(),
        "-filter_complex",
        &filter,
        "-map",
        "[mix]",
        "-map",
        "0:a:0",
        "-map",
        "0:a:1",
        "-c:a:0",
        "flac",
        "-sample_fmt:a:0",
        "s16",
        // copy = 不解码不重编码，原两轨无损
        "-c:a:1",
        "copy",
        "-c:a:2",
        "copy",
        "-metadata:s:a:0",
        &format!("title={TITLE_MIX}"),
        "-metadata:s:a:1",
        &format!("title={TITLE_MIC}"),
        "-metadata:s:a:2",
        &format!("title={TITLE_SYS}"),
        "-disposition:a:0",
        "default",
        "-disposition:a:1",
        "0",
        "-disposition:a:2",
        "0",
        &output.to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn verify(original: &Path, produced: &Path) -> Result<()> {
    let n = probe::audio_track_count(produced)?;
    if n != 3 {
        return Err(Error::Verify(format!("产物有 {n} 条音轨，应为 3")));
    }

    let (d0, d1) = (
        probe::duration_secs(original)?,
        probe::duration_secs(produced)?,
    );
    if (d0 - d1).abs() > DURATION_TOLERANCE {
        return Err(Error::Verify(format!("时长对不上：{d0:.2}s -> {d1:.2}s")));
    }

    // 比对解码后的 MD5，逐采样一致才算无损
    for (src, dst) in [(0usize, 1usize), (1, 2)] {
        let a = probe::track_audio_md5(original, src)?;
        let b = probe::track_audio_md5(produced, dst)?;
        if a != b {
            return Err(Error::Verify(format!(
                "原轨 {src} 在产物轨 {dst} 里被改动了（{a} != {b}）"
            )));
        }
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".mixing.mkv");
    PathBuf::from(s)
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

pub fn verdicts(path: &Path, mic_track: usize, sys_track: usize) -> Result<(Levels, Levels)> {
    Ok((
        probe::track_levels(path, mic_track)?,
        probe::track_levels(path, sys_track)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let d = std::env::temp_dir().join(format!(
                "is-mix-{}-{}-{tag}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // 满幅是故意的：两条相加必然削顶，用来验证限幅器真在干活
    fn synth_two_track(path: &Path, secs: u32) {
        let ok = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration={secs}"),
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=660:duration={secs}"),
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
                &path.to_string_lossy(),
            ])
            .status()
            .unwrap()
            .success();
        assert!(ok, "造素材失败");
    }

    fn synth_one_track(path: &Path) {
        let ok = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:a",
                "flac",
                "-sample_fmt",
                "s16",
                &path.to_string_lossy(),
            ])
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    // 本机没装工具时跳过是合理的；CI 里跳过不是。跳过和通过在测试日志里长得
    // 一模一样，混音这条链会在没人察觉的情况下变成「从未验证」。
    macro_rules! need_ffmpeg {
        () => {
            if !probe::tools_available() {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI 里必须装好 ffmpeg/ffprobe，否则这条测试等于没跑"
                );
                eprintln!("跳过：本机没有 ffmpeg/ffprobe");
                return;
            }
        };
    }

    #[test]
    fn produces_three_tracks_with_mix_as_default() {
        need_ffmpeg!();
        let s = Scratch::new("three");
        let f = s.file("a.mkv");
        synth_two_track(&f, 2);

        let r = mix_in_place(&f).expect("混音");
        assert!(!r.skipped);
        assert_eq!(probe::audio_track_count(&f).unwrap(), 3);
        assert_eq!(
            probe::track_titles(&f).unwrap(),
            vec![TITLE_MIX, TITLE_MIC, TITLE_SYS]
        );
        assert_eq!(
            probe::default_track_flags(&f).unwrap(),
            vec![true, false, false]
        );
    }

    #[test]
    fn original_tracks_survive_bit_identical() {
        need_ffmpeg!();
        let s = Scratch::new("lossless");
        let f = s.file("a.mkv");
        synth_two_track(&f, 2);

        let before = [
            probe::track_audio_md5(&f, 0).unwrap(),
            probe::track_audio_md5(&f, 1).unwrap(),
        ];
        mix_in_place(&f).expect("混音");
        let after = [
            probe::track_audio_md5(&f, 1).unwrap(),
            probe::track_audio_md5(&f, 2).unwrap(),
        ];
        assert_eq!(before, after, "原两轨被改动了");
    }

    #[test]
    fn limiter_prevents_clipping() {
        need_ffmpeg!();
        let s = Scratch::new("clip");
        let f = s.file("a.mkv");
        synth_two_track(&f, 2);

        let r = mix_in_place(&f).expect("混音");
        assert!(
            r.mix_peak_db <= -0.5,
            "混音轨峰值 {:.2} dB，限幅没起作用",
            r.mix_peak_db
        );
        assert!(!probe::track_levels(&f, 0).unwrap().is_clipping());
    }

    #[test]
    fn second_run_is_skipped_not_re_mixed() {
        need_ffmpeg!();
        let s = Scratch::new("idem");
        let f = s.file("a.mkv");
        synth_two_track(&f, 1);

        mix_in_place(&f).expect("首次");
        let md5_before = probe::track_audio_md5(&f, 0).unwrap();
        let r = mix_in_place(&f).expect("再次");
        assert!(r.skipped, "已经三轨了应当跳过");
        assert_eq!(probe::audio_track_count(&f).unwrap(), 3);
        assert_eq!(probe::track_audio_md5(&f, 0).unwrap(), md5_before);
    }

    #[test]
    fn wrong_track_count_leaves_file_untouched() {
        need_ffmpeg!();
        let s = Scratch::new("onetrack");
        let f = s.file("a.mkv");
        synth_one_track(&f);
        let before = std::fs::read(&f).unwrap();

        assert!(matches!(mix_in_place(&f), Err(Error::Verify(_))));
        assert_eq!(std::fs::read(&f).unwrap(), before, "原文件被动了");
        assert!(!tmp_path(&f).exists(), "临时文件没清理");
    }

    #[test]
    fn levels_classify_silence_and_clipping() {
        let silent = Levels {
            mean_db: -91.0,
            peak_db: -91.0,
        };
        assert!(silent.is_silent() && !silent.is_weak());

        let weak = Levels {
            mean_db: -60.0,
            peak_db: -40.0,
        };
        assert!(!weak.is_silent() && weak.is_weak());

        let ok = Levels {
            mean_db: -25.0,
            peak_db: -3.0,
        };
        assert!(!ok.is_silent() && !ok.is_weak() && !ok.is_clipping());

        assert!(
            Levels {
                mean_db: -20.0,
                peak_db: 0.0
            }
            .is_clipping()
        );
    }
}
