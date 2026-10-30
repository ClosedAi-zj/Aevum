# -*- coding: utf-8 -*-
"""延时照片序列 -> 视频（命令行入口）。

  python apps/timelapse.py -i aligned_lr4 --fps 120                 # 16:9 + 无裁切两版
  python apps/timelapse.py -i aligned_lr4 --fps 24 --aspect none -o clip.mp4
  python apps/timelapse.py -i "F:/照片/lr/lr4" --long-side 4096 --aspect 1.90:1

序列识别与命令拼装都在 lapse/video.py；带界面请用 apps/timelapse_gui.py。
"""
import argparse
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lapse import imio, video  # noqa: E402
from lapse.ff import find_ffmpeg  # noqa: E402


def main():
    ap = argparse.ArgumentParser(
        description="延时照片序列 -> 视频（ffmpeg）",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    ap.add_argument("-i", "--input", required=True, help="图片目录")
    ap.add_argument("-o", "--output", help="输出 mp4；不给则按参数自动命名")
    ap.add_argument("--fps", type=float, default=120, help="输出帧率")
    ap.add_argument("--aspect", default="both", choices=video.ASPECT_CHOICES,
                    help="裁切比例；1.90:1 = DCI 4K 宽银幕（配 --long-side 4096 "
                         "得 4096x2160）；both = 同时导出 16:9 与无裁切版")
    ap.add_argument("--long-side", type=int, default=3840,
                    help="长边像素（UHD 4K=3840，DCI 4K=4096）")
    ap.add_argument("--crf", type=int, default=18, help="质量，越小越好")
    ap.add_argument("--preset", default="medium", help="x265 速度档")
    ap.add_argument("--codec", default="hevc", choices=["hevc", "h264"])
    ap.add_argument("--pattern", help="输入文件名模式，默认自动识别"
                                      "（如 aligned_%%04d.jpg）")
    ap.add_argument("--start", type=int, help="起始编号，默认自动识别")
    ap.add_argument("--threads", type=int, help="编码线程数")
    ap.add_argument("--allow-gaps", action="store_true",
                    help="编号有缺口时继续，缺口处按上一帧停留")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    ff = find_ffmpeg()
    if not ff:
        sys.exit("找不到 ffmpeg，请先 pip install imageio-ffmpeg")
    src = args.input
    if not os.path.isdir(src):
        sys.exit(f"目录不存在: {src}")

    try:
        s = video.scan_series(src, allow_gaps=args.allow_gaps)
    except video.SeriesGapError as e:
        sys.exit(str(e) + "\n加 --allow-gaps 可按上一帧停留继续。")
    except video.SeriesError as e:
        sys.exit(str(e))
    if s["gaps"]:
        print(f"缺口 {len(s['gaps'])} 处（{s['gaps'][:10]}），"
              f"按上一帧停留补齐")
    start = s["first"] if args.start is None else args.start
    src_dir, pattern, start, count, cleanup = video.fill_gaps(src, s)
    if args.pattern:
        pattern = args.pattern
    print(f"序列: {os.path.join(src_dir, pattern)}"
          f"  {s['first']}..{s['last']}  共 {count} 帧"
          f"  ->  {count / args.fps:.2f}s @ {args.fps:g}fps")

    src_w = src_h = 0
    if args.aspect in ("none", "both"):
        src_w, src_h = imio.image_size(os.path.join(src, s["sample"]))
        print(f"原图尺寸: {src_w}x{src_h}")

    stem = args.output or video.default_out(src, s["prefix"], args.fps,
                                           args.long_side)
    outs = video.outputs_for(args.aspect, stem)

    try:
        for a, out in outs.items():
            cmd, w, h = video.build_cmd(
                ff, src_dir, out, args.fps, None if a == "none" else a,
                args.long_side, args.crf, args.preset, args.codec, pattern,
                start, count, src_w, src_h, args.threads, args.quiet)
            print("\n$ " + " ".join(f'"{c}"' if " " in c else c for c in cmd))
            sys.stdout.flush()
            if subprocess.run(cmd).returncode != 0:
                sys.exit(f"ffmpeg 失败: {out}")
            print(f"完成 {out}  ({w}x{h})  {os.path.getsize(out)/1e6:.1f} MB")
    finally:
        if cleanup:
            cleanup()


if __name__ == "__main__":
    main()
