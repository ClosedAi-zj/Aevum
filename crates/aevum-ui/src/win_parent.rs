//! 系统对话框的属主窗口。
//!
//! 十个系统对话框（打开照片/序列、XMP/LUT/音轨、输出目录、另存为）此前全部
//! `Show(NULL)` 弹出——rfd 没拿到属主句柄时就是这么做的。2026-11-08 实测的
//! 后果有三个，都能用 `GetWindowRect` 复现：
//!
//! 1. 对话框弹到**另一台显示器**（应用在主屏 (78,78)，对话框在 (4496,527)）；
//! 2. 对话框打开期间**主窗口被整体隐藏**（`IsWindowVisible=false`，非最小化、
//!    非被遮挡），关掉对话框才回来——用户看到的就是"窗口出现异常"；
//! 3. UI 线程在 winit 事件处理内被对话框的模态循环重入（craft-dialogs 与
//!    egui discussion #1740 都记录过这类危害）。
//!
//! 修法是 egui 社区同一问题的标准答案：给对话框 `set_parent(主窗口)`。Windows
//! 对有属主的对话框会做正确的模态管理——居中在属主上、永远在属主前、属主被
//! disable 而不是被藏起来。
//!
//! Parent window for system dialogs.
//!
//! All ten system dialogs used to pop up via `Show(NULL)` -- that is what rfd does
//! when it has no parent handle. Measured on 2026-11-08 with `GetWindowRect`:
//! the dialog landed on another monitor while the app sat on the primary one, and
//! the main window was **hidden outright** while the dialog was open, coming back
//! only after it closed. The fix is the community answer to the same problem:
//! `set_parent(main window)`, so Windows manages the dialog as owned -- centred on
//! the owner, always in front, owner disabled rather than hidden.

use std::sync::OnceLock;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};

/// 属主句柄。只在拿到 Win32 句柄时构造；其他平台保持 `None`，
/// 对话框维持原行为（rfd 对非 Win32 句柄会直接 `unreachable!`）。
///
/// The parent handle. Constructed only for a Win32 handle; other platforms stay
/// `None` and keep the previous dialog behaviour (rfd hits `unreachable!` on any
/// non-Win32 handle).
pub struct Parent {
    raw: RawWindowHandle,
    display: raw_window_handle::RawDisplayHandle,
}

// 安全性：句柄指向的窗口与应用同寿命，`OnceLock` 持有它直到进程结束；
// `borrow_raw` 只是把这段生命周期显式交给调用方。
//
// Safety: the handle's window lives as long as the app, and the `OnceLock` holds it
// until the process ends; `borrow_raw` merely hands that lifetime to the caller.
impl HasWindowHandle for Parent {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.raw) })
    }
}

impl HasDisplayHandle for Parent {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(self.display) })
    }
}

// 安全性：Win32 的 HWND / HMONITOR 是与线程无关的整数句柄，跨线程持有与使用都合法；
// 窗口本身与应用同寿命，进程结束才释放。
//
// Safety: Win32 HWND / HMONITOR handles are thread-agnostic integers; holding and using
// them from any thread is legal, and the window lives until the process ends.
unsafe impl Send for Parent {}
unsafe impl Sync for Parent {}

static PARENT: OnceLock<Parent> = OnceLock::new();

/// 启动时挂一次：`CreationContext` 本身实现了两个 Has* trait，直接借它的句柄。
///
/// Called once at startup: `CreationContext` implements both `Has*` traits, so its
/// handles are borrowed directly.
pub fn init(cc: &eframe::CreationContext<'_>) {
    let Ok(w) = cc.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(_) = w.as_raw() else {
        return;
    };
    let Ok(d) = cc.display_handle() else {
        return;
    };
    let _ = PARENT.set(Parent {
        raw: w.as_raw(),
        display: d.as_raw(),
    });
}

/// 有属主就返回它；调用方据此决定要不要 `set_parent`。
///
/// The parent, if any; callers use this to decide whether to `set_parent`.
pub fn parent() -> Option<&'static Parent> {
    PARENT.get()
}

/// 带属主的新建文件对话框：全部调用点必须走这里，不许直接 `rfd::FileDialog::new()`。
///
/// A new file dialog with the parent attached: every call site goes through here,
/// never a bare `rfd::FileDialog::new()`.
pub fn file_dialog() -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new();
    if let Some(p) = parent() {
        d = d.set_parent(p);
    }
    d
}
