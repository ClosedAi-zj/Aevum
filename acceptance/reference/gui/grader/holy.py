# -*- coding: utf-8 -*-
"""曝光台阶检测任务：先读 EXIF，覆盖不足时退回逐帧亮度推断。

EXIF 是一帧一次的小文件读取，很快；亮度推断要解码整段序列，只有在相机
没写全曝光三要素时才启动。已经测过的亮度（胶片曲线）由调用方传入，避免
同一段序列扫两遍。
"""
import math
import os

from reference import holy as core, imio, luma
from reference.gui import job as jobs


class DetectJob(jobs.Job):
    def __init__(self, items, means=None, ramp=core.RAMP):
        super().__init__([])
        self.items = list(items)
        self.means = list(means) if means is not None else None
        self.ramp = int(ramp)

    def run(self):
        items = self.items
        n = len(items)
        if n < 2:
            self.done.emit(False, "序列太短")
            return
        try:
            infos, ev = [], []
            for i, path in enumerate(items):
                if self.stopping:
                    self.done.emit(False, "cancelled")
                    return
                info = imio.exif_exposure(path)
                infos.append(info)
                ev.append(info.get("ev"))
                if i % 8 == 0 or i == n - 1:
                    self.progress.emit(0.45 * (i + 1) / n,
                                       f"EXIF {i + 1}/{n}")
            valid = sum(v is not None for v in ev)
            if valid >= max(2, math.ceil(n * 0.5)):
                steps = core.find_steps(ev)
                source = "exif"
            else:
                means = self.means
                if (means is None or len(means) != n
                        or sum(v is not None for v in means)
                        < max(2, n // 2)):
                    means, issues = luma.measure_luma(
                        items, 512,
                        on_progress=lambda d, _t: self.progress.emit(
                            0.45 + 0.55 * d / max(1, n), f"测光 {d}/{n}"),
                        cancel=lambda: self.stopping)
                    if means is None or self.stopping:
                        self.done.emit(False, "cancelled")
                        return
                    if issues:
                        self.log.emit(
                            f"跳过 {len(issues)} 帧无法测光的图片")
                steps = core.find_luma_steps(means)
                source = "luma"
            labeled = core.steps_with_labels(steps, infos)
            offsets = core.ramp_offsets(steps, n, self.ramp)
            self.progress.emit(1.0, f"{len(labeled)} 处台阶")
            self.done.emit(True, {
                "files": [os.path.basename(p) for p in items],
                "steps": labeled,
                "source": source,
                "ramp": self.ramp,
                "offsets": offsets.tolist(),
            })
        except Exception as e:
            self.log.emit(f"台阶检测失败：{e}")
            self.done.emit(False, str(e))
