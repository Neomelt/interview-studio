// Windows 从可执行文件的资源段读图标和版本信息。不嵌进去，任务栏、Alt-Tab
// 和资源管理器显示的就是默认的白纸图标——运行时用 with_icon 设的那个只管
// 窗口本身，管不到这些。
//
// 按「宿主是不是 Windows」而不是「目标是不是 Windows」来判断：资源编译要
// 调用 rc.exe，从 Linux 交叉编译时没有它。发行版是在 windows runner 上构建
// 的，所以图标一定嵌得上；本地交叉 check 只是少个图标，不影响编译。
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../packaging/icons/icon.ico");
        winresource::WindowsResource::new()
            .set_icon("../../packaging/icons/icon.ico")
            .set("ProductName", "Interview Studio")
            .set("FileDescription", "Interview Studio")
            .set("LegalCopyright", "MIT (c) 2026 Neomelt")
            .compile()
            .expect("嵌入 Windows 资源失败");
    }
}
