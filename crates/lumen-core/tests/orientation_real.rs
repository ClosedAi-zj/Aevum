//! 真机样本的方向验证：拿拍摄原片盘里的真实文件走一遍完整读图入口。
//!
//! 合成测试拦不住这一类问题（U2 的 RAW bug 就是合成全绿、真机才炸），所以留一个
//! 可选的真实文件入口。没给环境变量就跳过 —— CI 上没有原片盘。
//!
//! ```bash
//! LUMEN_REAL_RAW='F:\照片\...\PANA2495.RW2' \
//!   cargo test -p lumen-core --test orientation_real -- --nocapture
//! ```

use std::path::Path;

/// 6/8 是 ±90°、5/7 是 ±90° 再加镜像：这四种方向下，转正后的画面必须换长短边。
fn needs_transpose(orientation: u8) -> bool {
    matches!(orientation, 5..=8)
}

#[test]
fn real_sample_gets_transposed_on_read() {
    let Some(raw) = std::env::var_os("LUMEN_REAL_RAW") else {
        eprintln!("没设 LUMEN_REAL_RAW：跳过真机样本（CI 上没有原片盘）");
        return;
    };
    let path = Path::new(&raw);
    assert!(path.is_file(), "样本不存在：{}", path.display());

    let exif = lumen_core::exif::read(path);
    eprintln!(
        "样本 {}：exif.orientation={} make={:?} model={:?}",
        path.file_name().unwrap().to_string_lossy(),
        exif.orientation,
        exif.make(),
        exif.model()
    );
    assert_ne!(
        exif.orientation, 1,
        "真机竖拍样本读到方向 1，等于「不转」——方向通路又断了（RW2/ORF/NEF 的 magic 不在白名单里）"
    );

    let img = lumen_core::metrics::decode(path).expect("解码失败");
    let (w, h) = (img.width(), img.height());
    eprintln!("metrics::decode 之后：{w}x{h}");
    if needs_transpose(exif.orientation) {
        assert!(
            h > w,
            "方向 {} 需要转 90°，转正后应该高大于宽，实际 {w}x{h}（说明方向没真的落到像素上）",
            exif.orientation
        );
    }
}
