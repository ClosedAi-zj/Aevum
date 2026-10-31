# -*- coding: utf-8 -*-
"""把音频合进视频：长度算式与 ffmpeg 命令（界面在 acceptance/reference/gui/audiomix.py）。

画面一律 `-c:v copy`。配乐是交付前的最后一步，视频多半已经调色、裁剪过，
再压一次只会掉细节；而混流本来几秒就能干完，不该重编码几十分钟。
只有音频会解码重编——音量、淡入淡出、混音都得重编才做得了。
"""
import os

AUDIO_EXTS = (".mp3", ".wav", ".aac", ".m4a", ".flac", ".ogg", ".opus",
              ".wma", ".aif", ".aiff", ".m4b")
VIDEO_OUT_EXTS = (".mp4", ".m4v", ".mov", ".mkv")
# mp4/mov 里能原样放的音频编码；其余编码得转 AAC，否则播放器不认
COPYABLE = {"aac", "mp3", "alac"}

MODES = (
    ("replace", "替换原声", "丢掉视频里原来的声音，只留这段配乐"),
    ("mix", "与原声混合", "视频原声和配乐叠在一起，两边分别调音量"),
)

ACODECS = (
    ("自动（兼容优先）", "auto"),
    ("不转码（直接复制）", "copy"),
    ("AAC 128k", "aac128"),
    ("AAC 192k", "aac192"),
    ("AAC 256k", "aac256"),
    ("AAC 320k", "aac320"),
)

BITS = {"aac128": "128k", "aac192": "192k", "aac256": "256k", "aac320": "320k"}

DEFAULTS = {"mode": "replace", "delay": 0.0, "gain": 100, "orig_gain": 100,
            "fade_in": 0.0, "fade_out": 0.0, "loop": False, "trim": True,
            "acodec": "auto"}


def num(opt, key):
    """界面读出来的值可能是字符串，长度算式里统一按浮点算。"""
    return float(opt.get(key) or 0)


def needs_filter(opt):
    """要不要把音频解码走一遍滤镜：混音、改音量、淡入淡出、延后进入都得重编。

    延迟尤其要走 `adelay`（补真静音采样）而不是 `-itsoffset`：后者只在容器里
    记一个起始偏移，不少播放器与非线性剪辑软件根本不读它，配乐会从 0 秒开始抢。
    """
    return (opt.get("mode") == "mix" or num(opt, "delay") > 0
            or abs(num(opt, "gain") - 100.0) > 0.5
            or abs(num(opt, "orig_gain") - 100.0) > 0.5
            or num(opt, "fade_in") > 0 or num(opt, "fade_out") > 0)


def copy_ok(out_ext, codec, opt):
    """这条音频能不能一个比特都不改地塞进目标容器。"""
    if needs_filter(opt):
        return False
    if out_ext == ".mkv":
        return True                       # Matroska 基本什么都收
    return (codec or "").lower() in COPYABLE


def music_end(vdur, adur, opt):
    """配乐在成片里收尾的时刻（输出时间轴，秒）。"""
    if opt.get("loop"):
        return float(vdur)                # 循环就是铺满整支片
    end = num(opt, "delay") + float(adur)
    return min(end, float(vdur)) if opt.get("trim") else end


def total_dur(vdur, adur, opt):
    """成片时长：不截断时配乐可能比画面长，尾部只剩声音。"""
    return max(float(vdur), music_end(vdur, adur, opt))


def limit_dur(vdur, adur, opt):
    """该给 `-t` 就返回时长，否则 0。循环必须限长，否则配乐无限铺下去。"""
    if opt.get("loop") or opt.get("trim"):
        return float(vdur)
    return 0.0


def build_cmd(ff, video, audio, out, vinfo, ainfo, opt):
    """返回 (cmd, 成片时长)。vinfo/ainfo 来自 ff.probe_video / ff.probe_audio。

    只要 needs_filter 说了算就重编音频，其余情况一个比特都不动地复制。
    """
    vdur = float(vinfo.get("dur") or 0.0)
    adur = float(ainfo.get("dur") or 0.0)
    ext = os.path.splitext(out)[1].lower()
    filt = needs_filter(opt)
    delay = num(opt, "delay")

    cmd = [ff, "-y", "-hide_banner", "-loglevel", "error", "-nostats",
           "-progress", "pipe:1", "-i", video]
    if opt.get("loop"):
        cmd += ["-stream_loop", "-1"]
    cmd += ["-i", audio]

    maps = ["-map", "0:v:0"]
    if filt:
        parts = []
        if delay > 0:
            parts.append(f"adelay={int(round(delay * 1000))}:all=1")
        g = num(opt, "gain")
        if abs(g - 100.0) > 0.5:
            parts.append(f"volume={g / 100.0:.4f}")
        fi = num(opt, "fade_in")
        if fi > 0:
            parts.append(f"afade=t=in:st={delay:.3f}:d={fi:.3f}")
        fo = num(opt, "fade_out")
        if fo > 0:
            # 淡出要在配乐真正收尾处落下；循环/截断都会把落点拉到视频末尾
            st = max(delay, music_end(vdur, adur, opt) - fo)
            parts.append(f"afade=t=out:st={st:.3f}:d={fo:.3f}")
        chain = ["[1:a]" + (",".join(parts) if parts else "anull") + "[mu]"]
        tail = "[mu]"
        if opt.get("mode") == "mix":
            # normalize=0 是关键：amix 默认按输入路数把声音除以 2，
            # 混完比原声小 6dB，用户只觉得"一混合就变轻"却查不到原因。
            first = "[0:a]"
            if abs(num(opt, "orig_gain") - 100.0) > 0.5:
                og = num(opt, "orig_gain") / 100.0
                chain.append(f"[0:a]volume={og:.4f}[vo]")
                first = "[vo]"
            chain.append(f"{first}{tail}amix=inputs=2:duration=longest:"
                         f"normalize=0[am]")
            tail = "[am]"
        maps += ["-filter_complex", ";".join(chain), "-map", tail]
    else:
        maps += ["-map", "1:a:0"]

    cmd += maps
    cmd += ["-c:v", "copy"]
    ac = opt.get("acodec") or "auto"
    if ac == "copy" and not filt:
        cmd += ["-c:a", "copy"]
    elif ac in BITS:
        cmd += ["-c:a", "aac", "-b:a", BITS[ac]]
    elif ac == "auto" and copy_ok(ext, ainfo.get("codec"), opt):
        cmd += ["-c:a", "copy"]
    else:
        cmd += ["-c:a", "aac", "-b:a", "192k"]

    lim = limit_dur(vdur, adur, opt)
    if lim > 0:
        cmd += ["-t", f"{lim:.3f}"]
    if ext in (".mp4", ".m4v", ".mov"):
        cmd += ["-movflags", "+faststart"]
    cmd.append(out)
    return cmd, total_dur(vdur, adur, opt)
