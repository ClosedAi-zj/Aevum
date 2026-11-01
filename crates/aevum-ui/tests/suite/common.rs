//! 判据之间必须串行的那点东西：当前语言 + 缺译探针。
//!
//! # 为什么要有这把锁
//!
//! `i18n::current()` 是进程级原子，`i18n::take_untranslated` 的探针也是进程级静态。
//! 以前每个测试文件各成一个可执行文件，进程天然隔离，谁都不用管；现在合并成一个
//! （见台账 C1），`i18n_coverage` 正在画界面的时候 `i18n_errors` 可能正好把语言切成
//! 中文，两边都读到一个不属于自己的状态。
//!
//! 用法是**一把锁管两件事**，而不是各管一件：探针和环境语言描述的是同一个全局前提，
//! 用两把锁会出现「拿了探针锁、语言被另一个测试改掉」的缝。
//!
//! `unwrap_or_else(|e| e.into_inner())`：一条判据 panic 会毒化锁，后一条判据要能继续
//! 跑并报出自己的问题，而不是一起变成 `PoisonError`。
//!
//! # Why this lock exists
//!
//! `i18n::current()` is a process-wide atomic and the missing-translation probe behind
//! `i18n::take_untranslated` is a process-wide static. Each test file used to be its own
//! executable, so processes isolated them for free; now they are merged into one (ledger
//! item C1), and `i18n_errors` could switch the language to Chinese while
//! `i18n_coverage` is painting the UI -- both reading a state that is not theirs.
//!
//! One lock covers **both** concerns rather than one each: the probe and the ambient
//! language describe the same global precondition, and two locks would leave a window
//! where the probe is held while another test switches the language.
//!
//! `unwrap_or_else(|e| e.into_inner())`: a panicking test poisons the mutex, and the next
//! test must still run and report its own failure instead of turning into `PoisonError`.

use std::sync::{Mutex, MutexGuard};

static I18N: Mutex<()> = Mutex::new(());

/// 拿住「语言 + 探针」这把锁。守卫活到作用域结束。
///
/// Take the shared "language + probe" lock; the guard lives to the end of the scope.
pub fn i18n_guard() -> MutexGuard<'static, ()> {
    I18N.lock().unwrap_or_else(|e| e.into_inner())
}
