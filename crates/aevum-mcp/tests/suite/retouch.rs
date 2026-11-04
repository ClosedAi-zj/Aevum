//! 修复与几何两个工具的口径：归一化校验、批量原子性、退化四边形、写操作登记。
//!
//! 这里**不**验像素——修复与校正怎么改画面是 `aevum-core` 的 `retouch` 单元测试和
//! `tests/suite/retouch_pipeline.rs` 的事。这一层只回答「agent 通过工具面能不能把
//! 参数设对、设错时会不会被告知」。
//!
//! Conventions of the two repair/geometry tools: normalized validation, batch atomicity,
//! degenerate quads, write-op registration.
//!
//! Pixels are **not** checked here — how repair and correction change the image belongs to
//! `aevum-core`'s `retouch` unit tests and `tests/suite/retouch_pipeline.rs`. This layer only
//! answers "can an agent set the parameters correctly through the tool surface, and is it
//! told when it cannot".

use serde_json::{Value, json};

use aevum_mcp::{dispatch, reset_session};

use super::tools::LOCK;

fn call(tool: &str, args: Value) -> Result<Value, String> {
    dispatch(tool, &args)
}

/// 会话是进程级单例，所有碰它的用例共用同一把锁（见 `tools::LOCK` 的说明）。
///
/// The session is a process-level singleton; every test touching it shares one lock (see
/// `tools::LOCK`).
fn reset() {
    reset_session();
}

#[test]
fn strokes_round_trip_through_add_list_undo_and_clear() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset();

    let added = call(
        "aevum_retouch",
        json!({"op": "add", "spots": [
            {"x": 0.25, "y": 0.4, "r": 0.02},
            {"x": 0.6, "y": 0.5, "r": 0.03, "feather": 0.5, "source": [0.1, 0.0]}
        ]}),
    )
    .expect("加两笔应当成功");
    assert_eq!(added["count"], 2);
    assert_eq!(added["spots"][0]["x"], 0.25);
    // 省略 feather 时按默认 0.35 落盘：默认值本身就是口径的一部分，不能随实现漂。
    //
    // An omitted feather falls back to 0.35: the default is part of the convention and must
    // not drift with the implementation.
    // f32 → JSON 的十进制是 f32 的精确值（0.34999999…），容差按 f32 的精度给。
    //
    // The decimal in JSON is f32's exact value (0.34999999…), so the tolerance follows f32.
    assert!((added["spots"][0]["feather"].as_f64().unwrap() - 0.35).abs() < 1e-6);
    let src = added["spots"][1]["source"]
        .as_array()
        .expect("仿制笔触必须带源偏移");
    assert!((src[0].as_f64().unwrap() - 0.1).abs() < 1e-6);
    assert!(src[1].as_f64().unwrap().abs() < 1e-6);

    let after_undo = call("aevum_retouch", json!({"op": "undo"})).expect("撤一笔");
    assert_eq!(after_undo["count"], 1, "undo 默认只撤一笔");
    assert_eq!(after_undo["spots"][0]["x"], 0.25, "撤掉的是最后一笔");

    let cleared = call("aevum_retouch", json!({"op": "clear"})).expect("清空");
    assert_eq!(cleared["count"], 0);

    // 空表上再撤一次要报错，而不是静默成功——静默成功会让脚本以为还有笔触。
    //
    // Undoing an empty list is an error rather than a silent success: a silent success makes
    // a script believe strokes are still there.
    assert!(call("aevum_retouch", json!({"op": "undo"})).is_err());
}

