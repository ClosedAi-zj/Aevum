//! 老式界面布局：调色盘贴右边缘、直方图回收调色盘、底部图片列表铺满整条。
//!
//! 三条都只是把**既有组件**改成贴边，不新增任何控件：
//! * 调色盘：`dock_panel` 钉成右边缘固定列，直方图收在页签行下面
//!   （旧版稿 `.dock-tabs` → `.histo`），画布左上不再摆浮卡；
//! * 底部：`timeline::card` 的贴边形态，宽度钉成整条可用宽度，
//!   横贯到调色盘左缘（旧版稿 `.film{height:120px}`），
//!   不再是现代布局那种左下角一小块浮卡、右边空一大片。
//!
//! 由 `layout::Scheme` 切换，默认现代；老式只在用户明确启用时生效，
//! 因此默认界面与全部既有判据完全不受影响。
//!
//! Legacy interface layout: the palette docks at the right edge, the histogram
//! moves back inside it, and the bottom image list spans the full width.
//!
//! All three changes merely dock **existing widgets**, adding none:
//! * Palette: `dock_panel` pinned as a right-edge column, with the histogram
//!   back under its tab row (old mockup `.dock-tabs` -> `.histo`); no floating
//!   histogram card on the canvas.
//! * Bottom: the docked form of `timeline::card`, width pinned to the full
//!   available width so it reaches the palette's left edge (old mockup
//!   `.film{height:120px}`), instead of Modern's small corner card with a
//!   large empty gap to its right.
//!
//! `layout::Scheme` switches the two paths. Modern is the default, so the
//! stock UI and every existing assertion are untouched until the user opts in.

use egui::{Context, Ui};

use crate::state::AppState;

/// 右侧贴边调色盘：直接复用检查器的 `dock_panel`，页签与参数组一字不改。
///
/// 现代布局里这份内容由 `floating_dock` 包成浮卡；老式布局把它钉在右边缘，
/// 于是「右侧调色盘」是真的贴边列，而不是飘在照片上的卡片。底部胶片条
/// 由 `shell::mod` 照常绘制，这里不碰。
///
/// The right-hand docked palette reuses the inspector's `dock_panel` verbatim.
/// The modern layout wraps that content in a floating card; Legacy pins it to
/// the right edge, so "palette on the right" is a real docked column rather
/// than a card drifting over the photo. The bottom filmstrip is drawn as
/// usual by `shell::mod` and is not touched here.
pub(super) fn palette_column(state: &mut AppState, ui: &mut Ui, width: f32, max_h: f32) {
    super::dock::dock_panel(state, ui, width, max_h);
}

/// 底部铺满的图片列表预览：直接复用现代浮卡那张 `timeline::card` 的贴边形态，
/// 头行 / 曲线 / 缩略图 / 走带一字不改，只是宽度钉成整条可用宽度、不画浮卡投影。
/// 这就是旧版稿 `.film{height:120px}` 的读法，没有新增任何控件。
///
/// Bottom image list spanning the full width: it reuses the same `timeline::card`
/// as the modern floating card, in its docked form -- header, curve, thumbnails
/// and transport are unchanged, only the width is pinned to the full available
/// width and the floating shadow is dropped. This is the old mockup's
/// `.film{height:120px}` reading, with no new widgets.
pub(super) fn film_strip(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    super::timeline::card(state, ui, ctx, ui.available_width(), true, false);
}
