// 用 MMDevice + AudioSession 而不是 cpal：cpal 只给得出设备列表，看不到「哪台
// 设备上真的有流在播」——而那恰好是预检唯一要回答的问题。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, EDataFlow, IAudioCaptureClient, IAudioClient,
    IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture,
    eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, STGM_READ,
};
use windows::core::{Interface, PCWSTR};

use crate::{Backend, Device, Error, LoopbackSource, Result};

pub struct WasapiBackend {
    enumerator: IMMDeviceEnumerator,
}

impl WasapiBackend {
    pub fn new() -> Result<Self> {
        // eframe/winit 已经在主线程上初始化过 COM。重复调用返回 S_FALSE，用另一种
        // 套间模式调用返回 RPC_E_CHANGED_MODE——两者都说明 COM 可用，都不是错误，
        // 所以这里不看返回值。真正的失败会在 CoCreateInstance 上暴露出来。
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| Error::Unavailable(format!("拿不到音频设备枚举器: {e}")))?;
        Ok(Self { enumerator })
    }

    fn endpoints(&self, flow: EDataFlow) -> Result<Vec<Device>> {
        let coll = unsafe {
            self.enumerator
                .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
        }
        .map_err(failed("枚举音频端点"))?;
        let n = unsafe { coll.GetCount() }.map_err(failed("取端点数量"))?;
        (0..n)
            .map(|i| {
                let d = unsafe { coll.Item(i) }.map_err(failed("取端点"))?;
                describe(&d)
            })
            .collect()
    }

    fn default_endpoint(&self, flow: EDataFlow) -> Result<Device> {
        let d = unsafe { self.enumerator.GetDefaultAudioEndpoint(flow, eConsole) }
            .map_err(|e| Error::Unavailable(format!("没有默认{}设备: {e}", flow_name(flow))))?;
        describe(&d)
    }

    fn device_by_id(&self, id: &str) -> Result<IMMDevice> {
        let wide = wide(id);
        unsafe { self.enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
            .map_err(|e| Error::Parse(format!("找不到设备 {id}: {e}")))
    }

    // 一台渲染端点上只要有一个会话处于 Active，就算它在出声。对应 Linux 那边的
    // pactl list short sink-inputs。
    fn has_active_session(&self, id: &str) -> Result<bool> {
        let dev = self.device_by_id(id)?;
        let mgr: IAudioSessionManager2 =
            unsafe { dev.Activate(CLSCTX_ALL, None) }.map_err(failed("打开会话管理器"))?;
        let sessions = unsafe { mgr.GetSessionEnumerator() }.map_err(failed("枚举音频会话"))?;
        let n = unsafe { sessions.GetCount() }.map_err(failed("取会话数量"))?;

        for i in 0..n {
            // 单个会话读不出来就跳过：会话随时可能在枚举过程中消失，
            // 不该让整次预检失败。
            let Ok(ctl) = (unsafe { sessions.GetSession(i) }) else {
                continue;
            };
            // AudioSessionStateActive 是常量不是枚举变体，写进 matches! 的模式
            // 位置会被当成绑定名，那个分支就永远命中了。必须用等号比。
            if unsafe { ctl.GetState() }.is_ok_and(|s| s == AudioSessionStateActive)
                && !is_system_sounds(&ctl)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// 系统音效（通知音）不算「有人在这台设备上放东西」。它是一个常驻会话，
// 响一声就会短暂变成 Active，会让预检在错误的时刻给出绿灯。
fn is_system_sounds(ctl: &IAudioSessionControl) -> bool {
    let Ok(c2) = ctl.cast::<IAudioSessionControl2>() else {
        return false;
    };
    // 这个方法用返回码区分是与否：S_OK 表示是，S_FALSE 表示不是。两者都算
    // 「成功」，所以必须比 HRESULT 本身，不能用 is_ok()。
    (unsafe { c2.IsSystemSoundsSession() }) == S_OK
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> Error {
    move |e| Error::Unavailable(format!("{what}失败: {e}"))
}

fn flow_name(flow: EDataFlow) -> &'static str {
    if flow == eRender { "输出" } else { "输入" }
}

// 端点 ID 形如 {0.0.0.00000000}.{guid}，是跨重启稳定的标识，用来做 id；
// 友好名给人看。取不到友好名不致命，Display 会退回显示 id。
fn describe(d: &IMMDevice) -> Result<Device> {
    Ok(Device {
        id: endpoint_id(d)?,
        description: friendly_name(d),
    })
}

fn endpoint_id(d: &IMMDevice) -> Result<String> {
    let pw = unsafe { d.GetId() }.map_err(failed("取设备 ID"))?;
    // Safety: GetId 成功时返回一枚 COM 分配的 NUL 结尾宽字符串，
    // 读完由调用方释放，正是下面这一次。
    let s = unsafe { pw.to_string() }
        .map_err(|e| Error::Parse(format!("设备 ID 不是合法 UTF-16: {e}")))?;
    unsafe { CoTaskMemFree(Some(pw.0.cast())) };
    Ok(s)
}

fn friendly_name(d: &IMMDevice) -> String {
    unsafe {
        d.OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

impl Backend for WasapiBackend {
    fn default_sink(&self) -> Result<Device> {
        self.default_endpoint(eRender)
    }

    fn default_source(&self) -> Result<Device> {
        self.default_endpoint(eCapture)
    }

    fn sinks(&self) -> Result<Vec<Device>> {
        self.endpoints(eRender)
    }

    fn sources(&self) -> Result<Vec<Device>> {
        self.endpoints(eCapture)
    }

    fn active_sinks(&self) -> Result<Vec<Device>> {
        let mut out = Vec::new();
        for dev in self.sinks()? {
            if self.has_active_session(&dev.id)? {
                out.push(dev);
            }
        }
        Ok(out)
    }

    // 不像 PulseAudio 需要一个单独存在的 .monitor 源，Windows 上任何渲染端点都能
    // 用 AUDCLNT_STREAMFLAGS_LOOPBACK 打开。这里只确认 ID 还解析得到设备——
    // 设备可能在预检之后被拔掉。
    fn loopback_source(&self, sink: &Device) -> Result<LoopbackSource> {
        self.device_by_id(&sink.id)?;
        Ok(LoopbackSource::WasapiLoopback(sink.id.clone()))
    }

    // Windows 没有受支持的「设置默认音频设备」API。IPolicyConfig 是未文档化的
    // 内部接口，其 IID 与方法顺序在各版本间变过，不能拿去赌用户的录音。
    fn can_set_default_sink(&self) -> bool {
        false
    }
}

// ---- 采集 ----
//
// 共享模式，轮询而非事件驱动：环回采集配 AUDCLNT_STREAMFLAGS_EVENTCALLBACK
// 在部分 Windows 版本上会被拒（AUDCLNT_E_UNSUPPORTED），轮询两边都能用。
// 缓冲区留 200ms，8ms 轮一次，余量足够。

const CAPTURE_BUFFER: i64 = 2_000_000; // 100ns 单位 = 200ms
const POLL: Duration = Duration::from_millis(8);
// 补静音的上限。正常空闲会走到几十秒，但 QPC 出异常时不该无限分配。
const MAX_GAP_SECS: u64 = 60;

const WAVE_FORMAT_PCM: u32 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u32 = 3;
const WAVE_FORMAT_EXTENSIBLE: u32 = 0xFFFE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureFormat {
    pub rate: u32,
    pub channels: u16,
}

/// 一路 WASAPI 采集。`loopback` 为真时对渲染端点做环回采集，录的是这台
/// 设备正在播出的声音。交给回调的是交织的 i16，声道数见 `format()`。
pub struct Capture {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    format: CaptureFormat,
    failure: Arc<Mutex<Option<String>>>,
}

impl Capture {
    /// `make_sink` 拿到协商好的格式之后才构造消费者：采样率和声道数要等
    /// 打开设备才知道，而窗口长度、文件头这些都依赖它。
    pub fn start<F>(
        endpoint_id: &str,
        loopback: bool,
        make_sink: impl FnOnce(CaptureFormat) -> F + Send + 'static,
    ) -> Result<Self>
    where
        F: FnMut(&[i16]) + Send + 'static,
    {
        let id = endpoint_id.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();
        let (s, f) = (Arc::clone(&stop), Arc::clone(&failure));

        // 建立和使用都放在同一个线程：COM 对象有套间亲和性，跨线程传递需要
        // 编组。只把协商好的格式用 channel 传回来。
        let join = std::thread::Builder::new()
            .name(format!("wasapi:{}", if loopback { "sys" } else { "mic" }))
            .spawn(move || {
                // 自己的线程，用 MTA；失败也不致命，Session 里的调用会报错。
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                match Session::open(&id, loopback) {
                    Ok(session) => {
                        let mut sink = make_sink(session.format);
                        if tx.send(Ok(session.format)).is_err() {
                            return;
                        }
                        if let Err(e) = session.run(&s, &mut sink) {
                            *f.lock().unwrap() = Some(e.to_string());
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| Error::Unavailable(format!("起采集线程失败: {e}")))?;

        let format = rx
            .recv()
            .map_err(|_| Error::Unavailable("采集线程没有回话".into()))??;
        Ok(Self {
            stop,
            join: Some(join),
            format,
            failure,
        })
    }

    pub fn format(&self) -> CaptureFormat {
        self.format
    }

    /// 采集线程中途死掉时的原因。录音界面靠它把「还在录」和「已经断了」分开。
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }

    pub fn is_running(&self) -> bool {
        self.join.as_ref().is_some_and(|j| !j.is_finished())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct Session {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: CaptureFormat,
    sample: SampleKind,
}

#[derive(Clone, Copy)]
enum SampleKind {
    F32,
    I16,
}

impl Session {
    fn open(endpoint_id: &str, loopback: bool) -> Result<Self> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(failed("创建设备枚举器"))?;
        let wide = wide(endpoint_id);
        let device = unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
            .map_err(|e| Error::Parse(format!("找不到设备 {endpoint_id}: {e}")))?;

        let client: IAudioClient =
            unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(failed("打开音频客户端"))?;
        let mix = unsafe { client.GetMixFormat() }.map_err(failed("取混音格式"))?;
        if mix.is_null() {
            return Err(Error::Unavailable("混音格式为空".into()));
        }
        // Safety: GetMixFormat 成功时返回一枚 COM 分配的 WAVEFORMATEX，
        // 用完由调用方释放（下面 Initialize 之后立刻释放）。
        let (format, sample) = describe_format(unsafe { &*mix })?;

        let flags = if loopback {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };
        let init = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                CAPTURE_BUFFER,
                0,
                mix,
                None,
            )
        };
        unsafe { CoTaskMemFree(Some(mix.cast())) };
        init.map_err(failed("初始化采集流"))?;

        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.map_err(failed("取采集接口"))?;
        Ok(Self {
            client,
            capture,
            format,
            sample,
        })
    }

    fn run(&self, stop: &AtomicBool, on_frames: &mut impl FnMut(&[i16])) -> Result<()> {
        unsafe { self.client.Start() }.map_err(failed("启动采集"))?;
        let result = self.pump(stop, on_frames);
        let _ = unsafe { self.client.Stop() };
        result
    }

    fn pump(&self, stop: &AtomicBool, on_frames: &mut impl FnMut(&[i16])) -> Result<()> {
        let channels = self.format.channels as usize;
        let mut delivered: u64 = 0;
        let mut origin: Option<u64> = None;
        let mut buf: Vec<i16> = Vec::new();

        while !stop.load(Ordering::Relaxed) {
            let packet =
                unsafe { self.capture.GetNextPacketSize() }.map_err(failed("查询包大小"))?;
            if packet == 0 {
                std::thread::sleep(POLL);
                continue;
            }

            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            unsafe {
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
            }
            .map_err(failed("取采集缓冲"))?;

            let start = *origin.get_or_insert(qpc);
            let gap = gap_frames(qpc, start, self.format.rate, delivered);
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;

            // 用户几分钟不出声时 gap 会很大，一次性铺开是几十 MB 的分配。
            // 按秒切块发出去，消费者看到的仍是连续的静音。
            let mut remaining = gap;
            while remaining > 0 {
                let n = remaining.min(u64::from(self.format.rate)) as usize;
                buf.clear();
                buf.resize(n * channels, 0);
                on_frames(&buf);
                remaining -= n as u64;
            }

            buf.clear();
            if silent || data.is_null() {
                buf.resize(frames as usize * channels, 0);
            } else {
                // Safety: GetBuffer 成功时 data 指向 frames * channels 个
                // sample，格式即 Initialize 时协商的那个；ReleaseBuffer 之前有效。
                append_samples(
                    &mut buf,
                    data.cast_const(),
                    frames as usize * channels,
                    self.sample,
                );
            }

            let release = unsafe { self.capture.ReleaseBuffer(frames) };
            delivered += gap + frames as u64;
            if !buf.is_empty() {
                on_frames(&buf);
            }
            release.map_err(failed("归还采集缓冲"))?;
        }
        Ok(())
    }
}

// 环回采集在渲染端点空闲时根本不产生数据包（不是产生静音）。不补齐，静音段
// 就被吞掉：轨道变短、后面的内容整体前移，两条轨也就对不上了。QPC 是墙钟，
// 拿它算出「到现在为止本该有多少帧」，缺的部分补静音。
fn gap_frames(qpc_100ns: u64, origin: u64, rate: u32, delivered: u64) -> u64 {
    let elapsed = qpc_100ns.saturating_sub(origin);
    let expected = elapsed.saturating_mul(u64::from(rate)) / 10_000_000;
    expected
        .saturating_sub(delivered)
        .min(u64::from(rate) * MAX_GAP_SECS)
}

// 共享模式的混音格式实际上总是 32 位浮点，但别的驱动给 s16 也是合法的。
// 其余格式宁可明确报错，也不要按错误的位宽解释出噪声。
fn describe_format(f: &WAVEFORMATEX) -> Result<(CaptureFormat, SampleKind)> {
    // WAVEFORMATEX 是 packed 的，取字段引用是未定义行为（format! 会隐式取引用），
    // 所以先按值读进局部变量再用。
    let (tag, bits, rate, channels) = (
        format_tag(f),
        f.wBitsPerSample,
        f.nSamplesPerSec,
        f.nChannels,
    );
    let sample = match (tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleKind::F32,
        (WAVE_FORMAT_PCM, 16) => SampleKind::I16,
        _ => {
            return Err(Error::Unavailable(format!(
                "不认识的采集格式：tag {tag}，{bits} 位"
            )));
        }
    };
    Ok((CaptureFormat { rate, channels }, sample))
}

// WAVEFORMATEXTENSIBLE 的 SubFormat GUID 形如
// {tag}-0000-0010-8000-00AA00389B71，头 4 字节就是等价的 format tag。
fn format_tag(f: &WAVEFORMATEX) -> u32 {
    if u32::from(f.wFormatTag) == WAVE_FORMAT_EXTENSIBLE && f.cbSize >= 22 {
        // Safety: wFormatTag 为 EXTENSIBLE 且 cbSize 覆盖了扩展部分时，
        // 这块内存按定义就是一个 WAVEFORMATEXTENSIBLE。
        let ext = unsafe { &*std::ptr::from_ref(f).cast::<WAVEFORMATEXTENSIBLE>() };
        ext.SubFormat.data1
    } else {
        u32::from(f.wFormatTag)
    }
}

fn append_samples(out: &mut Vec<i16>, src: *const u8, count: usize, kind: SampleKind) {
    match kind {
        SampleKind::F32 => {
            // Safety: 调用方保证 src 指向 count 个 f32，且在本次调用期间有效。
            let s = unsafe { std::slice::from_raw_parts(src.cast::<f32>(), count) };
            out.extend(s.iter().map(|v| f32_to_i16(*v)));
        }
        SampleKind::I16 => {
            // Safety: 同上，count 个 i16。
            let s = unsafe { std::slice::from_raw_parts(src.cast::<i16>(), count) };
            out.extend_from_slice(s);
        }
    }
}

// 夹到 [-1, 1] 再定标：浮点混音可以超过满幅，直接乘会绕回成刺耳的爆音。
fn f32_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_padded_to_wall_clock() {
        // 48kHz，墙钟已过 1 秒，只交付了 0 帧 -> 该补 48000 帧
        assert_eq!(gap_frames(10_000_000, 0, 48_000, 0), 48_000);
        // 已经交付到位就不补
        assert_eq!(gap_frames(10_000_000, 0, 48_000, 48_000), 0);
        // 交付得比墙钟还多（正常抖动）也不能补出负数
        assert_eq!(gap_frames(10_000_000, 0, 48_000, 50_000), 0);
    }

    #[test]
    fn gap_padding_is_capped() {
        // QPC 异常跳变时不该分配出一个天文数字的缓冲
        let huge = gap_frames(u64::MAX, 0, 48_000, 0);
        assert_eq!(huge, 48_000 * MAX_GAP_SECS);
    }

    #[test]
    fn first_packet_never_pads() {
        // origin 就是第一个包的 qpc，差值为 0
        assert_eq!(gap_frames(123_456_789, 123_456_789, 48_000, 0), 0);
    }

    #[test]
    fn float_samples_clamp_instead_of_wrapping() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(-1.0), -i16::MAX);
        // 超过满幅要削平，不能绕回成正数
        assert_eq!(f32_to_i16(1.5), i16::MAX);
        assert_eq!(f32_to_i16(-1.5), -i16::MAX);
    }

    // 这条在 CI 那台没有声卡的 windows runner 上也会真跑：枚举器本身不依赖
    // 端点存在，没有设备时要给出干净的错误而不是 panic 或空 id。
    #[test]
    fn enumerator_works_even_with_no_devices() {
        let b = WasapiBackend::new().expect("COM 枚举器应当创建成功");
        let sinks = b.sinks().expect("列端点不该失败，哪怕一个都没有");
        let sources = b.sources().expect("列端点不该失败，哪怕一个都没有");
        eprintln!("输出端点 {} 个，输入端点 {} 个", sinks.len(), sources.len());

        match b.default_sink() {
            Ok(d) => assert!(!d.id.is_empty(), "默认输出的 id 不该为空"),
            Err(Error::Unavailable(m)) => eprintln!("没有默认输出（无声卡环境的预期结果）: {m}"),
            Err(e) => panic!("错误类型不对，应当是 Unavailable: {e}"),
        }
    }

    #[test]
    fn cannot_switch_default_sink_on_windows() {
        let b = WasapiBackend::new().expect("COM 枚举器应当创建成功");
        assert!(!b.can_set_default_sink());
        let dev = Device {
            id: "whatever".into(),
            description: String::new(),
        };
        assert!(b.set_default_sink(&dev).is_err(), "不该假装切成功了");
    }

    macro_rules! require_devices {
        ($b:ident) => {
            let Ok($b) = WasapiBackend::new() else {
                eprintln!("跳过：拿不到 WASAPI 设备枚举器");
                return;
            };
            if $b.sinks().map(|s| s.is_empty()).unwrap_or(true) {
                eprintln!("跳过：本机没有可用的音频输出端点");
                return;
            }
        };
    }

    #[test]
    fn enumerates_real_devices() {
        require_devices!(b);
        for s in b.sinks().unwrap() {
            assert!(!s.id.is_empty());
            assert!(!s.description.is_empty(), "{} 没有友好名", s.id);
        }
    }

    #[test]
    fn default_devices_are_in_the_list() {
        require_devices!(b);
        let d = b.default_sink().expect("默认输出");
        assert!(b.sinks().unwrap().iter().any(|s| s.id == d.id));
        let s = b.default_source().expect("默认输入");
        assert!(b.sources().unwrap().iter().any(|x| x.id == s.id));
    }

    #[test]
    fn every_sink_can_be_loopback_captured() {
        require_devices!(b);
        for sink in b.sinks().unwrap() {
            match b.loopback_source(&sink) {
                Ok(LoopbackSource::WasapiLoopback(id)) => assert_eq!(id, sink.id),
                Ok(other) => panic!("Windows 上不该返回 {other:?}"),
                Err(e) => panic!("{sink} 解析 loopback 失败: {e}"),
            }
        }
    }

    #[test]
    fn active_sinks_are_a_subset_of_all_sinks() {
        require_devices!(b);
        let all = b.sinks().unwrap();
        for a in b.active_sinks().unwrap() {
            assert!(all.iter().any(|s| s.id == a.id), "{a} 不在设备列表里");
        }
    }

    #[test]
    fn routing_check_runs_and_explains_itself() {
        require_devices!(b);
        let r = b.check_routing().expect("路由检查");
        assert!(!r.summary().is_empty());
        eprintln!("本机路由结论: {}", r.summary());
    }
}
