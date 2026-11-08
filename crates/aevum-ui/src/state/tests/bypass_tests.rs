//! 「眼睛」旁路的判据（评审 U1）。
//!
//! 旁路的一切行为都从两条公理推出来，判据也钉在这两条上：
//! ① **眼睛 = 复位**——旁路看到的画面必须与按分区头复位之后完全一致，
//! 否则对比在撒谎；② **旁路只属于预览**——`params` 本体一字不动，导出照旧。
//!
//! Judges for the "eye" bypass (review item U1).
//!
//! Every behaviour follows from two axioms, and the judges pin exactly those:
//! ① **eye == reset** -- what the bypass shows must be identical to what the section
//! header's reset produces, otherwise the comparison lies; ② **bypass belongs to the
//! preview only** -- `params` itself is untouched, exports render as always.

use super::*;
use crate::controls;

/// 显影八组：旁路的全部合法对象。
const DEVELOP: [&str; 8] = [
    "basic", "curve", "mixer", "grade", "detail", "optics", "effects", "look",
];

/// 把全部推子推到一个非默认位置，外加表外字段（曲线点、外观路径）——
/// 制造「最坏情况」：所有东西都动过，再看旁路能不能全部收回来。
///
/// Push every slider off its default, plus the out-of-table fields (curve points, the
/// look path) -- the worst case: everything moved, then check bypass winds it all back.
fn perturb_everything(p: &mut GradeParams) {
    for s in controls::SPECS.iter() {
        *controls::param_slot(p, s.field) = s.max * 0.8;
    }
    p.look_cube = "probe.cube".to_owned();
    p.par_split1 = 40.0;
    p.lv_gamma = 1.4;
    p.curve_rgb = vec![[0.1, 0.9], [0.2, 0.8], [0.3, 0.7], [0.4, 0.6]];
    p.grade_sh_h = 30.0;
    p.grade_hi_s = 60.0;
}

/// 公理①的眼睛面：对一个「全都动过」的参数，逐组旁路必须逐字段等于
/// 同一组上按复位按钮的结果。复位如果将来长出旁路不知道的字段，这里红。
///
/// The eye side of axiom ①: starting from fully perturbed parameters, bypassing one
/// group must equal pressing that group's reset button, field for field. If reset ever
/// grows a field bypass does not know about, this goes red.
#[test]
fn bypassing_a_group_equals_resetting_it() {
    for g in DEVELOP {
        let mut a = GradeParams::default();
        perturb_everything(&mut a);
        let mut st = AppState::new();
        st.params = a;
        crate::shell::reset_group(&mut st, g);

        let mut b = GradeParams::default();
        perturb_everything(&mut b);
        let mut st2 = AppState::new();
        st2.params = b;
        st2.bypassed.insert(g);
        let via_eye = st2.preview_params();

        assert_eq!(
            via_eye.look_cube, st.params.look_cube,
            "{g}: 外观路径在两种口径下不一致"
        );
        for s in controls::SPECS.iter() {
            assert_eq!(
                controls::param_get(&via_eye, s.field),
                controls::param_get(&st.params, s.field),
                "{g}: 字段 {} 在「旁路」与「复位」下不一致",
                s.key
            );
            if s.group == g {
                // 不只跟复位比，还跟出厂默认比：复位和旁路共用一份实现，
                // 若那份实现漏写一个字段，两边会「一致地错」——这一句堵住它。
                //
                // Not only equal to reset but equal to factory defaults: reset and bypass
                // share one implementation, and a missing field there would make both
                // "consistently wrong" -- this line closes that hole.
                assert_eq!(
                    controls::param_get(&via_eye, s.field),
                    s.default,
                    "{g}: 字段 {} 旁路后没回到默认",
                    s.key
                );
            }
        }
        // 表外字段同样直比默认。
        let d = GradeParams::default();
        match g {
            "curve" => assert_eq!(via_eye.lv_gamma, d.lv_gamma, "curve: 色阶没回默认"),
            "mixer" => assert_eq!(via_eye.hsl_h, d.hsl_h, "mixer: HSL 没回默认"),
            "grade" => assert_eq!(via_eye.grade_sh_h, d.grade_sh_h, "grade: 分级没回默认"),
            "look" => assert!(via_eye.look_cube.is_empty(), "look: 外观没卸"),
            _ => {}
        }
    }
}

/// 公理②：旁路只改 `preview_params()` 的返回值，`params` 本体一字不动——
/// 导出、工程保存、预设全都读本体。
///
/// Axiom ②: bypass only changes what `preview_params()` returns; `params` itself stays
/// byte-identical -- export, project save and presets all read the body.
#[test]
fn bypass_never_touches_the_params_itself() {
    let mut st = AppState::new();
    perturb_everything(&mut st.params);
    let before = st.params.clone();
    st.bypassed.insert("basic");
    st.bypassed.insert("effects");
    let preview = st.preview_params();
    // 预览里那两组真的被收掉了……
    assert_ne!(preview.exposure, st.params.exposure);
    assert_eq!(preview.grain, 0.0);
    // ……而本体一动没动。
    assert_eq!(st.params, before);
}

/// 旁路一个组只收那一个组：其它组动了的东西必须原样保留。
///
/// Bypassing one group winds back only that group: everything the others moved stays.
#[test]
fn bypass_leaves_other_groups_alone() {
    let mut st = AppState::new();
    perturb_everything(&mut st.params);
    st.bypassed.insert("curve");
    let preview = st.preview_params();
    assert_eq!(
        preview.exposure, st.params.exposure,
        "basic 不该被 curve 的旁路收掉"
    );
    assert_eq!(preview.temperature, st.params.temperature);
    assert_eq!(preview.grain, st.params.grain);
    assert_eq!(preview.look_cube, st.params.look_cube);
    // 被 旁路的那一组自己回到默认。
    assert_eq!(preview.lv_gamma, GradeParams::default().lv_gamma);
}

