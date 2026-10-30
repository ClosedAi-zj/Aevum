# -*- coding: utf-8 -*-
"""延时照片序列合成视频（图形界面）。

选目录 -> 自动识别序列编号/张数/尺寸 -> 选帧率、比例、分辨率、质量 -> 合成。
序列识别与 ffmpeg 命令拼装全部复用 video.py，界面只负责参数和进度。
编号有缺口会直接标红拒绝合成，避免把跳帧当成运动。
启动：apps/timelapse_gui.py（双击 延时视频.bat）。
"""
import os
import queue
import re
import subprocess
import threading
import tkinter as tk
from tkinter import ttk, filedialog, messagebox

from lapse import imio, video
from lapse.ff import NO_WINDOW, find_ffmpeg

APP_TITLE = "延时视频合成工具"
FPS_CHOICES = ["120", "100", "60", "50", "30", "25", "24"]
SIDE_CHOICES = ["3840 (UHD 4K)", "4096 (DCI 4K)", "2560 (2.5K)",
                "1920 (1080p)", "1280 (720p)"]
ASPECT_CHOICES = [("16:9 + 原始（两版）", "both"),
                  ("16:9  UHD 4K 3840x2160", "16:9"),
                  ("1.90:1  DCI 4K 4096x2160", "1.90:1"),
                  ("3:2", "3:2"), ("4:3", "4:3"),
                  ("原始比例（不裁切）", "none")]
CODEC_CHOICES = [("HEVC / x265（4K 高帧率必需）", "hevc"),
                 ("H.264 / x264（兼容性优先）", "h264")]
PRESET_CHOICES = ["ultrafast", "veryfast", "faster", "fast",
                  "medium", "slow", "slower", "veryslow"]
# CRF 越大越糊，这点反直觉，必须在界面上说出来
CRF_WORDS = ((17, "极高"), (21, "推荐·视觉无损"), (26, "一般"), (31, "偏软"),
             (99, "明显压缩"))


def crf_word(v):
    for lim, w in CRF_WORDS:
        if v <= lim:
            return w
    return "明显压缩"


