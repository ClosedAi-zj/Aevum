//! 真机 lensfun 数据库验证：合成 XML 拦不住解析器对真实 lensfun 语法的疏漏
//! （与 orientation_real 同理——「合成全绿、真机才炸」的那类问题）。没给
//! LUMEN_LENSFUN 就跳过，CI 上没有数据库。
//!
//! ```bash
//! LUMEN_LENSFUN='D:\work\samples\lumen\lensfun\data\db' \
//!   cargo test -p lumen-core --test lensfun_real -- --nocapture
//! ```

use lumen_core::lensdb;

#[test]
fn real_lensfun_db_parses_and_matches_panasonic() {
    let Some(root) = std::env::var_os("LUMEN_LENSFUN") else {
        eprintln!("没设 LUMEN_LENSFUN：跳过真机 lensfun 库（CI 上没有数据库）");
        return;
    };
    eprintln!("LUMEN_LENSFUN = {}", root.to_string_lossy());

    let files = lensdb::find_database_files();
    assert!(
        !files.is_empty(),
        "LUMEN_LENSFUN 指向的目录里没找到 *.xml：{}",
        root.to_string_lossy()
    );

    let (db, ok) = lensdb::load_files(&files);
    // 关键回归点：最小 XML 解析器必须吃下每一个真实 lensfun 文件。坏文件是
    // 静默跳过的，所以这里显式核对「解析成功数 == 文件数」。
    assert_eq!(
        ok,
        files.len(),
        "有 {} 个真实 lensfun 文件没解析成功（最小 XML 解析器漏了某种语法）",
        files.len() - ok
    );
    eprintln!(
        "解析成功 {ok} 个文件：机身 {} 条、镜头 {} 条",
        db.cameras.len(),
        db.lenses.len()
    );
    assert!(!db.cameras.is_empty(), "真实库里没有机身条目");
    assert!(!db.lenses.is_empty(), "真实库里没有镜头条目");

    // 机身：compact-panasonic.xml 里有 DMC-LX3。
    let cam = lensdb::find_camera(&db, "Panasonic", "DMC-LX3")
        .expect("按 EXIF 口径应能找到 Panasonic DMC-LX3");
    eprintln!("命中机身 {} {} crop={:?}", cam.maker, cam.model, cam.crop);
    assert!(cam.crop.is_some(), "DMC-LX3 应带裁切系数");

    // 镜头：真实库里找一支带畸变标定的松下镜头，走完整 match_correction。
    let pana = db
        .lenses
        .iter()
        .find(|l| l.maker.to_lowercase().contains("panasonic") && !l.dist.is_empty())
        .expect("真实库里应至少有一支带畸变标定的松下镜头");
    let focal = pana
        .dist
        .iter()
        .map(|d| d.focal)
        .min_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap();
    eprintln!(
        "命中镜头 {} {}（畸变标定 {} 档，最短焦距 {focal}mm）",
        pana.maker,
        pana.model,
        pana.dist.len()
    );

    let corr = lensdb::match_correction(&db, "Panasonic", "DMC-LX3", &pana.model, focal, None)
        .expect("match_correction 应对真实松下镜头折出校正");
    assert!(!corr.is_empty(), "折出的校正是空的");
    eprintln!("校正 to_json = {}", corr.to_json());
}
