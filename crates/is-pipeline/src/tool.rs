use std::path::PathBuf;
use std::process::Command;

// Windows 的安装包把 ffmpeg 和主程序装在同一个目录里，而那个目录不一定在
// PATH 上。所以先找和自己放在一起的那份，再退回 PATH——Linux 上后者就是包
// 依赖装进 /usr/bin 的那份，行为不变。
pub fn command(name: &str) -> Command {
    let mut c = match beside_exe(name) {
        Some(p) => Command::new(p),
        None => Command::new(name),
    };
    no_console(&mut c);
    c
}

// 主程序是 windows 子系统，本身没有控制台；子进程默认会自己申请一个，于是
// 每次探测、混音、校验都闪一个黑框。录一次要闪十几下。
#[cfg(windows)]
fn no_console(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console(_c: &mut Command) {}

fn beside_exe(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(exe_name(name));
    candidate.is_file().then_some(candidate)
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_looks_for_an_exe_suffix() {
        if cfg!(windows) {
            assert_eq!(exe_name("ffmpeg"), "ffmpeg.exe");
        } else {
            assert_eq!(exe_name("ffmpeg"), "ffmpeg");
        }
    }

    // 没有随程序分发的那份时必须退回 PATH，否则 Linux 上一个都找不到
    #[test]
    fn falls_back_to_path_when_not_bundled() {
        assert!(beside_exe("definitely-not-bundled-xyz").is_none());
        let mut c = command("definitely-not-bundled-xyz");
        assert_eq!(c.get_program(), "definitely-not-bundled-xyz");
        assert!(c.status().is_err(), "不存在的程序不该能跑起来");
    }

    // 找到了就必须用绝对路径去调，用名字调等于又交回给 PATH
    #[test]
    fn bundled_tool_is_invoked_by_absolute_path() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        let name = "is-fake-tool";
        let path = dir.join(exe_name(name));
        std::fs::write(&path, b"").unwrap();

        let found = beside_exe(name);
        std::fs::remove_file(&path).ok();

        assert_eq!(found.as_deref(), Some(path.as_path()));
        assert!(found.unwrap().is_absolute());
    }
}