/// 旁路对象恰为显影八组：序列、成片、音频那些组的参数不在调色管线里，
/// 误给它们出眼睛按钮等于提供一个什么都不做的开关。
///
/// The bypassable set is exactly the Develop eight: sequence, render and audio groups
/// keep their parameters outside the grading pipeline, and an eye button on them would
/// be a switch that does nothing.
#[test]
fn only_the_develop_eight_are_bypassable() {
    for g in DEVELOP {
        assert!(controls::is_bypassable(g), "{g} 应可旁路");
    }
    for g in [
        "seq",
        "seq-cull",
        "seq-qa",
        "seq-align",
        "seq-spec",
        "seq-issues",
        "seq-align-report",
        "deflicker",
        "def-stats",
        "def-exposure",
        "hdr",
        "pano",
        "stack",
        "timeblend",
        "focus",
        "frame",
        "frame-guides",
        "render-out",
        "render-watermark",
        "render-time",
        "render-retime",
        "render-motion",
        "render-codec",
        "render-export",
        "render-queue",
        "audio",
        "audio-time",
        "audio-mix",
        "info-meta",
        "keys",
        "ai-link",
        "masks",
    ] {
        assert!(!controls::is_bypassable(g), "{g} 不应可旁路");
    }
}

/// 旁路是内存态：`AppState::new()` 出来必须为空，重启即清空的行为由这里钉住。
///
/// The bypass is memory-only: a fresh `AppState` must start empty -- this pins the
/// "cleared on restart" behaviour.
#[test]
fn a_fresh_state_has_no_bypass() {
    let st = AppState::new();
    assert!(st.bypassed.is_empty());
    assert_eq!(st.preview_params(), st.params);
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aevum-ui-bypass-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 接线判据：`ensure_preview` 发出去的预览必须真的按旁路后的参数渲染。
/// 曝光推到 +3 EV 再旁路 basic，收到的预览必须与「默认参数直渲」逐点一致；
/// 接线一旦退回 `self.params`，收到的预览会整体变亮，这条当场红。
///
/// Wiring judge: the preview job `ensure_preview` posts must really render the
/// bypassed parameters. Exposure at +3 EV with Basic bypassed must come back
/// point-identical to a direct render at defaults; if the wiring ever falls back to
/// `self.params`, the preview comes back brighter and this goes red.
#[test]
fn the_preview_job_renders_the_bypassed_params() {
    let dir = temp_dir("wiring");
    // 左暗右亮的灰阶：曝光一拉，整幅的差别远超任何抖动。
    //
    // A left-dark right-bright ramp: one pull of exposure moves the whole frame far
    // beyond any dither.
    let (w, h) = (64usize, 48usize);
    let mut img = image::RgbImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let v = (x * 255 / (w - 1)) as u8;
            img.put_pixel(x as u32, y as u32, image::Rgb([v, v, v]));
        }
    }
    img.save(dir.join("f001.png")).expect("写测试图");

    let mut st = AppState::new();
    st.seq = Some(aevum_core::sequence::Sequence::open(&dir).expect("开序列"));
    st.params = GradeParams::default();
    st.params.exposure = 3.0;
    st.bypassed.insert("basic");
    st.preview_dirty = true;
    st.ensure_preview(&egui::Context::default());

    let mut msg = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while msg.is_none() && std::time::Instant::now() < deadline {
        match st.rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Msg::Preview(p)) => msg = Some(*p),
            Ok(_) => continue,
            Err(e) => panic!("预览线程没回话：{e}"),
        }
    }
    let got = msg.expect("30 秒内没等到预览回包");

    let path = dir.join("f001.png");
    let want = render_preview_file(
        &path,
        &GradeParams::default(),
        Ev::NONE,
        None,
        None,
        Framing::default(),
        None,
        false,
        false,
        false,
        false,
    )
    .expect("直渲基准");
    assert_eq!(got.graded.size, want.0.size, "预览尺寸与直渲不一致");
    let worst = got
        .graded
        .pixels
        .iter()
        .zip(want.0.pixels.iter())
        .map(|(a, b)| {
            (0..3)
                .map(|i| (a[i] as i32 - b[i] as i32).abs())
                .max()
                .unwrap()
        })
        .max()
        .unwrap();
    assert!(
        worst <= 1,
        "旁路后的预览与默认参数直渲差了 {worst} 级——预览没有按旁路参数渲染"
    );
    // 反向钉死：不旁路时同一帧必须明显不同（否则上面的一致是两张黑图凑巧相等）。
    //
    // Pin the other direction: without the bypass the same frame must differ clearly,
    // otherwise the equality above could be two coincidentally-black images.
    let direct = render_preview_file(
        &path,
        &st.params,
        Ev::NONE,
        None,
        None,
        Framing::default(),
        None,
        false,
        false,
        false,
        false,
    )
    .expect("带曝光直渲");
    let spread = got
        .graded
        .pixels
        .iter()
        .zip(direct.0.pixels.iter())
        .map(|(a, b)| {
            (0..3)
                .map(|i| (a[i] as i32 - b[i] as i32).abs())
                .max()
                .unwrap()
        })
        .max()
        .unwrap();
    assert!(
        spread > 16,
        "带 +3 EV 的直渲与旁路预览只差 {spread} 级，反向对照失效"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
