# -*- coding: utf-8 -*-
"""延时照片对齐（命令行入口）。

  python apps/align.py -i "F:/照片/lr/lr4" -o aligned_lr4
  python apps/align.py -i raw目录 -o 预览 --sample 12 --verify     # 先抽帧预检
  python apps/align.py -i 调色后目录 -o 输出 --load-shifts shifts.json

算法都在 lapse/align.py，本文件只管参数、打印与两步流程。
"""
import argparse
import json
import os
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lapse import align, imio  # noqa: E402


def parse_args():
    ap = argparse.ArgumentParser(
        description="延时照片对齐：位移估计 + 抖动保留平滑 + 统一裁切",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    ap.add_argument("-i", "--input", required=True, help="图片目录")
    ap.add_argument("-o", "--output", required=True, help="输出目录")
    ap.add_argument("-r", "--reference", type=int, default=0, help="参考帧索引")
    ap.add_argument("-e", "--ext", default="jpg,jpeg,png,tif,tiff",
                    help="扩展名列表；加 --raw 时忽略")
    ap.add_argument("--raw", action="store_true", help="输入是 RAW（.RW2/.CR3…）")
    ap.add_argument("--est", default="template", choices=align.ESTIMATORS,
                    help="位移估计器：template=底部模板匹配（对垂直漂移最稳），"
                         "phase=相位相关，ecc=ECC（可带旋转/透视）")
    ap.add_argument("--motion", default="euclid", choices=tuple(align.MOTIONS),
                    help="仅 --est ecc：trans=平移，euclid=平移+旋转，"
                         "affine=加缩放剪切，homography=单应")
    ap.add_argument("--template-start", type=float, default=0.7,
                    help="模板起始高度（0~1）")
    ap.add_argument("--search-margin", type=int, default=150,
                    help="模板搜索向四周扩展的像素")
    ap.add_argument("--roi", default="0,0.6,1,1",
                    help="--est phase 的区域 x1,y1,x2,y2（0~1，默认底部 40%%）")
    ap.add_argument("--no-normalize", action="store_true",
                    help="--est phase 关闭亮度归一化（序列曝光突变时反而更稳）")
    ap.add_argument("--smooth", type=int, default=3,
                    help="位移序列 median 平滑窗口（奇数；1=不平滑）")
    ap.add_argument("--jump-px", type=float, default=1.0,
                    help="偏离局部中值超过该像素数视为真实抖动，不参与平滑")
    ap.add_argument("--min-score", type=float, default=0.6,
                    help="低于该匹配置信度的离群值按误匹配处理")
    ap.add_argument("--out-ext", default="jpg", help="输出扩展名")
    ap.add_argument("--out-bps", type=int, default=8, choices=[8, 16],
                    help="解码/输出位深（16 位只在 png/tif 里保得住）")
    ap.add_argument("--half", action="store_true",
                    help="RAW 半分辨率解码（快约 4 倍，精度略降）")
    ap.add_argument("--no-crop", action="store_true", help="只对齐不裁切")
    ap.add_argument("--sample", type=int, default=0,
                    help="只抽 N 帧预检（含参考帧），用来先看位移是否合理")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--save-shifts", help="把位移与裁切写成 JSON")
    ap.add_argument("--load-shifts", help="复用 JSON 里的位移，跳过估计")
    ap.add_argument("--verify", action="store_true",
                    help="对齐后抽帧复测残余位移")
    ap.add_argument("-w", "--workers", type=int, default=0)
    return ap.parse_args()


def _tick(n, t0):
    """进度回调：整条序列最多打印 20 行。"""
    step = max(1, n // 20)

    def cb(done, _total):
        if done % step == 0 or done == n:
            print(f"  {done}/{n}  {time.time()-t0:.1f}s")
    return cb


def main():
    args = parse_args()
    t0 = time.time()
    exts = tuple("." + e.strip().lower().lstrip(".")
                 for e in args.ext.split(",") if e.strip())
    files = imio.scan_files(args.input, exts, raw=args.raw)
    if not files:
        sys.exit(f"错误：'{args.input}' 里没有可处理的图片")
    if not (0 <= args.reference < len(files)):
        sys.exit("错误：参考帧索引超范围")

    idx = [args.reference]
    if 0 < args.sample < len(files):
        idx = sorted(set(align.sample_indices(len(files), args.sample,
                                              args.seed)) | {args.reference})
        files = [files[i] for i in idx]
        print(f"预检采样 {len(idx)} 帧：{idx}")
    n = len(files)
    ref_index = idx.index(args.reference) if args.sample else args.reference

    roi = tuple(float(x) for x in args.roi.split(","))
    if len(roi) != 4 or not (0 <= roi[0] < roi[2] <= 1 and 0 <= roi[1] < roi[3] <= 1):
        sys.exit("错误：--roi 应为 x1,y1,x2,y2（0~1）")

    ref_img = imio.decode_image(files[ref_index], 8)
    if ref_img is None:
        sys.exit(f"错误：无法读取参考图 {files[ref_index]}")
    ref_shape = ref_img.shape[:2]
    del ref_img
    print(f"共 {n} 张  参考: {os.path.basename(files[ref_index])}  "
          f"尺寸 {ref_shape[1]}x{ref_shape[0]}")

    mats = None
    if args.load_shifts:
        with open(args.load_shifts, encoding="utf-8") as f:
            meta = json.load(f)
        offsets = {i: tuple(s) for i, s in enumerate(meta["shifts"])}
        if meta.get("mats"):
            mats = {i: np.asarray(m, dtype=np.float32)
                    for i, m in enumerate(meta["mats"])}
        crop = tuple(meta["crop"])
        print(f"[1/2] 复用位移 {args.load_shifts}  裁切 {crop}"
              + ("（含矩阵）" if mats else ""))
    else:
        labels = f"{args.est}/{args.motion}" if args.est == "ecc" else args.est
        print(f"[1/2] 估计每帧位移（{labels}）...")
        mats = {} if args.est == "ecc" else None
        offsets, scores, issues, dt = align.measure_offsets(
            files, ref_index, est=args.est, workers=args.workers,
            template_start=args.template_start, search_margin=args.search_margin,
            motion=args.motion, roi=roi, normalize=not args.no_normalize,
            half=args.half, mats=mats, on_progress=_tick(n, t0))
        print(f"  用时 {dt:.1f}s  异常 {len(issues)} 帧")
        for i, msg in issues:
            print(f"    [{i:4d}] {msg}")

        xs = [offsets[i][0] for i in range(n)]
        ys = [offsets[i][1] for i in range(n)]
        print(f"  dx 范围 [{min(xs):+.2f}, {max(xs):+.2f}]  "
              f"dy 范围 [{min(ys):+.2f}, {max(ys):+.2f}]")
        if mats is not None:
            rots = [align.rotation_deg(mats[i]) for i in range(n)]
            print(f"  旋转范围 [{min(rots):+.2f}°, {max(rots):+.2f}°]")
        low = [(i, round(scores[i], 3)) for i in range(n)
               if scores[i] < args.min_score]
        if low:
            print(f"  ⚠ 置信度偏低（可能误匹配）：{low[:10]}")
        if n <= 60:
            for i in range(n):
                print(f"    [{i:4d}] {os.path.basename(files[i]):>22s}  "
                      f"dx={offsets[i][0]:+8.2f}  dy={offsets[i][1]:+8.2f}  "
                      f"score={scores[i]:.3f}"
                      + ("  ← 参考" if i == ref_index else ""))

        if args.smooth > 1:
            offsets, kx, ky = align.smooth_offsets(
                offsets, scores, args.smooth, args.jump_px, args.min_score)
            if mats is not None:
                mats = align.retranslate(mats, offsets)
            print(f"  median 平滑（窗口 {args.smooth}）：判定为真实抖动并保留 "
                  f"dx {kx} 帧 / dy {ky} 帧")

        x1, y1, x2, y2 = align.common_crop(
            mats if mats is not None else offsets, ref_shape[1], ref_shape[0])
        crop = (0, 0, ref_shape[1], ref_shape[0]) if args.no_crop \
            else (x1, y1, x2, y2)
        cw, ch = x2 - x1, y2 - y1
        loss = 100.0 * (1 - cw * ch / float(ref_shape[1] * ref_shape[0]))
        print(f"  统一裁切 ({x1},{y1})-({x2},{y2})  输出 {cw}x{ch}  "
              f"面积损失 {loss:.2f}%")
        if cw <= 100 or ch <= 100:
            sys.exit("错误：有效区域太小")

        if args.save_shifts:
            meta = {"input": os.path.abspath(args.input),
                    "est": args.est, "motion": args.motion,
                    "reference": os.path.basename(files[ref_index]),
                    "files": [os.path.basename(f) for f in files],
                    "shifts": [list(offsets[i]) for i in range(n)],
                    "crop": list(crop),
                    "source_shape": list(ref_shape)}
            if mats is not None:
                meta["mats"] = [np.asarray(mats[i]).tolist() for i in range(n)]
            with open(args.save_shifts, "w", encoding="utf-8") as f:
                json.dump(meta, f, ensure_ascii=False, indent=1)
            print(f"  位移已写入 {args.save_shifts}")

    print("[2/2] 应用位移并裁切...")
    out, failed = align.apply_offsets(
        files, args.output, offsets, crop, ref_shape, out_ext=args.out_ext,
        bps=args.out_bps, half=args.half, workers=args.workers, mats=mats,
        on_progress=_tick(n, t0))
    if failed:
        print(f"  ⚠ 写入失败帧：{failed[:20]}")

    if args.verify:
        check = list(range(len(out)))
        if len(check) > 24:
            check = sorted(set(np.linspace(0, len(out) - 1, 12).astype(int))
                           | {ref_index})
        v = align.verify_residuals([out[i] for i in check], roi=roi,
                                   ref_index=check.index(ref_index),
                                   normalize=not args.no_normalize)
        mx = 0.0
        print("\n[校验] 残余位移（理想 < 1px）：")
        for j, i in enumerate(check):
            _, s, resp = v[j]
            if s is None:
                print(f"    [{i:4d}] 校验失败")
                continue
            mx = max(mx, float(np.hypot(*s)))
            print(f"    [{i:4d}] dx={s[0]:+6.2f} dy={s[1]:+6.2f} "
                  f"|s|={np.hypot(*s):.2f}px  resp={resp:.3f}")
        print(f"  最大残余 {mx:.2f}px" + ("  ✅" if mx < 1.0 else "  ⚠ 超过 1px"))

    print(f"\n完成，总用时 {time.time()-t0:.1f}s  输出: {args.output}")
    print('合成视频： python apps/timelapse.py -i "%s" --fps 24' % args.output)


if __name__ == "__main__":
    main()
