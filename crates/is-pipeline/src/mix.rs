use std::path::{Path, PathBuf};

use crate::probe::{self, Levels};
use crate::{Error, MixReport, Result, TITLE_MIC, TITLE_MIX, TITLE_SYS};

// 直接相加会削顶：实测 39 分钟录音有 486 个采样撞满刻度。-1 dBFS 限幅后
// 峰值 -1.0 dB，平均电平不变。
const LIMIT_LINEAR: &str = "0.891";

const DURATION_TOLERANCE: f64 = 2.0;

// 配平参数。差在容差以内就不动——两个人音量本来就不会完全一样，
// 强行拉平反而不自然。
const BALANCE_TOLERANCE_DB: f32 = 6.0;
const MAX_ATTENUATION_DB: f32 = 18.0;
// 压完整体会变小，用一个共同的补偿增益把预测峰值推回接近满幅。
const MAKEUP_TARGET_PEAK_DB: f32 = -3.0;
const MAX_MAKEUP_DB: f32 = 12.0;

// 播放器默认只播第一条轨（原来是麦克风），所以双击只听得到自己。
// 录完再混而不是实时混：录音这条路径不能失败，合成挂了原件还在。
pub fn mix_in_place(path: &Path) -> Result<MixReport> {
    let tracks = probe::audio_track_count(path)?;
    if tracks == 3 {
        // 已经混过了：轨序是 [混音, 我, 对方]
        return Ok(MixReport {
            path: path.to_path_buf(),
            mix_peak_db: probe::track_levels(path, 0)?.peak_db,
            mic: probe::track_levels(path, 1)?,
            sys: probe::track_levels(path, 2)?,
            balance_db: (0.0, 0.0),
            skipped: true,
        });
    }
    if tracks != 2 {
        return Err(Error::Verify(format!(
            "需要恰好 2 条音轨，实际 {tracks} 条"
        )));
    }

    // 必须在混音之前量：混完轨号整体后移一位，再量就量错对象了
    let mic = probe::track_levels(path, 0)?;
    let sys = probe::track_levels(path, 1)?;
    let balance_db = balance(mic, sys);

    let tmp = tmp_path(path);
    let _guard = Cleanup(tmp.clone());

    let status = crate::tool::command("ffmpeg")
        .args(build_args(path, &tmp, balance_db))
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
        mic,
        sys,
        balance_db,
        skipped: false,
    })
}

/// 混音轨的配平。
///
/// amix 是两路满增益直加，一方比另一方响 20 dB 时，混音轨就只听得见响的那个
/// ——而这条轨存在的唯一理由是「双击一下能同时听见两个人」。原始双轨不受影响。
///
/// 只压不抬：抬高安静的那条会把它的底噪一起抬上来，而底噪往往正是它安静的
/// 原因。压完整体变小，再给两条同样的补偿增益推回去；补偿是共同的，不改变
/// 两者的相对关系。
fn balance(mic: Levels, sys: Levels) -> (f32, f32) {
    // 有一条全程静音就不动：那条轨没内容，压另一条只会让整个混音白白变小
    if mic.is_silent() || sys.is_silent() {
        return (0.0, 0.0);
    }

    let gap = mic.mean_db - sys.mean_db;
    let cut = if gap.abs() <= BALANCE_TOLERANCE_DB {
        0.0
    } else {
        (gap.abs() - BALANCE_TOLERANCE_DB).min(MAX_ATTENUATION_DB)
    };
    let (mut gm, mut gs) = if gap > 0.0 { (-cut, 0.0) } else { (0.0, -cut) };

    // 两路相加的峰值上界就是各自峰值的线性和，据此算补偿。多出来的部分
    // 由 alimiter 兜住，所以宁可估得保守。
    let predicted = sum_db(mic.peak_db + gm, sys.peak_db + gs);
    let makeup = (MAKEUP_TARGET_PEAK_DB - predicted).clamp(0.0, MAX_MAKEUP_DB);
    gm += makeup;
    gs += makeup;
    (gm, gs)
}

fn sum_db(a: f32, b: f32) -> f32 {
    let lin = 10f32.powf(a / 20.0) + 10f32.powf(b / 20.0);
    20.0 * lin.log10()
}