#[test]
fn out_of_range_coordinates_are_rejected_instead_of_clamped() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset();

    // 像素坐标（x=800）必须被拒：归一化口径是这套参数能在预览与导出之间对上的前提。
    //
    // Pixel coordinates (x=800) must be rejected: normalization is what lets one parameter
    // set line up between preview and export.
    let err = call(
        "aevum_retouch",
        json!({"op": "add", "spots": [{"x": 800, "y": 0.5, "r": 0.02}]}),
    )
    .expect_err("像素坐标应当被拒");
    assert!(err.contains("归一化"), "报错要说清口径：{err}");

    let err = call(
        "aevum_retouch",
        json!({"op": "add", "spots": [{"x": 0.5, "y": 0.5, "r": 0.5}]}),
    )
    .expect_err("超上限的半径应当被拒");
    assert!(err.contains("r 必须"), "报错要指明字段：{err}");

    let err = call(
        "aevum_retouch",
        json!({"op": "add", "spots": [{"x": 0.5, "y": 0.5, "r": 0.02, "source": [0.0, 0.0]}]}),
    )
    .expect_err("零偏移的仿制应当被拒");
    assert!(err.contains("原地复制"), "报错要说清为什么：{err}");

    let list = call("aevum_retouch", json!({"op": "list"})).expect("列一下");
    assert_eq!(list["count"], 0, "被拒的笔触不许留下痕迹");
}

/// 批量里有一笔非法 → 整批不落地。半批落地比整批失败更难查：调用方会以为改完了。
///
/// One illegal stroke in a batch means the whole batch is dropped. A half-applied batch is
/// harder to diagnose than a failure: the caller believes it is done.
#[test]
fn a_batch_with_one_bad_stroke_writes_nothing() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset();

    let err = call(
        "aevum_retouch",
        json!({"op": "add", "spots": [
            {"x": 0.2, "y": 0.2, "r": 0.02},
            {"x": 0.5, "y": 0.5, "r": 0.9}
        ]}),
    )
    .expect_err("第二笔超限，整批应当失败");
    assert!(err.contains("spots[1]"), "报错要指名第几笔：{err}");

    let list = call("aevum_retouch", json!({"op": "list"})).expect("列一下");
    assert_eq!(list["count"], 0, "第一笔也不许落地");
}

#[test]
fn upright_set_get_and_clear_round_trip() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset();

    let fresh = call("aevum_upright", json!({})).expect("默认是 get");
    assert!(fresh["upright"].is_null());
    assert_eq!(fresh["identity"], true);

    let set = call(
        "aevum_upright",
        json!({"op": "set", "quad": [[0.05, 0.0], [0.95, 0.0], [1.0, 1.0], [0.0, 1.0]]}),
    )
    .expect("设一个梯形");
    assert_eq!(set["identity"], false);
    assert_eq!(set["upright"][0], json!([0.05, 0.0]));

    let cleared = call("aevum_upright", json!({"op": "clear"})).expect("清除");
    assert!(cleared["upright"].is_null());
    assert_eq!(cleared["identity"], true);
}

#[test]
fn a_degenerate_quad_is_refused_before_it_reaches_the_renderer() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset();

    // 四点共线：面积为零，单应求不出来。放行等于让渲染阶段静默不动作。
    //
    // Four collinear points: zero area, no homography. Letting it through would make the
    // render stage silently do nothing.
    let err = call(
        "aevum_upright",
        json!({"op": "set", "quad": [[0.0, 0.5], [0.3, 0.5], [0.6, 0.5], [1.0, 0.5]]}),
    )
    .expect_err("退化四边形应当被拒");
    assert!(err.contains("退化"), "报错要点明原因：{err}");

    // 越界也要拒：调用方给的是像素比例而不是归一化时，越界是唯一的外在症状。
    //
    // Out-of-range values are rejected too: when a caller passes pixel ratios instead of
    // normalized ones, being out of range is the only visible symptom.
    assert!(
        call(
            "aevum_upright",
            json!({"op": "set", "quad": [[0.0, 0.0], [1.5, 0.0], [1.0, 1.0], [0.0, 1.0]]}),
        )
        .is_err()
    );

    let after = call("aevum_upright", json!({"op": "get"})).expect("get");
    assert!(after["upright"].is_null(), "被拒的四边形不许留下痕迹");
}

/// 两个工具都是写操作：不进 op_journal 的话，出问题时没法复盘「谁在什么时候改了画面」。
///
/// Both tools are write operations: without an op_journal entry there is no way to reconstruct
/// who changed the frame and when.
#[test]
fn both_tools_are_registered_as_writes() {
    for tool in ["aevum_retouch", "aevum_upright"] {
        assert!(
            aevum_core::journal::is_write_op(tool),
            "{tool} 没登记为写操作，审计日志会缺行"
        );
    }
}