class TimelapseApp(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title(APP_TITLE)
        self.minsize(760, 620)

        self.ff = find_ffmpeg()
        self.series = None        # dict，见 detect()
        self.proc = None
        self.stopped = False
        self.running = False
        self.q = queue.Queue()
        self.out_manual = False   # 用户手改过输出名就不再自动覆盖
        self._debounce = None

        self._build_ui()
        self._on_crf()          # 初始数值也要显示质量档位文字
        self.after(100, self._poll)
        self.protocol("WM_DELETE_WINDOW", self._on_close)

        self.log(f"ffmpeg: {self.ff or '未找到（请 pip install imageio-ffmpeg）'}")
        if not self.ff:
            messagebox.showerror(APP_TITLE,
                                 "未找到 ffmpeg。\n请安装 ffmpeg，或在运行本工具的 "
                                 "Python 环境里 pip install imageio-ffmpeg。")

    # ---------- UI ----------
    def _build_ui(self):
        top = ttk.Frame(self, padding=(12, 12, 12, 0))
        top.pack(fill="x")
        ttk.Button(top, text="选择序列目录…", command=self.choose_dir).pack(side="left")
        self.ent_dir = ttk.Entry(top)
        self.ent_dir.pack(side="left", fill="x", expand=True, padx=8)
        self.ent_dir.bind("<KeyRelease>", self._on_dir_key)
        ttk.Button(top, text="重新检测", width=8,
                   command=self.detect).pack(side="left")

        self.lbl_info = ttk.Label(self, padding=(20, 6, 20, 0), anchor="w",
                                  foreground="#555", wraplength=720, justify="left")
        self.lbl_info.pack(fill="x")

        box = ttk.LabelFrame(self, text="输出参数", padding=12)
        box.pack(fill="x", padx=12, pady=10)
        box.columnconfigure(1, weight=1)
        box.columnconfigure(4, weight=1)

        def add(row, col, label, widget, extra=None):
            ttk.Label(box, text=label).grid(row=row, column=col * 3,
                                            sticky="w", pady=3)
            widget.grid(row=row, column=col * 3 + 1, sticky="w", padx=(6, 12))
            if extra is not None:
                extra.grid(row=row, column=col * 3 + 2, sticky="w")

        self.ent_fps = ttk.Combobox(box, values=FPS_CHOICES, width=6)
        self.ent_fps.insert(0, "120")
        self.ent_fps.bind("<<ComboboxSelected>>", lambda e: self._refresh())
        self.ent_fps.bind("<KeyRelease>", lambda e: self._refresh())
        add(0, 0, "帧率", self.ent_fps)

        self.cb_aspect = ttk.Combobox(box, state="readonly", width=20,
                                      values=[t for t, _ in ASPECT_CHOICES])
        self.cb_aspect.current(0)
        self.cb_aspect.bind("<<ComboboxSelected>>", lambda e: self._refresh())
        add(0, 1, "画面比例", self.cb_aspect)

        self.ent_side = ttk.Combobox(box, values=SIDE_CHOICES, width=14)
        self.ent_side.insert(0, SIDE_CHOICES[0])
        self.ent_side.bind("<<ComboboxSelected>>", lambda e: self._refresh())
        self.ent_side.bind("<KeyRelease>", lambda e: self._refresh())
        add(1, 0, "长边分辨率", self.ent_side)

        self.cb_codec = ttk.Combobox(box, state="readonly", width=24,
                                     values=[t for t, _ in CODEC_CHOICES])
        self.cb_codec.current(0)
        self.cb_codec.bind("<<ComboboxSelected>>", lambda e: self._refresh())
        add(1, 1, "编码器", self.cb_codec)

        # CRF 数值和滑条放同一个 Frame：分格摆会被读成下一列的标题
        crf_row = ttk.Frame(box)
        self.scl_crf = ttk.Scale(crf_row, from_=8, to=35, orient="horizontal",
                                 length=130)
        self.scl_crf.set(18)
        # 先定值再挂回调：Tk 的 Scale 程序化设值也会触发 command，
        # 那时 ent_out 还没建好。
        self.scl_crf.configure(command=lambda v: self._on_crf())
        self.lbl_crf = ttk.Label(crf_row, text="18", width=13)
        self.scl_crf.pack(side="left")
        self.lbl_crf.pack(side="left", padx=(6, 0))
        ttk.Label(box, text="质量 CRF（左=最清晰）").grid(
            row=2, column=0, sticky="w", pady=3)
        crf_row.grid(row=2, column=1, sticky="w", padx=(6, 12))

        self.cb_preset = ttk.Combobox(box, state="readonly", width=12,
                                      values=PRESET_CHOICES)
        self.cb_preset.set("medium")
        self.cb_preset.bind("<<ComboboxSelected>>", lambda e: self._refresh())
        add(2, 1, "编码速度", self.cb_preset)

        self.sp_threads = ttk.Spinbox(box, from_=0, to=64, width=6)
        self.sp_threads.set(0)
        add(3, 0, "线程（0=自动）", self.sp_threads)
        self.lbl_dur = ttk.Label(box, text="", foreground="#0a6")
        self.lbl_dur.grid(row=3, column=2, columnspan=4, sticky="w")

        outf = ttk.Frame(self, padding=(12, 0))
        outf.pack(fill="x")
        ttk.Label(outf, text="输出文件").pack(side="left")
        self.ent_out = ttk.Entry(outf)
        self.ent_out.pack(side="left", fill="x", expand=True, padx=8)
        self.ent_out.bind("<KeyRelease>", self._on_out_key)
        ttk.Button(outf, text="另存为…", width=8,
                   command=self.choose_out).pack(side="left")
        self.lbl_out = ttk.Label(self, padding=(20, 4, 20, 0), anchor="w",
                                 foreground="#555", wraplength=720, justify="left")
        self.lbl_out.pack(fill="x")

        bar = ttk.Frame(self, padding=(12, 10))
        bar.pack(fill="x")
        self.run_btn = ttk.Button(bar, text="开始合成", command=self.run)
        self.run_btn.pack(side="left")
        self.cancel_btn = ttk.Button(bar, text="取消", command=self.cancel,
                                     state="disabled")
        self.cancel_btn.pack(side="left", padx=6)
        self.open_btn = ttk.Button(bar, text="打开输出", command=self.open_out,
                                   state="disabled")
        self.open_btn.pack(side="left", padx=6)
        self.prog = ttk.Progressbar(bar, maximum=1000)
        self.prog.pack(side="left", fill="x", expand=True, padx=10)
        self.prog_lbl = ttk.Label(bar, text="0%", width=22, anchor="e")
        self.prog_lbl.pack(side="left")

        lf = ttk.LabelFrame(self, text="日志 / ffmpeg 命令", padding=4)
        lf.pack(fill="both", expand=True, padx=12, pady=(0, 12))
        self.log_txt = tk.Text(lf, height=10, font=("consolas", 9), wrap="none",
                               bg="#222", fg="#ddd", insertbackground="#ddd",
                               state="disabled")
        sb = ttk.Scrollbar(lf, command=self.log_txt.yview)
        self.log_txt.configure(yscrollcommand=sb.set)
        self.log_txt.pack(side="left", fill="both", expand=True)
        sb.pack(side="left", fill="y")

    # ---------- 序列检测 ----------
    def choose_dir(self):
        d = filedialog.askdirectory(title="选择延时照片序列目录")
        if d:
            self.ent_dir.delete(0, "end")
            self.ent_dir.insert(0, os.path.normpath(d))
            self.detect()

    def _on_dir_key(self, _e):
        if self._debounce:
            self.after_cancel(self._debounce)
        self._debounce = self.after(500, self.detect)

    def detect(self):
        d = self.ent_dir.get().strip().strip('"')
        self.series = None
        if not d:
            self._set_info("（未选择目录）", False)
            return
        if not os.path.isdir(d):
            self._set_info(f"目录不存在：{d}", False)
            return
        try:
            s = video.scan_series(d)
        except video.SeriesGapError as e:
            if not messagebox.askyesno(
                    "序列有缺口",
                    str(e) + "\n\n允许缺口，按上一帧停留继续？"):
                self._set_info("序列有缺口，已取消", False)
                return
            s = video.scan_series(d, allow_gaps=True)
        except video.SeriesError as e:
            self._set_info(str(e), False)
            return
        w, h = imio.image_size(os.path.join(d, s["sample"]))
        self.series = dict(s, dir=d, w=w, h=h)
        size = f"{w}x{h}" if w else "尺寸未知（未安装 cv2）"
        self._set_info(
            f"序列  {s['prefix']}{s['first']:0{s['width']}d}.."
            f"{s['last']:0{s['width']}d}   共 {s['count']} 张   "
            f"{size}   源文件模式  {s['pattern']}", True)
        self._refresh()

    def _set_info(self, text, ok):
        self.lbl_info.configure(text=text,
                                foreground="#555" if ok else "#c0392b")

    # ---------- 参数 -> 输出 ----------
    def _aspect(self):
        return dict(ASPECT_CHOICES)[self.cb_aspect.get()]

    def _fps(self):
        m = re.search(r"\d+(\.\d+)?", self.ent_fps.get())
        return float(m.group()) if m else 0.0

    def _long_side(self):
        m = re.search(r"\d+", self.ent_side.get())
        return int(m.group()) if m else 0

    def _codec(self):
        return dict(CODEC_CHOICES)[self.cb_codec.get()]

    def _on_crf(self):
        v = int(float(self.scl_crf.get()))
        self.lbl_crf.configure(text=f"{v}  {crf_word(v)}")
        self._refresh()

    def _on_out_key(self, _e):
        if self.ent_out.focus_get() is self.ent_out:
            self.out_manual = True
        self._refresh_out_label()

    def _refresh(self):
        self._refresh_out()
        self._refresh_labels()

    def _refresh_out(self):
        if self.out_manual or not self.series:
            self._refresh_out_label()
            return
        s = self.series
        stem = os.path.splitext(os.path.abspath(os.path.join(
            s["dir"].rstrip("/\\"), "..",
            f"{s['prefix'].strip('_')}_{self._fps():g}fps_"
            f"{self._long_side()}.mp4")))[0]
        self.ent_out.delete(0, "end")
        self.ent_out.insert(0, stem)
        self._refresh_out_label()

    def _jobs(self):
        """返回 [(比例, 输出路径)]；stem 由用户填写或自动生成，不含后缀。"""
        stem = self.ent_out.get().strip().strip('"')
        if not stem:
            return []
        stem = os.path.splitext(os.path.abspath(stem))[0]
        aspects = video.aspects_for(self._aspect())
        if len(aspects) == 1:
            return [(aspects[0], stem + ".mp4")]
        return [(a, stem + video.out_label(a) + ".mp4") for a in aspects]

    def _refresh_labels(self):
        if not self.series:
            self.lbl_dur.configure(text="")
            self._refresh_out_label()
            return
        fps = self._fps()
        if fps > 0:
            sec = self.series["count"] / fps
            if sec >= 60:
                mm, ss = divmod(int(sec), 60)
                when = f"{mm} 分 {ss} 秒"
            else:
                when = f"{sec:.1f} 秒"
            self.lbl_dur.configure(text=f"成片时长  {when}   @ {fps:g}fps")
        else:
            self.lbl_dur.configure(text="帧率必须是正数")
        self._refresh_out_label()

    def _dims(self, aspect):
        s = self.series or {}
        w, h = video.target_size(aspect, self._long_side(),
                                     s.get("w", 0), s.get("h", 0))
        return f"{w}x{h}" if h else f"{w}x?"

    def _refresh_out_label(self):
        jobs = self._jobs()
        if not jobs or not self.series:
            self.lbl_out.configure(text="")
            return
        self.lbl_out.configure(
            text="将写出：\n" +
                 "\n".join(f"{p}   ({self._dims(a)})" for a, p in jobs))

    def choose_out(self):
        d = filedialog.asksaveasfilename(
            title="输出文件（两版会自动加后缀）", defaultextension=".mp4",
            filetypes=[("MP4 视频", "*.mp4")])
        if d:
            self.out_manual = True
            self.ent_out.delete(0, "end")
            self.ent_out.insert(0, os.path.normpath(d))
            self._refresh_out_label()

    # ---------- 合成 ----------
    def run(self):
        if not self.ff:
            messagebox.showerror(APP_TITLE, "未找到 ffmpeg，无法合成。")
            return
        if not self.series:
            messagebox.showwarning(APP_TITLE, "请先选择有效的图片序列目录。")
            return
        if self._fps() <= 0 or self._long_side() < 64:
            messagebox.showwarning(APP_TITLE, "帧率和长边分辨率必须是正数。")
            return
        jobs = self._jobs()
        if not jobs:
            messagebox.showwarning(APP_TITLE, "请填写输出文件路径。")
            return
        for _a, p in jobs:
            od = os.path.dirname(p)
            if od:
                os.makedirs(od, exist_ok=True)

        # tkinter 有线程亲和性：控件只能在主线程读，工作线程里 get() 会直接崩。
        # 所以这里把参数一次性快照成普通数据传给线程。
        opts = {"fps": self._fps(), "long_side": self._long_side(),
                "crf": int(float(self.scl_crf.get())),
                "preset": self.cb_preset.get(), "codec": self._codec(),
                "threads": self._threads()}
        series = dict(self.series)
        self.log(f"参数: {opts['codec']}  crf={opts['crf']}"
                 f"（{crf_word(opts['crf'])}）  preset={opts['preset']}  "
                 f"长边={opts['long_side']}  fps={opts['fps']:g}  "
                 f"帧数={series['count']}")
        if opts["crf"] >= 28:
            self.log("提醒：CRF 越大画质越差，4K 成片一般用 17~20。")

        self.running = True
        self.stopped = False
        self.run_btn.configure(state="disabled")
        self.open_btn.configure(state="disabled")
        self.cancel_btn.configure(state="normal")
        self.prog.configure(value=0)
        threading.Thread(target=self._encode,
                         args=(jobs, opts, series), daemon=True).start()

    def _threads(self):
        m = re.search(r"\d+", self.sp_threads.get())
        return int(m.group()) if m else 0

    def _encode(self, jobs, o, s):
        src_dir, pattern, start, total, cleanup = video.fill_gaps(s["dir"], s)
        if cleanup:
            total = s["last"] - s["first"] + 1
            self.q.put(("log", f"缺口 {len(s.get('gaps') or ())} 处，"
                               f"已按上一帧补齐为 {total} 帧"))
        n = len(jobs)
        results = []
        try:
            for k, (a, out) in enumerate(jobs):
                if self.stopped:
                    break
                self.q.put(("job", (k, n, out)))
                cmd, w, h = video.build_cmd(
                    self.ff, src_dir, out, o["fps"],
                    None if a == "none" else a, o["long_side"], o["crf"],
                    o["preset"], o["codec"], pattern,
                    start, total, s["w"], s["h"], o["threads"], False,
                    progress=True)
                self.q.put(("cmd", " ".join(f'"{c}"' if " " in c else c
                                            for c in cmd)))
                try:
                    proc = subprocess.Popen(
                        cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                        text=True, encoding="utf-8", errors="replace",
                        creationflags=NO_WINDOW)
                except OSError as e:
                    self.q.put(("fail", f"无法启动 ffmpeg：{e}"))
                    return
                self.proc = proc
                misc = []
                for line in proc.stdout:
                    line = line.strip()
                    if line.startswith("frame="):
                        mf = re.search(r"\d+", line)
                        if mf:
                            self.q.put(("prog", (k, int(mf.group()), total, n)))
                    elif line.startswith("progress=") or not line:
                        continue
                    else:
                        misc.append(line)
                rc = proc.wait()
                self.proc = None
                if self.stopped:
                    self.q.put(("cancelled", out))
                    self._drop_partials(jobs[k:])
                    return
                if rc != 0 or not os.path.exists(out):
                    for msg in misc:
                        self.q.put(("log", msg))
                    self._drop_partials(jobs[k:])
                    self.q.put(("fail", f"ffmpeg 退出码 {rc}：{out}"))
                    return
                mb = os.path.getsize(out) / 1e6
                results.append(out)
                self.q.put(("jobdone", (k, n, out, w, h, mb)))
        finally:
            if cleanup:
                cleanup()
        self.q.put(("done", results))

    def _drop_partials(self, jobs):
        """取消后留下的是不可播放的半成品，删掉免得误当成交付物。"""
        for _a, p in jobs:
            try:
                if os.path.exists(p):
                    os.remove(p)
                    self.q.put(("log", f"已删除未完成的 {p}"))
            except OSError as e:
                self.q.put(("log", f"删除失败 {p}: {e}"))

    def cancel(self):
        if self.proc and self.proc.poll() is None:
            self.stopped = True
            self.proc.terminate()
            self.log("正在取消…（等当前帧编码完）")

    # ---------- 事件回传 ----------
    def _poll(self):
        try:
            while True:
                kind, data = self.q.get_nowait()
                if kind == "log":
                    self.log(data)
                elif kind == "cmd":
                    self.log("$ " + data)
                elif kind == "job":
                    k, n, out = data
                    self.log(f"\n[{k + 1}/{n}] {out}")
                elif kind == "prog":
                    k, f, total, n = data
                    frac = (k + min(f / total, 1.0)) / n if total else 0
                    self.prog.configure(value=frac * 1000)
                    self.prog_lbl.configure(
                        text=f"{frac * 100:4.1f}%   帧 {f}/{total}")
                elif kind == "jobdone":
                    k, n, out, w, h, mb = data
                    self.log(f"完成 {out}  ({w}x{h})  {mb:.1f} MB")
                elif kind == "cancelled":
                    self.prog_lbl.configure(text="已取消")
                    self.log(f"已取消，未完成的文件已清除：{data}")
                    self._finish()
                elif kind == "fail":
                    self.prog.configure(value=0)
                    self.prog_lbl.configure(text="失败")
                    self.log("失败：" + data)
                    self._finish()
                elif kind == "done":
                    outs = data
                    self.prog.configure(value=1000)
                    self.prog_lbl.configure(text=f"完成 {len(outs)} 段")
                    if outs:
                        self.open_btn.configure(state="normal")
                        self._last_out = outs[0]
                        self.log(f"\n全部完成：{len(outs)} 个文件")
                    self._finish()
        except queue.Empty:
            pass
        self.after(100, self._poll)

    def _finish(self):
        self.running = False
        self.run_btn.configure(state="normal")
        self.cancel_btn.configure(state="disabled")

    def open_out(self):
        p = getattr(self, "_last_out", None)
        if p and os.path.exists(p):
            subprocess.Popen(["explorer", "/select,", os.path.normpath(p)],
                             creationflags=NO_WINDOW)

    def _on_close(self):
        if self.proc and self.proc.poll() is None:
            if not messagebox.askyesno(APP_TITLE, "正在合成，确定要退出吗？"):
                return
            self.stopped = True
            self.proc.terminate()
        self.destroy()

    def log(self, text):
        self.log_txt.configure(state="normal")
        self.log_txt.insert("end", text + "\n")
        self.log_txt.see("end")
        self.log_txt.configure(state="disabled")


if __name__ == "__main__":
    TimelapseApp().mainloop()