fn build_args(input: &Path, output: &Path, (gm, gs): (f32, f32)) -> Vec<String> {
    let filter = format!(
        "[0:a:0]volume={gm:.2}dB[m];[0:a:1]volume={gs:.2}dB[s];\
         [m][s]amix=inputs=2:duration=longest:normalize=0,\
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
        let ok = crate::tool::command("ffmpeg")
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
        let ok = crate::tool::command("ffmpeg")
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

    fn lv(mean: f32, peak: f32) -> Levels {
        Levels {
            mean_db: mean,
            peak_db: peak,
        }
    }

    // 用户实测到的场景：放着音乐说话，混音轨里音乐把人声完全盖掉
    #[test]
    fn loud_side_is_pulled_down_toward_the_quiet_one() {
        let mic = lv(-35.0, -12.0); // 说话
        let sys = lv(-12.0, -1.0); // 音乐
        let (gm, gs) = balance(mic, sys);
        let after = (mic.mean_db + gm) - (sys.mean_db + gs);
        assert!(
            after.abs() < 23.0,
            "配平后差距应当明显变小，实际 {after:.1} dB"
        );
        assert!(
            gs < gm,
            "该被压的是响的那条（对方），实际 gm={gm:.1} gs={gs:.1}"
        );
    }

    #[test]
    fn attenuation_is_capped() {
        // 差 60 dB 也不该把一条压到听不见
        let (gm, gs) = balance(lv(-70.0, -40.0), lv(-10.0, 0.0));
        assert!(gs - gm >= -MAX_ATTENUATION_DB - 0.01, "gm={gm} gs={gs}");
    }

    #[test]
    fn balanced_pair_is_left_alone_apart_from_makeup() {
        // 差在容差以内：两个人音量本来就不会完全一样，不该强行拉平
        let (gm, gs) = balance(lv(-20.0, -6.0), lv(-23.0, -8.0));
        assert!((gm - gs).abs() < 0.01, "只该有共同的补偿增益: {gm} {gs}");
    }

    #[test]
    fn silent_track_disables_balancing() {
        // 一条全程静音时压另一条只会让整个混音白白变小
        let silent = lv(-91.0, -91.0);
        assert_eq!(balance(lv(-20.0, -5.0), silent), (0.0, 0.0));
        assert_eq!(balance(silent, lv(-20.0, -5.0)), (0.0, 0.0));
    }

    // 两条本来就接近满幅的轨相加必然过冲，那是 alimiter 的活，不是配平的。
    // 配平该保证的是：补偿只在有余量时才加，且加完不越过目标峰值。
    #[test]
    fn makeup_only_uses_available_headroom() {
        for (m, s) in [(-35.0, -12.0), (-20.0, -23.0), (-60.0, -8.0), (-6.0, -6.0)] {
            let (mic, sys) = (lv(m, m + 20.0), lv(s, s + 20.0));
            let (gm, gs) = balance(mic, sys);
            let before = sum_db(mic.peak_db, sys.peak_db);
            let after = sum_db(mic.peak_db + gm, sys.peak_db + gs);
            assert!(
                after <= before.max(MAKEUP_TARGET_PEAK_DB) + 0.01,
                "配平把峰值从 {before:.1} 推到了 {after:.1} dB（{m}/{s}）"
            );
        }
    }

    // 没有余量时不该硬加补偿
    #[test]
    fn hot_tracks_get_no_makeup() {
        let (gm, gs) = balance(lv(-20.0, 0.0), lv(-23.0, -3.0));
        assert_eq!((gm, gs), (0.0, 0.0), "两条都快满幅了，不该再抬");
    }

    #[test]
    fn gains_appear_in_the_filter_graph() {
        let joined = build_args(Path::new("a.mkv"), Path::new("b.mkv"), (-3.5, 0.0)).join(" ");
        assert!(joined.contains("volume=-3.50dB"), "{joined}");
        assert!(joined.contains("volume=0.00dB"), "{joined}");
        // 配平只作用在混音轨上，原始两轨仍然是 copy
        assert!(joined.contains("-c:a:1 copy"), "{joined}");
        assert!(joined.contains("-c:a:2 copy"), "{joined}");
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
