//! 选区几何与 Python `lapse/crop.py` 的基准比对。
//!
//! 基准由 `oracle/freeze_crop.py` 生成（462 条随机用例，覆盖八个把手、
//! 七种比例、六种画幅）。改几何先跑这个，别凭手感调。

use aevum_core::align::CropRect;
use aevum_core::crop::{
    Edges, Handle, Hit, center_rect, draw_rect, hit_test, move_rect, norm_rect, nudge_rect,
    refit_rect, resize_rect,
};
use serde_json::Value;

fn golden() -> Value {
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle/golden/crop.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到基准 {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("基准 crop.json 不是合法 JSON: {e}"))
}

fn frame(v: &Value) -> (u32, u32) {
    let f = v["frame"].as_array().unwrap();
    (f[0].as_u64().unwrap() as u32, f[1].as_u64().unwrap() as u32)
}

fn rect(v: &Value) -> CropRect {
    let r = v.as_array().unwrap();
    CropRect {
        x: r[0].as_u64().unwrap() as u32,
        y: r[1].as_u64().unwrap() as u32,
        w: r[2].as_u64().unwrap() as u32,
        h: r[3].as_u64().unwrap() as u32,
    }
}

fn aspect(v: &Value) -> Option<(u32, u32)> {
    v.as_array()
        .map(|a| (a[0].as_u64().unwrap() as u32, a[1].as_u64().unwrap() as u32))
}

fn point(v: &Value) -> (f64, f64) {
    (v[0].as_f64().unwrap(), v[1].as_f64().unwrap())
}

fn handle(v: &Value) -> Handle {
    match v.as_str().unwrap() {
        "nw" => Handle::Nw,
        "n" => Handle::N,
        "ne" => Handle::Ne,
        "e" => Handle::E,
        "se" => Handle::Se,
        "s" => Handle::S,
        "sw" => Handle::Sw,
        "w" => Handle::W,
        other => panic!("未知把手 {other}"),
    }
}

fn check(name: &str, cases: &[Value], got: impl Fn(&Value) -> CropRect, want: &str) {
    for (i, c) in cases.iter().enumerate() {
        let a = got(c);
        let e = rect(&c["out"]);
        assert_eq!(
            a, e,
            "{name}[{i}] {want}\n  用例 {c}\n  期望 {e:?}\n  实得 {a:?}"
        );
    }
    println!("  {name}: {} 条一致", cases.len());
}

#[test]
fn crop_geometry_matches_the_oracle() {
    let doc = golden();
    let cases = doc["cases"].as_object().unwrap();
    let each = |k: &str| cases[k].as_array().unwrap().clone();

    check(
        "norm",
        &each("norm"),
        |c| {
            norm_rect(
                c["x0"].as_f64().unwrap(),
                c["y0"].as_f64().unwrap(),
                c["x1"].as_f64().unwrap(),
                c["y1"].as_f64().unwrap(),
                frame(c).0,
                frame(c).1,
            )
        },
        "整理",
    );
    check(
        "center",
        &each("center"),
        |c| {
            let (fw, fh) = frame(c);
            center_rect(fw, fh, aspect(&c["aspect"]))
        },
        "居中",
    );
    check(
        "refit",
        &each("refit"),
        |c| {
            let (fw, fh) = frame(c);
            refit_rect(rect(&c["sel"]), fw, fh, aspect(&c["aspect"]))
        },
        "换比例",
    );
    check(
        "move",
        &each("move"),
        |c| {
            let (fw, fh) = frame(c);
            let (x, y) = point(&c["at"]);
            move_rect(rect(&c["sel"]), x, y, fw, fh)
        },
        "平移",
    );
    check(
        "draw",
        &each("draw"),
        |c| {
            let (fw, fh) = frame(c);
            let (ax, ay) = point(&c["anchor"]);
            let (px, py) = point(&c["at"]);
            draw_rect((ax, ay), px, py, fw, fh, aspect(&c["aspect"]))
        },
        "拖新框",
    );
    check(
        "resize",
        &each("resize"),
        |c| {
            let (fw, fh) = frame(c);
            let (px, py) = point(&c["at"]);
            resize_rect(
                handle(&c["handle"]),
                rect(&c["sel"]),
                px,
                py,
                fw,
                fh,
                aspect(&c["aspect"]),
            )
        },
        "缩放",
    );
    check(
        "nudge",
        &each("nudge"),
        |c| {
            let (fw, fh) = frame(c);
            let d = c["d"].as_array().unwrap();
            nudge_rect(
                rect(&c["sel"]),
                d[0].as_i64().unwrap(),
                d[1].as_i64().unwrap(),
                c["step"].as_u64().unwrap() as u32,
                fw,
                fh,
            )
        },
        "微调",
    );

    // 命中检测：Python 返回字符串，这里换成 enum 再比。
    let hits = each("hit");
    for (i, c) in hits.iter().enumerate() {
        let r = c["rect"].as_array().unwrap();
        let e = Edges::new(
            r[0].as_f64().unwrap() as f32,
            r[1].as_f64().unwrap() as f32,
            r[2].as_f64().unwrap() as f32,
            r[3].as_f64().unwrap() as f32,
        );
        let (px, py) = point(&c["at"]);
        let got = hit_test(e, px as f32, py as f32, 8.0, 5.0);
        let want = c["out"].as_str().unwrap();
        let text = match got {
            Hit::Corner(h) => format!("corner:{}", label(h)),
            Hit::Edge(h) => format!("edge:{}", label(h)),
            Hit::Inside => "in".to_owned(),
            Hit::Outside => "out".to_owned(),
        };
        assert_eq!(text, want, "hit[{i}] 用例 {c}");
    }
    println!("  hit: {} 条一致", hits.len());
}

fn label(h: Handle) -> &'static str {
    match h {
        Handle::Nw => "nw",
        Handle::N => "n",
        Handle::Ne => "ne",
        Handle::E => "e",
        Handle::Se => "se",
        Handle::S => "s",
        Handle::Sw => "sw",
        Handle::W => "w",
    }
}
