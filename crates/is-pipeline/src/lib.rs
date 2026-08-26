// 录制交给 ffmpeg 子进程：它处理两路输入的时钟对齐——麦克风和系统输出是
// 两个独立时钟域，40 分钟下来会漂移。

pub mod disk;
pub mod meter;
pub mod mix;
pub mod probe;
pub mod record;
pub mod tool;

use std::fmt;
use std::path::PathBuf;

pub use meter::{FLOOR_DB, Meter};
pub use mix::mix_in_place;
pub use probe::{Levels, track_levels};
pub use record::{RecordConfig, Recording};

pub const TITLE_MIX: &str = "混音(双方)";
pub const TITLE_MIC: &str = "我(麦克风)";
pub const TITLE_SYS: &str = "对方(系统输出)";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    ToolMissing(String),
    Tool { what: String, detail: String },
    Io(std::io::Error),
    Parse(String),
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

#[derive(Debug, Clone)]
pub struct MixReport {
    pub path: PathBuf,
    pub mix_peak_db: f32,
    /// 原始双轨各自的电平，在混音之前量的——混完轨号会整体后移一位。
    pub mic: Levels,
    pub sys: Levels,
    /// 为配平混音轨给两路各加的增益（dB）。(0, 0) 表示本来就够均衡。
    pub balance_db: (f32, f32),
    pub skipped: bool,
}
