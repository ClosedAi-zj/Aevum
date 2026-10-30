# -*- coding: utf-8 -*-
"""延时照片对齐：估计每帧位移 → 平滑 → 统一裁切输出。

四种估计器：
  "template"  底部模板匹配（TM_CCOEFF_NORMED + 抛物线亚像素）。对相机下坠造成
              的垂直漂移最稳，是 lr3 成片用的那套。
  "phase"     相位相关（cv2.phaseCorrelate），可指定 ROI 与亮度归一化。
  "ecc"       增强相关系数（cv2.findTransformECC），可选纯平移 / 平移+旋转 /
              仿射 / 单应；旋转与缩放用 2×3（单应 3×3）矩阵贯穿裁切与写出。
  "feature"   特征点 + RANSAC（ORB/SIFT，带防幻觉校验与逐帧降级）。
              大位移、画面被遮挡、或需要旋转补偿时用；矩阵同 ECC 贯穿全流程。

位移统一约定为 "warpAffine 里要加的偏移"：dst(x, y) = src(x - off_x, y - off_y)。
相位相关给出的是"当前帧相对参考帧动了多少"，取负号后才与模板匹配的约定一致。
ECC / 特征点的矩阵同样是"施加矩阵"：直接交给 cv2.warpAffine / warpPerspective。
"""
import math
import os
import time
from multiprocessing import Pool, cpu_count

import cv2
import numpy as np

from lapse import imio

ESTIMATORS = ("template", "phase", "ecc", "feature")
# ECC 的运动模型：平移 / 平移+旋转 / 仿射（+缩放剪切）/ 单应（3×3）
MOTIONS = {"trans": "MOTION_TRANSLATION",
           "euclid": "MOTION_EUCLIDEAN",
           "affine": "MOTION_AFFINE",
           "homography": "MOTION_HOMOGRAPHY"}
SMALL_BATCH = 12          # 少于这么多帧就别起进程池，spawn 比算图还贵

# 子进程里传参考图用不到类，模块级 dict 当上下文最省事（Pool initializer 填）
_G = {}


def to_gray(bgr):
    """CLAHE 均衡后的灰度：延时序列曝光会渐变，直方图拉平后模板匹配更稳。"""
    gray = cv2.cvtColor(bgr, cv2.COLOR_BGR2GRAY)
    clahe = cv2.createCLAHE(clipLimit=2.0, tileGridSize=(8, 8))
    return clahe.apply(gray)


def _parab(a, b, c):
    """三点抛物线插值，返回峰值相对中间点的偏移（-1~1）。"""
    den = a - 2.0 * b + c
    return 0.0 if abs(den) < 1e-12 else float(np.clip(0.5 * (a - c) / den, -1.0, 1.0))


def _match_shape(img, ref_shape):
    ref_h, ref_w = ref_shape
    if img.shape[:2] != (ref_h, ref_w):
        img = cv2.resize(img, (ref_w, ref_h), interpolation=cv2.INTER_AREA)
    return img


# ---------------- 估计器 1：模板匹配 ----------------
def _init_template(ref_shape, template_bytes, ref_pos, search_box):
    _G['ref_shape'] = ref_shape
    _G['ref_pos'] = ref_pos          # (tx, ty) 模板左上角在参考图中的坐标
    _G['search_box'] = search_box    # (sx, sy, ex, ey) 当前帧搜索区域
    buf = np.frombuffer(template_bytes, dtype=np.uint8)
    _G['template'] = cv2.imdecode(buf, cv2.IMREAD_GRAYSCALE)
    cv2.setNumThreads(1)


