#!/usr/bin/env python3
"""从 bak.rs 重建 shell/ 模块目录：步1(mask/adjust/common) + 步2(视图区七件)
+ 步3(library/tabs/windows) + mod.rs 定稿 + 完整 use 树回填（带括号校验）。

用法：python tools/rebuild_split.py
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
from pathlib import Path

# 仓库根从脚本自身位置推出来，不写死盘符和目录名（改名不该让工具失效）。
ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "crates/aevum-ui/src"
SHELL = SRC / "shell"
BAK = ROOT / "tools/_reconstructed_shell.bak.rs"

DECL = re.compile(r"^(?:(?:pub(?:\((?:crate|super|in [a-z_:]+)\))?\s+)?"
                  r"(?:fn|struct|enum|const|static|mod|impl|trait|use)\b)")

STEP1 = {"mask", "common", "adjust"}
STEP2 = {"chrome", "menu", "dialogs", "toolbar", "canvas", "timeline", "dock"}
STEP3 = {"library", "tabs", "windows"}

MOD_DOCS = {
    "chrome": "窗口镶边：拖拽缩放区、三枚窗口按钮（mac 红绿灯 / Win 几何）。",
    "menu": "顶部菜单行与全局输入：菜单/菜单项/四个下拉、键盘与滚轮分发。",
    "dialogs": "系统对话框：打开照片/序列、XMP 导入导出、.cube 导入导出、音轨选择。",
    "toolbar": "选项栏/文档页签/工具轨/状态栏（界面顶部与左右竖条）。",
    "canvas": "画布区：图像绘制、裁剪/运动叠加、手柄、标尺与空态。",
    "timeline": "底部两行：时间线、曲线、色标、胶片条与走带控制。",
    "dock": "右坞骨架：页签容器、直方图与平滑辅助。",
    "library": "图库：目录树/筛选/元数据/目录视图/堆叠与关键字应用。",
    "tabs": "信息/快捷键/AI/历史四个页签。",
    "windows": "浮动窗口：菜单段/内置动作/插件菜单/市场/调色窗口/关于。",
}


def run(*args: str) -> int:
    print("+", " ".join(args))
    r = subprocess.run(args, cwd=str(ROOT))
    return r.returncode


def item_names(path: Path) -> list[str]:
    sys.path.insert(0, str(ROOT / "tools"))
    from move_audit import split_items
    items = split_items(path.read_text(encoding="utf-8"))
    return [it["name"] for it in items]


def full_use_tree() -> list[str]:
    lines = BAK.read_text(encoding="utf-8").split("\n")
    out = []
    i = 0
    while i < len(lines):
        if lines[i].startswith("use "):
            j = i
            while j < len(lines) and not lines[j].rstrip().endswith(";"):
                j += 1
            out.extend(lines[i:j + 1])
            i = j + 1
        else:
            i += 1
    blk = "\n".join(out)
    assert blk.count("{") == blk.count("}"), f"use tree braces unbalanced: {blk.count('{')}/{blk.count('}')}"
    return out


def restore_tree(path: Path, tree: list[str]) -> None:
    """把完整 use 树塞进非 mod.rs 文件（先删旧顶层 use），并校验括号平衡。"""
    text = path.read_text(encoding="utf-8")
    before_open, before_close = text.count("{"), text.count("}")
    ls = text.split("\n")
    out, i = [], 0
    while i < len(ls):
        if ls[i].startswith("use "):
            j = i
            while j < len(ls) and not ls[j].rstrip().endswith(";"):
                j += 1
            if j >= len(ls):
                raise RuntimeError(f"{path.name}: unterminated use at line {i}")
            # 校验被删块自身括号平衡（use 组内只允许花括号成对）
            blk = "\n".join(ls[i:j + 1])
            if blk.count("{") != blk.count("}"):
                raise RuntimeError(f"{path.name}: unbalanced use block at line {i}: {blk[:80]!r}")
            i = j + 1
            continue
        out.append(ls[i])
        i += 1
    ins = 0
    while ins < len(out) and (out[ins].startswith("//!") or not out[ins].strip()):
        ins += 1
    out[ins:ins] = ["", "// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。"] + tree
    new = "\n".join(out)
    after_open, after_close = new.count("{"), new.count("}")
    # 比较开-闭差值而非原始计数（字符串/注释里的花括号会让原始计数有偏）。
    if (before_open - before_close) != (after_open - after_close):
        raise RuntimeError(f"{path.name}: brace delta changed {before_open - before_close} -> {after_open - after_close}")
    path.write_text(new, encoding="utf-8", newline="\n")


def split_step(mapping: dict) -> None:
    mp = ROOT / "tools/_rebuild_mapping.json"
    mp.write_text(mapping, encoding="utf-8")
    if run("python", "tools/split_shell.py", str(mp)) != 0:
        raise RuntimeError("split failed")


def add_decls(names: list[str]) -> None:
    p = SHELL / "mod.rs"
    t = p.read_text(encoding="utf-8")
    block = "".join(f"mod {n};\n" for n in names)
    anchor = "// ---- 模块划分（性能重构设计-2026-10-30 §7/§10）----\n"
    assert anchor in t
    t = t.replace(anchor, anchor + block, 1)
    p.write_text(t, encoding="utf-8", newline="\n")


def main() -> int:
    # 0) 清场：bak.rs 作为 shell.rs 起点
    if SHELL.exists():
        shutil.rmtree(SHELL)
    shutil.copy(BAK, SRC / "shell.rs")

    # 1) 步 1：mask/common/adjust
    names = item_names(SRC / "shell.rs")
    step1_map = {
        "mask": ["MASK_SLIDERS", "MaskGeo", "MaskGeoCache", "HistMeshCache", "DeflCurveCache",
                 "FpHasher", "impl@FpHasher", "impl@FpHasher#2", "mask_geo_fingerprint",
                 "mask_geo_hash_comp", "MASK_BLENDS", "MASK_BLEND_LABELS", "LUM_DEFAULT",
                 "HUE_DEFAULT", "MaskView", "mask_comp_object", "mask_comp_object_mut",
                 "mask_comp_count", "mask_geo_of", "mask_geos", "mask_range_of", "mask_hue_of",
                 "blend_index", "mask_number", "mask_flag", "mask_view", "is_radial", "is_brush",
                 "mask_write_geo", "mask_write", "adjust_masks", "CompAction", "mask_pixel_size",
                 "mask_linear_ends", "mask_radial_axes", "mask_radial_points", "mask_screen",
                 "mask_norm", "mask_pick_color", "mask_handles", "mask_hit", "mask_hit_any",
                 "mask_drag_handle", "handle_is_center", "mask_comp_is_brush",
                 "brush_screen_radius", "brush_note", "hue_color", "brush_begin", "brush_extend",
                 "brush_clear", "draw_mask_overlay", "mask_stroke_lines", "mask_geometry_tests",
                 "mask_panel_tests"],
        "common": ["preset_panel", "side_head", "side_row", "side_note", "group_gap", "opt_sep",
                   "group_help", "group_header", "in_field", "slider", "spec_slider", "slider_row",
                   "kv", "note", "bar_button_on", "slider_reset_tests"],
        "adjust": ["adjust_tab", "QA_KINDS", "qa_color", "adjust_sequence", "adjust_deflicker",
                   "copies_group", "hdr_group", "pano_group", "merge_progress_row",
                   "adjust_develop", "adjust_look", "adjust_levels", "adjust_parametric",
                   "adjust_curve", "adjust_hsl", "adjust_mono", "adjust_grade", "adjust_detail",
                   "adjust_grain", "adjust_profiles", "adjust_lens", "adjust_frame",
                   "adjust_render", "adjust_audio", "queue_table", "queue_counts", "flat_speed",
                   "reset_group", "qa_panel_tests", "gradient_rail_tests"],
    }
    import json
    split_step(json.dumps({
        "target": str(SHELL),
        "source": str(SRC / "shell.rs"),
        "modules": {
            "mask": {"doc": "蒙版域：几何解析、画布 overlay 与手柄、画笔、面板与缓存（性能重构设计 §7）。", "items": step1_map["mask"]},
            "common": {"doc": "面板公共件：预设栏、侧栏行、组头、滑杆、键值行（被 adjust/mask/dock 引用）。", "items": step1_map["common"]},
            "adjust": {"doc": "调整组全家：显影/颜色/细节/成片队列等右侧 dock 的调整面板。", "items": step1_map["adjust"]},
        },
    }, ensure_ascii=False))

    # shell.rs → shell/mod.rs + 步1 decls + 对外重导出（插在首个非 use 顶层项之前）
    t = (SRC / "shell.rs").read_text(encoding="utf-8")
    ls = t.split("\n")
    item_decl = re.compile(r"^(?:(?:pub(?:\((?:crate|super|in [a-z_:]+)\))?\s+)?"
                           r"(?:fn|struct|enum|const|static|mod|impl|trait)\b)")
    first = next(i for i, l in enumerate(ls) if l and l[0] not in " \t" and item_decl.match(l))
    block = (
        "\n// ---- 模块划分（性能重构设计-2026-10-30 §7/§10）----\n"
        "mod adjust;\nmod common;\nmod mask;\n\n"
        "// 对外（crate 内其他模块）仍走 crate::shell::X 路径，重新导出保持外部不变：\n"
        "pub(crate) use adjust::reset_group;\n"
        "pub(crate) use mask::{\n"
        "    DeflCurveCache, HistMeshCache, MaskGeo, MaskGeoCache, mask_comp_object,\n"
        "    mask_geo_fingerprint, mask_geos, mask_handles, mask_radial_points, mask_stroke_lines,\n"
        "};\n\n"
    ).split("\n")
    ls[first:first] = block
    (SHELL / "mod.rs").write_text("\n".join(ls), encoding="utf-8", newline="\n")
    (SRC / "shell.rs").unlink()
    print("step 1 done (mask/common/adjust + mod.rs)")

    # 2/3) 步 2 + 步 3：视图区七件 + 收尾三件（一次跑完，逐段由编译器校验在后面统一做）
    remain = set(item_names(SHELL / "mod.rs"))
    for step_mods in (STEP2, STEP3):
        spec = {}
        for m in step_mods:
            want = [n for n in remain if n == m]  # 占位，实际映射在下方硬表
        # 硬表（§7 映射）
        hard = {
            "chrome": ["RESIZE_ZONE", "WINBTN_W", "WINBTN_ZONE_W", "WINBTN_INSET", "WINBTN_R",
                       "WINBTN_STROKE", "WIN_CLOSE_HOT", "window_resize", "window_buttons",
                       "mac_lights", "win_buttons", "chrome_tests"],
            "menu": ["menu_row", "menu_style", "menu_item", "file_menu", "edit_menu",
                     "develop_menu", "view_menu", "help_menu", "handle_input", "digit_key",
                     "fit_view", "actual_size", "chrome_labels_tests"],
            "dialogs": ["open_dialog", "open_photos_dialog", "rescan", "import_xmp_dialog",
                        "export_xmp_dialog", "import_look_dialog", "export_look_dialog",
                        "choose_audio", "codec_label"],
            "toolbar": ["options_bar", "doc_tab", "tool_rail", "bar_button", "bar_primary",
                        "bar_chip", "bar_value", "status_bar", "ctx_seg"],
            "canvas": ["canvas_panel", "crop_overlay", "motion_overlay", "resize_cursor",
                       "crop_handles", "chip_label", "draw_rulers", "empty_state"],
            "timeline": ["timeline", "curves", "label_color", "filmstrip", "transport"],
            "dock": ["dock_panel", "histogram", "smooth_twice", "dock_paint_tests"],
            "library": ["library_tab", "roots_view", "lib_filter_bar", "lib_meta_panel",
                        "catalog_view", "lib_row_menu", "lib_stack_act", "collection_view",
                        "lib_apply", "lib_apply_frames", "lib_apply_targets", "split_words",
                        "lib_apply_keywords", "lib_apply_keywords_frames", "StackBadge",
                        "library_row", "group_title"],
            "tabs": ["info_tab", "demosaic_cn", "meta_editor", "meta_field", "sparkline",
                     "keys_tab", "ai_tab", "run_history_command", "history_tab"],
            "windows": ["menu_section", "builtin_action", "plugin_menu", "market_window",
                        "look_section", "look_row", "look_window", "about_window"],
        }
        mods = {}
        for m in step_mods:
            missing = [n for n in hard[m] if n not in remain]
            if missing:
                raise RuntimeError(f"step items missing for {m}: {missing}")
            mods[m] = {"doc": MOD_DOCS[m], "items": hard[m]}
        split_step(json.dumps({"target": str(SHELL), "source": str(SHELL / "mod.rs"),
                               "modules": mods}, ensure_ascii=False))
        add_decls(sorted(step_mods))
        print(f"step done: {sorted(step_mods)}")

    # 4) 完整 use 树回填（非 mod.rs）
    tree = full_use_tree()
    for f in sorted(SHELL.glob("*.rs")):
        if f.name != "mod.rs":
            restore_tree(f, tree)
    print("use tree restored to", len(list(SHELL.glob('*.rs'))) - 1, "files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
