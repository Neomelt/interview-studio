//! 音频设备枚举与录音源解析。
//!
//! 面试录音要同时抓两路：麦克风（你）和系统输出（对方）。后者在不同平台上
//! 机制完全不同——Linux 是 PulseAudio/PipeWire 的 `.monitor` 源，Windows 是
//! WASAPI 的 loopback 标志。[`Backend`] 就是为了把这个差异挡住。
//!
//! 目前只有 Linux 实现（[`pulse::PulseBackend`]），走 `pactl`。之所以不用
//! cpal：cpal 在 Linux 默认走 ALSA，拿不到 monitor 源；而 `pactl` 这条路是
//! 现有 bash 版本已经跑了几十场录音验证过的。Windows 那版再上 cpal。

pub mod pulse;

use std::fmt;

/// 一个音频设备。`id` 是给程序用的稳定标识（Linux 上是 sink/source 名），
/// `description` 是给人看的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub description: String,
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.description.is_empty() {
            f.write_str(&self.id)
        } else {
            write!(f, "{}", self.description)
        }
    }
}

/// 录制系统输出所需要的采集源。Linux 上是个 monitor 源名；
/// Windows 上会是「某个输出设备 + loopback 标志」，所以这里留成枚举而不是裸字符串。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopbackSource {
    /// PulseAudio/PipeWire 的 monitor 源名，可直接当采集设备打开
    PulseMonitor(String),
    /// Windows：以 loopback 模式打开这个输出设备（预留，暂未实现）
    #[allow(dead_code)]
    WasapiLoopback(String),
}

/// 默认输出设备与「实际在出声的设备」是否一致。
///
/// 这是面试录音最容易翻车的地方：PipeWire 会按应用记住输出设备
/// （module-stream-restore），所以完全可能出现默认设备是耳机、
/// 但会议软件被记成了 HDMI——录出来对方那条轨全程静音，而你耳朵里一切正常。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routing {
    /// 当前没有任何应用在放声音，无法判断
    NoAudioPlaying { default: Device },
    /// 在出声的设备就是默认设备
    Aligned { default: Device },
    /// 默认设备在出声，但还有别的应用去了其它设备
    PartlyElsewhere {
        default: Device,
        elsewhere: Vec<Device>,
    },
    /// 所有声音都去了别的设备——照现在的配置录，对方那轨会是静音
    AllElsewhere {
        default: Device,
        elsewhere: Vec<Device>,
    },
}

impl Routing {
    /// 照当前配置录，对方那条轨会不会有声音。
    pub fn will_capture_system_audio(&self) -> bool {
        matches!(self, Self::Aligned { .. } | Self::PartlyElsewhere { .. })
    }

    /// 给人看的一句话结论。
    pub fn summary(&self) -> String {
        match self {
            Self::NoAudioPlaying { default } => {
                format!("当前没有声音在播放，测不出路由是否正确（默认输出：{default}）")
            }
            Self::Aligned { default } => format!("路由正确，声音走的就是默认输出：{default}"),
            Self::PartlyElsewhere { default, elsewhere } => format!(
                "默认输出 {default} 有声音，但还有应用在用：{}。确认会议软件走的是前者",
                join(elsewhere)
            ),
            Self::AllElsewhere { default, elsewhere } => format!(
                "声音全去了 {}，而录音抓的是默认输出 {default} —— 对方那轨会是静音",
                join(elsewhere)
            ),
        }
    }
}