def _worker_template(task):
    i, filepath, bps, half = task
    img = imio.decode_image(filepath, bps, half)
    if img is None:
        return i, None, 0.0, "读取失败"
    ref_h, ref_w = _G['ref_shape']
    img = _match_shape(img, (ref_h, ref_w))

    gray = to_gray(img)
    template = _G['template']
    th, tw = template.shape

    sx, sy, ex, ey = _G['search_box']
    sx, sy = max(0, sx), max(0, sy)
    ex, ey = min(ref_w, ex), min(ref_h, ey)
    if ex - tw <= sx or ey - th <= sy:
        return i, (0.0, 0.0), 0.0, "搜索区域太小"

    res = cv2.matchTemplate(gray[sy:ey, sx:ex], template, cv2.TM_CCOEFF_NORMED)
    _, max_val, _, (mx, my) = cv2.minMaxLoc(res)
    rh, rw = res.shape

    # 亚像素：对峰值做抛物线拟合。峰值贴边时跳过——真实位置在窗外，拟合无意义。
    sub_x = _parab(res[my, mx - 1], res[my, mx], res[my, mx + 1]) if 0 < mx < rw - 1 else 0.0
    sub_y = _parab(res[my - 1, mx], res[my, mx], res[my + 1, mx]) if 0 < my < rh - 1 else 0.0

    # 模板在参考图中的位置减去它在这张图里的位置 = 需要施加的平移
    tx, ty = _G['ref_pos']
    # 静止时峰值应在搜索窗内的 (tx-sx, ty-sy)；只有搜索余量被削到 0 时它才会
    # 正好落在边界上，那种情况不能算"被夹住"，否则每帧都误报。
    still_x, still_y = tx - sx, ty - sy
    at_edge = ((mx == 0 or mx == rw - 1) and mx != still_x) or \
              ((my == 0 or my == rh - 1) and my != still_y)
    return i, (float(tx - (sx + mx + sub_x)),
               float(ty - (sy + my + sub_y))), float(max_val),         ("峰值贴边，位移被搜索窗夹住" if at_edge else "OK")


def _setup_template(ref_gray, ref_shape, template_start, search_margin):
    """取底部中间 60% 作模板：天空/云会整体漂移，画面下缘的静态结构才可靠。

    模板底边不贴画面底边，留一小条护带：模板一旦贴底，画面内容往下走时
    模板也跟着出画，搜索必然顶在底边，那半个方向的位移就永远测成 0。
    """
    ref_h, ref_w = ref_shape
    ty = int(ref_h * template_start)
    tx = int(ref_w * 0.2)
    tw = int(ref_w * 0.6)
    guard = max(8, int(round(ref_h * 0.04)))
    guard = min(guard, max(8, (ref_h - ty) // 3))
    template = ref_gray[ty:ref_h - guard, tx:tx + tw]
    m = search_margin
    search_box = (tx - m, ty - m, tx + tw + m, ref_h + m)
    png = cv2.imencode('.png', template)[1].tobytes()
    return ((ref_h, ref_w), png, (tx, ty), search_box), template.shape


# ---------------- 估计器 2：相位相关 ----------------
def _roi_px(roi, W, H):
    x1, y1, x2, y2 = roi
    return (max(0, int(round(W * x1))), max(0, int(round(H * y1))),
            min(W, int(round(W * x2))), min(H, int(round(H * y2))))


def _init_phase(ref_gray_png, ref_shape, roi, normalize):
    _G['ref_shape'] = ref_shape
    _G['normalize'] = normalize
    ref = cv2.imdecode(np.frombuffer(ref_gray_png, dtype=np.uint8),
                       cv2.IMREAD_GRAYSCALE)
    if ref is None:
        raise RuntimeError("参考灰度解码失败")
    H, W = ref_shape
    x1, y1, x2, y2 = _roi_px(roi, W, H)
    patch = ref[y1:y2, x1:x2].astype(np.float32)
    if normalize:
        patch = (patch - patch.mean()) / (patch.std() + 1e-6)
    _G['ref_roi'] = patch
    _G['roi_px'] = (x1, y1, x2, y2)
    _G['hann'] = cv2.createHanningWindow((x2 - x1, y2 - y1), cv2.CV_32F)
    cv2.setNumThreads(1)


def _worker_phase(task):
    i, filepath, bps, half = task
    try:
        img = imio.decode_image(filepath, bps, half)
    except Exception as e:
        return i, None, 0.0, f"解码失败: {e}"
    if img is None:
        return i, None, 0.0, "读取失败"
    img = _match_shape(img, _G['ref_shape'])
    x1, y1, x2, y2 = _G['roi_px']
    cur = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY).astype(np.float32)[y1:y2, x1:x2]
    if cur.shape != _G['ref_roi'].shape:
        return i, None, 0.0, f"ROI 尺寸不一致 {cur.shape} vs {_G['ref_roi'].shape}"
    if _G['normalize']:
        cur = (cur - cur.mean()) / (cur.std() + 1e-6)
    try:
        (dx, dy), resp = cv2.phaseCorrelate(_G['ref_roi'] * _G['hann'],
                                           cur * _G['hann'])
    except cv2.error as e:
        return i, None, 0.0, f"phaseCorrelate 失败: {e}"
    return i, (-float(dx), -float(dy)), float(resp), "OK"


def _setup_phase(ref_gray, ref_shape, roi):
    png = cv2.imencode('.png', ref_gray)[1].tobytes()
    return (png, ref_shape, roi)


# ---------------- 估计器 3：ECC（旋转 / 缩放 / 单应） ----------------
def _init_ecc(ref_gray_png, ref_shape, motion, iters, eps, gauss):
    ref = cv2.imdecode(np.frombuffer(ref_gray_png, dtype=np.uint8),
                       cv2.IMREAD_GRAYSCALE)
    if ref is None:
        raise RuntimeError("参考灰度解码失败")
    _G['ref'] = ref.astype(np.float32) / 255.0
    _G['ref_shape'] = ref_shape
    _G['motion'] = getattr(cv2, MOTIONS[motion])
    _G['criteria'] = (cv2.TERM_CRITERIA_EPS | cv2.TERM_CRITERIA_COUNT,
                      int(iters), float(eps))
    _G['gauss'] = int(gauss)
    cv2.setNumThreads(1)


def _worker_ecc(task):
    i, filepath, bps, half = task
    try:
        img = imio.decode_image(filepath, bps, half)
    except Exception as e:
        return i, None, 0.0, f"解码失败: {e}"
    if img is None:
        return i, None, 0.0, "读取失败"
    img = _match_shape(img, _G['ref_shape'])
    cur = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY).astype(np.float32) / 255.0

    homography = _G['motion'] == cv2.MOTION_HOMOGRAPHY
    m = np.eye(3, 3, dtype=np.float32) if homography else \
        np.eye(2, 3, dtype=np.float32)
    try:
        cc, warp = cv2.findTransformECC(_G['ref'], cur, m, _G['motion'],
                                        _G['criteria'], None, _G['gauss'])
    except cv2.error as e:
        return i, None, 0.0, f"ECC 失败: {e}"
    # findTransformECC 给出的是 template→input 的映射；统一成"施加矩阵"
    inv = (np.linalg.inv(warp.astype(np.float64)).astype(np.float32)
           if homography else cv2.invertAffineTransform(warp))
    dx, dy = float(inv[0, 2]), float(inv[1, 2])
    return i, (dx, dy), float(cc), "OK", inv


