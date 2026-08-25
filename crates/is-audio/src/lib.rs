#[cfg(unix)]
pub mod pulse;
#[cfg(windows)]
pub mod wasapi;

use std::fmt;

// 让上层不用到处写 cfg：本平台该用哪个后端，在这里定一次。
#[cfg(unix)]
pub fn default_backend() -> Result<Box<dyn Backend>> {
    Ok(Box::new(pulse::PulseBackend::new()?))
}

#[cfg(windows)]
pub fn default_backend() -> Result<Box<dyn Backend>> {
    Ok(Box::new(wasapi::WasapiBackend::new()?))
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopbackSource {
    PulseMonitor(String),
    #[allow(dead_code)]
    WasapiLoopback(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
// PipeWire 按应用记住输出设备，默认设备与实际出声设备可能不一致：
// 会议软件被记去别的设备时，对方那条轨全程静音，而耳朵里一切正常。
pub enum Routing {
    NoAudioPlaying {
        default: Device,
    },
    Aligned {
        default: Device,
    },
    PartlyElsewhere {
        default: Device,
        elsewhere: Vec<Device>,
    },
    AllElsewhere {
        default: Device,
        elsewhere: Vec<Device>,
    },
}

impl Routing {
    pub fn will_capture_system_audio(&self) -> bool {
        matches!(self, Self::Aligned { .. } | Self::PartlyElsewhere { .. })
    }

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

pub trait Backend {
    fn default_sink(&self) -> Result<Device>;
    fn default_source(&self) -> Result<Device>;
    fn sinks(&self) -> Result<Vec<Device>>;
    fn sources(&self) -> Result<Vec<Device>>;
    fn active_sinks(&self) -> Result<Vec<Device>>;
    fn loopback_source(&self, sink: &Device) -> Result<LoopbackSource>;

    // 路由不对时能不能替用户切默认输出。Linux 上 pactl 就能改；Windows 上没有
    // 受支持的 API（只有未文档化的 IPolicyConfig，跨版本会碎），只能让界面改成
    // 引导用户自己去系统设置里改。
    fn can_set_default_sink(&self) -> bool {
        false
    }

    fn set_default_sink(&self, _sink: &Device) -> Result<()> {
        Err(Error::Unavailable(
            "这个平台不支持由程序切换默认输出设备".into(),
        ))
    }

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
        // 测不出来 ≠ 一定能录到，不能报「没问题」
        let r = routing("earpods", &[]);
        assert!(matches!(r, Routing::NoAudioPlaying { .. }));
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
