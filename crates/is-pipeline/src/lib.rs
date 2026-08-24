//! 录音与后处理管线。
//!
//! 录制和编码都交给 ffmpeg 子进程。看起来"不够 Rust"，但它替我们处理了一件
//! 不显眼却关键的事：**两路输入的时钟对齐**。麦克风和系统输出是两个独立时钟域，
//! 一场 40 分钟的面试下来会漂移。等做 Windows 后端必须直面这个问题时再一起解决。
//!
//! 三个能力：
//! - [`record`]：起停双轨录音，停止走 SIGINT 让 ffmpeg 写完容器尾
//! - [`probe`]：读时长、轨数、电平

pub mod probe;
pub mod record;

use std::fmt;

pub use probe::{Levels, track_levels};
pub use record::{RecordConfig, Recording};

/// 轨道标题。写进容器元数据，播放器和后续处理都靠它认轨。
pub const TITLE_MIX: &str = "混音(双方)";
pub const TITLE_MIC: &str = "我(麦克风)";
pub const TITLE_SYS: &str = "对方(系统输出)";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// ffmpeg / ffprobe 不存在或起不来
    ToolMissing(String),
    /// 工具跑了但失败了，附上它自己的错误输出
    Tool {
        what: String,
        detail: String,
    },
    Io(std::io::Error),
    Parse(String),
    /// 产物没通过校验，原文件已保持不动
    Verify(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolMissing(s) => write!(f, "找不到 {s}"),
            Self::Tool { what, detail } => write!(f, "{what} 失败: {detail}"),
            Self::Io(e) => write!(f, "IO 错误: {e}"),
            Self::Parse(s) => write!(f, "解析失败: {s}"),
            Self::Verify(s) => write!(f, "校验不通过: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