# ---------------- 估计器 4：特征点 + RANSAC ----------------
# 降级阶梯借鉴 Futura Time Lapse 的 S1/S2/S4/S5：常规 ORB → 降阈值多抓点 →
# 换 SIFT → Sobel 边缘上提 SIFT。每帧独立走一遍，第一档成功就停。
_FEATURE_WORK_MAX = 1200        # 检测在最长边不超过它的缩略图上做，矩阵再放大
_FEATURE_MIN_INLIERS = 8        # 少于这个内点不可信（3 点就能解出相似变换）
_FEATURE_SCALE_BAND = 0.15      # 相似变换缩放偏离 1 超过它判幻觉（延时不会变焦）
_FEATURE_MAX_RZ = 90.0          # cos(rz) < 0 即画面翻转，必是误配（防 glitch）
_FEATURE_SPREAD_MIN = 0.03      # 内点 bbox 短边占画面短边的最小比例
# _FEATURE_ATTEMPTS: (检测器, 点数上限, FAST 阈值, Lowe 比率)
# 第三档直接 SIFT：换一族描述子救 ORB 误配的场景（OpenCV 5 已移除 AKAZE）
_FEATURE_ATTEMPTS = (
    ("orb", 1500, 20, 1.2),
    ("orb", 4000, 10, 1.3),
    ("sift", 4000, None, 1.3),
    ("sobel_sift", 4000, None, 1.3),
)


def _feature_detect(kind, gray, n, fast):
    """按档位检测特征点，返回 (关键点, 描述子, 匹配距离范型)。"""
    if kind == "orb":
        det = cv2.ORB_create(nfeatures=n or 1500)
        if fast is not None:
            det.setFastThreshold(fast)
        kps, des = det.detectAndCompute(gray, None)
        return kps, des, cv2.NORM_HAMMING
    if kind == "sift":
        kps, des = cv2.SIFT_create(nfeatures=n or 4000) \
            .detectAndCompute(gray, None)
        return kps, des, cv2.NORM_L2
    # sobel_sift：平坦低对比场景里直接提 SIFT 提不出点，改在 Sobel 梯度幅值图
    # 上提——延时的天空/雾景里只有地平线和剪影有梯度，正好聚焦到这些结构上
    gx = cv2.Sobel(gray, cv2.CV_32F, 1, 0, ksize=3)
    gy = cv2.Sobel(gray, cv2.CV_32F, 0, 1, ksize=3)
    mag = cv2.magnitude(gx, gy)
    cv2.normalize(mag, mag, 0, 255, cv2.NORM_MINMAX)
    kps, des = cv2.SIFT_create(nfeatures=n or 4000) \
        .detectAndCompute(mag.astype(np.uint8), None)
    return kps, des, cv2.NORM_L2