fn join(ds: &[Device]) -> String {
    ds.iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("、")
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// 后端工具不可用（Linux 上是 pactl 缺失或 PipeWire 没跑）
    Unavailable(String),
    Parse(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(s) => write!(f, "音频后端不可用: {s}"),
            Self::Parse(s) => write!(f, "解析失败: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// 平台音频后端。Windows 版将来实现同一套接口，上层不用改。
pub trait Backend {
    /// 默认输出设备（对方声音从这儿出来）
    fn default_sink(&self) -> Result<Device>;
    /// 默认输入设备（你的麦克风）
    fn default_source(&self) -> Result<Device>;
    fn sinks(&self) -> Result<Vec<Device>>;
    fn sources(&self) -> Result<Vec<Device>>;
    /// 此刻真正有音频流在播的输出设备
    fn active_sinks(&self) -> Result<Vec<Device>>;
    /// 把一个输出设备转成「能录到它声音」的采集源
    fn loopback_source(&self, sink: &Device) -> Result<LoopbackSource>;

    /// 默认设备与实际出声设备是否对得上。录音前必查。
    fn check_routing(&self) -> Result<Routing> {
        let default = self.default_sink()?;
        let active = self.active_sinks()?;
        if active.is_empty() {
            return Ok(Routing::NoAudioPlaying { default });
        }
        let elsewhere: Vec<Device> = active
            .iter()
            .filter(|d| d.id != default.id)
            .cloned()
            .collect();
        let default_active = active.iter().any(|d| d.id == default.id);
        Ok(match (default_active, elsewhere.is_empty()) {
            (true, true) => Routing::Aligned { default },
            (true, false) => Routing::PartlyElsewhere { default, elsewhere },
            (false, _) => Routing::AllElsewhere { default, elsewhere },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str) -> Device {
        Device {
            id: id.into(),
            description: id.into(),
        }
    }

    /// 用假后端把 check_routing 的四个分支都走一遍，
    /// 不依赖跑测试的机器上插着什么设备。
    struct Fake {
        default: Device,
        active: Vec<Device>,
    }

    impl Backend for Fake {
        fn default_sink(&self) -> Result<Device> {
            Ok(self.default.clone())
        }
        fn default_source(&self) -> Result<Device> {
            Ok(dev("mic"))
        }
        fn sinks(&self) -> Result<Vec<Device>> {
            Ok(vec![])
        }
        fn sources(&self) -> Result<Vec<Device>> {
            Ok(vec![])
        }
        fn active_sinks(&self) -> Result<Vec<Device>> {
            Ok(self.active.clone())
        }
        fn loopback_source(&self, sink: &Device) -> Result<LoopbackSource> {
            Ok(LoopbackSource::PulseMonitor(format!("{}.monitor", sink.id)))
        }
    }

    fn routing(default: &str, active: &[&str]) -> Routing {
        Fake {
            default: dev(default),
            active: active.iter().map(|s| dev(s)).collect(),
        }
        .check_routing()
        .unwrap()
    }

    #[test]
    fn no_audio_playing() {
        let r = routing("earpods", &[]);
        assert!(matches!(r, Routing::NoAudioPlaying { .. }));
        // 测不出来 ≠ 一定会失败，但也不能报「没问题」
        assert!(!r.will_capture_system_audio());
    }

    #[test]
    fn aligned() {
        let r = routing("earpods", &["earpods"]);
        assert!(matches!(r, Routing::Aligned { .. }));
        assert!(r.will_capture_system_audio());
    }

    #[test]
    fn partly_elsewhere_still_captures() {
        // 会议软件在默认设备上，别的应用跑去了 HDMI —— 仍然录得到对方
        let r = routing("earpods", &["earpods", "hdmi"]);
        match &r {
            Routing::PartlyElsewhere { elsewhere, .. } => {
                assert_eq!(elsewhere.len(), 1);
                assert_eq!(elsewhere[0].id, "hdmi");
            }
            other => panic!("{other:?}"),
        }
        assert!(r.will_capture_system_audio());
    }

    /// 这就是会让面试录音报废的那种情况
    #[test]
    fn all_elsewhere_means_silent_track() {
        let r = routing("earpods", &["hdmi"]);
        assert!(matches!(r, Routing::AllElsewhere { .. }));
        assert!(!r.will_capture_system_audio(), "必须判定为录不到");
        assert!(
            r.summary().contains("静音"),
            "结论要说人话: {}",
            r.summary()
        );
    }
}