def _feature_mutual(des_ref, des_cur, norm, ratio):
    """knn + Lowe 比率 + 互为最佳。互最佳天然一对一，无需再去重。"""
    if des_ref is None or des_cur is None \
            or len(des_ref) < 2 or len(des_cur) < 2:
        return []
    bf = cv2.BFMatcher(norm)
    pairs = bf.knnMatch(des_ref, des_cur, k=2)
    good = [m[0] for m in pairs
            if len(m) == 2 and m[0].distance < ratio * m[1].distance]
    if not good:
        return []
    back = bf.knnMatch(des_cur, des_ref, k=1)
    reverse = {m[0].queryIdx: m[0].trainIdx for m in back if m}
    return [m for m in good if reverse.get(m.trainIdx) == m.queryIdx]


def _feature_validate(M, pts_cur, inliers, w, h):
    """防幻觉校验，借鉴 Futura 的四道闸，返回 (错误说明 | None)。

    M 是 RANSAC 拟合的 ref→cur 相似变换：缩放与旋转角看它，内点散布看
    当前帧上内点的包围盒。
    """
    n_in = int(inliers.sum())
    if n_in < _FEATURE_MIN_INLIERS:
        return f"内点只剩 {n_in}"
    a, b, c, d = (float(M[0, 0]), float(M[0, 1]),
                  float(M[1, 0]), float(M[1, 1]))
    scale = math.sqrt((a * a + b * b + c * c + d * d) / 2.0)
    if abs(scale - 1.0) > _FEATURE_SCALE_BAND:
        return f"缩放 {scale:.3f} 超出 ±{_FEATURE_SCALE_BAND}"
    rz = math.degrees(math.atan2(c, a))
    if abs((rz + 180.0) % 360.0 - 180.0) > _FEATURE_MAX_RZ:
        return f"旋转 {rz:.1f}° 超过 {_FEATURE_MAX_RZ:.0f}°"
    ip = pts_cur[inliers.ravel().astype(bool)]
    bw, bh = float(np.ptp(ip[:, 0])), float(np.ptp(ip[:, 1]))
    if max(bw, bh) < 0.25 * max(w, h) \
            or min(bw, bh) < max(12.0, _FEATURE_SPREAD_MIN * min(w, h)):
        return f"内点太集中 bbox {bw:.0f}x{bh:.0f}"
    return None


def _init_feature(ref_gray_png, ref_shape):
    ref = cv2.imdecode(np.frombuffer(ref_gray_png, dtype=np.uint8),
                       cv2.IMREAD_GRAYSCALE)
    if ref is None:
        raise RuntimeError("参考灰度解码失败")
    _G['ref_shape'] = ref_shape
    h, w = ref.shape
    s = max(w, h) / _FEATURE_WORK_MAX
    if s > 1.0:
        ref = cv2.resize(ref, (max(1, int(round(w / s))),
                               max(1, int(round(h / s)))),
                         interpolation=cv2.INTER_AREA)
    # 每档缓存参考侧 (关键点坐标, 描述子)：互最佳要用 queryIdx 回查坐标
    _G['ref_feature'] = []
    for kind, n, fast, _ratio in _FEATURE_ATTEMPTS:
        kps, des, _norm = _feature_detect(kind, ref, n, fast)
        pts = (np.float32([k.pt for k in kps]) if kps
               else np.empty((0, 2), np.float32))
        _G['ref_feature'].append((pts, des))
    cv2.setNumThreads(1)


def _worker_feature(task):
    i, filepath, bps, half = task
    try:
        img = imio.decode_image(filepath, bps, half)
    except Exception as e:
        return i, None, 0.0, f"解码失败: {e}"
    if img is None:
        return i, None, 0.0, "读取失败"
    img = _match_shape(img, _G['ref_shape'])
    gray = to_gray(img)
    h, w = gray.shape
    s = max(w, h) / _FEATURE_WORK_MAX
    if s > 1.0:
        work = cv2.resize(gray, (max(1, int(round(w / s))),
                                 max(1, int(round(h / s)))),
                          interpolation=cv2.INTER_AREA)
        s = w / work.shape[1]
    else:
        work, s = gray, 1.0
    wh, ww = work.shape
    last = "两图都提不出特征点"
    for ai, (kind, n, fast, ratio) in enumerate(_FEATURE_ATTEMPTS):
        kps_cur, des_cur, norm = _feature_detect(kind, work, n, fast)
        pts_ref, des_ref = _G['ref_feature'][ai]
        n_det = 0 if kps_cur is None else len(kps_cur)
        if des_cur is None or des_ref is None or n_det < _FEATURE_MIN_INLIERS:
            last = f"{kind} 特征点不足（当前 {n_det}）"
            continue
        pairs = _feature_mutual(des_ref, des_cur, norm, ratio)
        if len(pairs) < _FEATURE_MIN_INLIERS:
            last = f"{kind} 互最佳匹配只剩 {len(pairs)} 对"
            continue
        pts_r = pts_ref[[m.queryIdx for m in pairs]]
        pts_c = np.float32([kps_cur[m.trainIdx].pt for m in pairs])
        T, inl = cv2.estimateAffinePartial2D(
            pts_r, pts_c, method=cv2.RANSAC,
            ransacReprojThreshold=3.0, maxIters=5000, confidence=0.995)
        if T is None or inl is None:
            last = f"{kind} RANSAC 无解"
            continue
        err = _feature_validate(T, pts_c, inl, ww, wh)
        if err:
            last = f"{kind}: {err}"
            continue
        # 施加矩阵 = ref→cur 相似变换的逆（cur→ref），与 ECC 的 mats 同约定；
        # 平移是在缩略图上算的，按缩放比还原到全尺寸，线性部分不受均匀缩放影响
        M = cv2.invertAffineTransform(T)
        M[0, 2] *= s
        M[1, 2] *= s
        score = float(inl.sum()) / len(pairs)
        return i, (float(M[0, 2]), float(M[1, 2])), score, "OK", \
            M.astype(np.float32)
    return i, None, 0.0, f"特征点估计失败：{last}"


# ---------------- 估计入口 ----------------
def measure_offsets(files, ref_index, *, est="template", workers=0,
                    template_start=0.7, search_margin=150,
                    roi=(0.0, 0.6, 1.0, 1.0), normalize=True,
                    motion="euclid", ecc_iters=50, ecc_eps=1e-4,
                    ecc_gauss=5, bps=8, half=False, on_progress=None,
                    mats=None):
    """返回 (offsets, scores, issues, 用时)。offsets[i] = (dx, dy) 是要施加的
    平移；issues 是读不出来或读数可疑的帧 (索引, 说明)。

    est="ecc" 或 "feature" 时传 mats={}：主进程会填满每帧的施加矩阵
    （2×3，单应 3×3），旋转/缩放只有矩阵里才有；offsets 仍是矩阵的平移列，
    供平滑与统计使用。
    """
    if est not in ESTIMATORS:
        raise ValueError(f"est 只能是 {ESTIMATORS}")
    if motion not in MOTIONS:
        raise ValueError(f"motion 只能是 {tuple(MOTIONS)}")
    n = len(files)
    ref_img = imio.decode_image(files[ref_index], 8, half)
    if ref_img is None:
        raise RuntimeError(f"无法读取参考图: {files[ref_index]}")
    ref_shape = ref_img.shape[:2]
    ref_gray = to_gray(ref_img) if est in ("template", "feature") else \
        cv2.cvtColor(ref_img, cv2.COLOR_BGR2GRAY)
    del ref_img

    if est == "template":
        init_args, _ = _setup_template(ref_gray, ref_shape,
                                       template_start, search_margin)
        initializer, worker = _init_template, _worker_template
    elif est == "phase":
        init_args = _setup_phase(ref_gray, ref_shape, roi) + (normalize,)
        initializer, worker = _init_phase, _worker_phase
    elif est == "feature":
        png = cv2.imencode('.png', ref_gray)[1].tobytes()
        init_args = (png, ref_shape)
        initializer, worker = _init_feature, _worker_feature
    else:
        png = cv2.imencode('.png', ref_gray)[1].tobytes()
        init_args = (png, ref_shape, motion, ecc_iters, ecc_eps, ecc_gauss)
        initializer, worker = _init_ecc, _worker_ecc

    tasks = [(i, files[i], bps, half) for i in range(n)]
    offsets, scores, issues = {}, {}, []
    w = workers if workers > 0 else max(1, cpu_count() - 1)
    w = min(w, n)
    done = 0

    def consume(r):
        nonlocal done
        done += 1
        i, val, score, msg = r[:4]
        if val is None:
            issues.append((i, msg))
            offsets[i], scores[i] = (0.0, 0.0), 0.0
        else:
            offsets[i] = val
            scores[i] = score
            if mats is not None and len(r) > 4:
                mats[i] = np.asarray(r[4], np.float32)
            if msg != "OK":
                issues.append((i, msg))
        if on_progress:
            on_progress(done, n)

    t0 = time.time()
    if n <= SMALL_BATCH:
        # Windows 起进程池是 spawn：每个 worker 重新 import cv2，光启动就要几秒。
        # 预检十来帧的活还不如在任务线程里直接算。
        old = cv2.getNumThreads()
        initializer(*init_args)
        try:
            for task in tasks:
                consume(worker(task))
        finally:
            cv2.setNumThreads(old)
    else:
        with Pool(w, initializer=initializer, initargs=init_args) as pool:
            for r in pool.imap_unordered(worker, tasks, chunksize=2):
                consume(r)
    if mats is not None:
        for i in range(n):
            if i not in mats:
                dx, dy = offsets[i]
                mats[i] = np.array([[1.0, 0.0, dx], [0.0, 1.0, dy]],
                                   dtype=np.float32)
    return offsets, scores, issues, time.time() - t0


# ---------------- 平滑 ----------------
def preserve_outliers(vals, scores, half, jump_px, min_score):
    """局部中值平滑，但把"相机真的动了"的帧原样保留。

    中值滤波默认假设离群=噪声，可延时拍摄里一次被碰、快门震动产生的
    单帧大位移同样是离群值，抹掉它反而会把抖动留在成品里。区分依据是匹配
    置信度：误匹配的 score 会掉，真实抖动的 score 依然很高。
    """
    n = len(vals)
    med = np.empty(n, dtype=np.float64)
    for k in range(n):
        lo, hi = max(0, k - half), min(n, k + half + 1)
        med[k] = np.median(vals[lo:hi])
    real = (np.abs(vals - med) > jump_px) & (scores >= min_score)
    return np.where(real, vals, med), int(real.sum())


def smooth_offsets(offsets, scores, window, jump_px, min_score):
    """就地平滑位移序列，返回 (新 offsets, dx 保留帧数, dy 保留帧数)。"""
    n = len(offsets)
    dxs = np.array([offsets[i][0] for i in range(n)], dtype=np.float64)
    dys = np.array([offsets[i][1] for i in range(n)], dtype=np.float64)
    scs = np.array([scores[i] for i in range(n)], dtype=np.float64)
    half = max(1, window // 2)
    dxs, kx = preserve_outliers(dxs, scs, half, jump_px, min_score)
    dys, ky = preserve_outliers(dys, scs, half, jump_px, min_score)
    return {i: (float(dxs[i]), float(dys[i])) for i in range(n)}, kx, ky


# ---------------- 公共裁切 ----------------
def as_matrix(v):
    """(dx, dy) 或 2×3 / 3×3 施加矩阵 → float64 矩阵。"""
    a = np.asarray(v, dtype=np.float64)
    if a.ndim == 2 and a.shape in ((2, 3), (3, 3)):
        return a
    return np.array([[1.0, 0.0, float(v[0])],
                     [0.0, 1.0, float(v[1])]], dtype=np.float64)


def _apply_pts(M, pts):
    """施加矩阵把源坐标映到目标坐标（与 cv2.warpAffine 默认语义一致）。"""
    if M.shape == (3, 3):
        ph = np.hstack([pts, np.ones((len(pts), 1))]) @ M.T
        return ph[:, :2] / ph[:, 2:3]
    return pts @ M[:, :2].T + M[:, 2]


def _inverse(M):
    """施加矩阵的逆：2×3 或 3×3。"""
    if M.shape == (3, 3):
        return np.linalg.inv(M)
    inv_a = np.linalg.inv(M[:, :2])
    return np.hstack([inv_a, -inv_a @ M[:, 2:3]])


def retranslate(mats, offsets):
    """平滑完平移后，把新平移写回矩阵，旋转/缩放部分保持不动。"""
    out = {}
    for i, M in mats.items():
        m = np.array(M, dtype=np.float64, copy=True)
        m[0, 2], m[1, 2] = offsets[i]
        out[i] = m.astype(np.float32)
    return out


def rotation_deg(M):
    """矩阵里的旋转角，正值 = 画面逆时针（getRotationMatrix2D 的约定）。"""
    m = as_matrix(M)
    return float(np.degrees(np.arctan2(float(m[1, 0]), float(m[0, 0]))))


def common_crop(offsets, w, h):
    """所有帧变换后都还有效的那块矩形。

    offsets 的值可以是纯平移 (dx, dy)，也可以是 2×3 / 3×3 施加矩阵：
    把画面四角变换到目标坐标，取全体帧的交集。有旋转时再把交集按中心
    缩放，保证裁切框四个角都真的落在每一帧里（旋转后的四边不再是轴对齐，
    直接取包围盒会在角上带进画面外）。
    """
    mats = [as_matrix(v) for v in offsets.values()]
    if not mats:
        return 0, 0, w, h
    corners = np.array([[0.0, 0.0], [w, 0.0], [0.0, h], [w, h]])
    pts = np.stack([_apply_pts(M, corners) for M in mats])   # (n,4,2)
    x1 = max(0.0, float(np.ceil(pts[..., 0].min(axis=1).max())))
    x2 = min(float(w), float(np.floor(pts[..., 0].max(axis=1).min())))
    y1 = max(0.0, float(np.ceil(pts[..., 1].min(axis=1).max())))
    y2 = min(float(h), float(np.floor(pts[..., 1].max(axis=1).min())))
    if x2 - x1 < 2 or y2 - y1 < 2:
        return int(x1), int(y1), int(x2), int(y2)

    rotated = any(abs(float(M[0, 1])) > 1e-9 or abs(float(M[1, 0])) > 1e-9
                  or (M.shape == (3, 3)
                      and (abs(float(M[2, 0])) > 1e-9
                           or abs(float(M[2, 1])) > 1e-9))
                  for M in mats)
    if not rotated:
        return int(x1), int(y1), int(x2), int(y2)

    invs = [_inverse(M) for M in mats]
    cx, cy = (x1 + x2) * 0.5, (y1 + y2) * 0.5
    hx, hy = (x2 - x1) * 0.5, (y2 - y1) * 0.5

    def fits(s):
        rect = np.array([[cx - hx * s, cy - hy * s],
                         [cx + hx * s, cy - hy * s],
                         [cx - hx * s, cy + hy * s],
                         [cx + hx * s, cy + hy * s]])
        for inv in invs:
            if inv.shape == (3, 3):
                ph = np.hstack([rect, np.ones((4, 1))]) @ inv.T
                q = ph[:, :2] / ph[:, 2:3]
            else:
                q = rect @ inv[:, :2].T + inv[:, 2]
            if (q < -1e-6).any() or (q[:, 0] > w + 1e-6).any() \
                    or (q[:, 1] > h + 1e-6).any():
                return False
        return True

    if fits(1.0):
        return int(x1), int(y1), int(x2), int(y2)
    lo, hi = 0.0, 1.0
    for _ in range(30):
        mid = (lo + hi) * 0.5
        if fits(mid):
            lo = mid
        else:
            hi = mid
    s = lo * 0.999
    nx1 = int(np.ceil(cx - hx * s))
    nx2 = int(np.floor(cx + hx * s))
    ny1 = int(np.ceil(cy - hy * s))
    ny2 = int(np.floor(cy + hy * s))
    if nx2 - nx1 < 2 or ny2 - ny1 < 2:
        return int(x1), int(y1), int(x2), int(y2)
    return nx1, ny1, nx2, ny2


# ---------------- 应用 ----------------
def _init_apply(ref_shape, bps, half):
    _G['ref_shape'] = ref_shape
    _G['bps'] = bps
    _G['half'] = half
    cv2.setNumThreads(1)


def _worker_apply(task):
    i, filepath, output_path, off, crop, mat = task
    try:
        img = imio.decode_image(filepath, _G['bps'], _G['half'])
    except Exception:
        return i, False
    if img is None:
        return i, False
    ref_h, ref_w = _G['ref_shape']
    img = _match_shape(img, (ref_h, ref_w))

    if mat is not None:
        M = np.asarray(mat, dtype=np.float32)
        if M.shape == (3, 3):
            img = cv2.warpPerspective(img, M, (ref_w, ref_h),
                                      flags=cv2.INTER_LANCZOS4,
                                      borderMode=cv2.BORDER_REFLECT_101)
        elif not np.allclose(M, np.float32([[1, 0, 0], [0, 1, 0]]),
                             atol=1e-6):
            img = cv2.warpAffine(img, M, (ref_w, ref_h),
                                 flags=cv2.INTER_LANCZOS4,
                                 borderMode=cv2.BORDER_REFLECT_101)
    else:
        dx, dy = off
        if abs(dx) > 1e-6 or abs(dy) > 1e-6:
            img = cv2.warpAffine(img, np.float32([[1, 0, dx], [0, 1, dy]]),
                                 (ref_w, ref_h), flags=cv2.INTER_LANCZOS4,
                                 borderMode=cv2.BORDER_REFLECT_101)
    x1, y1, x2, y2 = crop
    return i, imio.cv_imwrite(output_path, img[y1:y2, x1:x2])


def apply_offsets(files, out_dir, offsets, crop, ref_shape, *,
                  out_ext="jpg", bps=8, half=False, workers=0,
                  on_progress=None, mats=None):
    """平移/矩阵 + 统一裁切，写出 aligned_0000.<ext>，返回输出路径列表。

    mats 给出时优先用矩阵（可带旋转/缩放/透视），否则按 offsets 平移。
    """
    os.makedirs(out_dir, exist_ok=True)
    n = len(files)
    tasks = [(i, files[i],
              os.path.join(out_dir, f"aligned_{i:04d}.{out_ext}"),
              offsets.get(i, (0.0, 0.0)), crop,
              None if mats is None else mats.get(i)) for i in range(n)]
    w = workers if workers > 0 else max(1, cpu_count() - 1)
    w = min(w, n)
    done = 0
    failed = []

    def consume(i, ok):
        nonlocal done
        done += 1
        if not ok:
            failed.append(i)
        if on_progress:
            on_progress(done, n)

    if n <= SMALL_BATCH:
        old = cv2.getNumThreads()
        _init_apply(ref_shape, bps, half)
        try:
            for task in tasks:
                consume(*_worker_apply(task))
        finally:
            cv2.setNumThreads(old)
    else:
        with Pool(w, initializer=_init_apply,
                  initargs=(ref_shape, bps, half)) as pool:
            for i, ok in pool.imap_unordered(_worker_apply, tasks, chunksize=4):
                consume(i, ok)
    return [t[2] for t in tasks], failed


# ---------------- 校验 ----------------
def verify_residuals(paths, roi=(0.0, 0.6, 1.0, 1.0), ref_index=0,
                     normalize=True):
    """对已对齐的结果再测一次相对参考帧的残余平移，抽帧即可。

    返回 [(原索引, (dx, dy) | None, 响应)]，用相位相关而不是模板匹配：
    残余在亚像素级，需要 phaseCorrelate 的频域精度。
    """
    patches = []
    for p in paths:
        img = imio.decode_image(p, 8)
        if img is None:
            patches.append(None)
            continue
        g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY).astype(np.float32)
        H, W = g.shape
        x1, y1, x2, y2 = _roi_px(roi, W, H)
        patch = g[y1:y2, x1:x2]
        if normalize:
            patch = (patch - patch.mean()) / (patch.std() + 1e-6)
        patches.append(patch * cv2.createHanningWindow((x2 - x1, y2 - y1),
                                                      cv2.CV_32F))
    ref = patches[ref_index] if 0 <= ref_index < len(patches) else None
    out = []
    for i, p in enumerate(patches):
        if p is None or ref is None:
            out.append((i, None, 0.0))
        elif i == ref_index:
            out.append((i, (0.0, 0.0), 1.0))
        else:
            (dx, dy), resp = cv2.phaseCorrelate(ref, p)
            out.append((i, (float(dx), float(dy)), float(resp)))
    return out


def sample_indices(n, k, seed=0):
    """从 n 帧里均匀随机抽 k 个索引（预检用，0 与 n-1 之外都靠随机）。"""
    if k >= n:
        return list(range(n))
    rng = np.random.default_rng(seed)
    idx = [0]
    for j in range(1, k):
        lo = 1 + (j - 1) * (n - 1) // (k - 1)
        hi = 1 + j * (n - 1) // (k - 1)
        if hi <= lo:
            hi = lo + 1
        idx.append(int(rng.integers(lo, hi)))
    return sorted(set(idx))
