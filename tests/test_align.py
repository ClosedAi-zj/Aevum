# -*- coding: utf-8 -*-
"""lapse/align.py 回归测试：位移估计精度 + "真实抖动不被中值平滑抹掉"。

后一条是 lr3 那次翻车的根因：相机被碰了一下产生的单帧大位移，和误匹配在数值
上都是离群点，只有匹配置信度能分开它们。这个测试就是守住这条。
运行：PYTHONIOENCODING=utf-8 ./.venv/Scripts/python.exe tests/test_align.py
"""
import math
import os
import sys

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
TMP = os.path.join(HERE, "_tmp", "align")

from lapse import align  # noqa: E402

FAIL = []
TOTAL = [0]


def check(name, cond, detail=""):
    TOTAL[0] += 1
    print(f"[{'PASS' if cond else 'FAIL'}] {name}  {detail}", flush=True)
    if not cond:
        FAIL.append(name)


def make_base(w=1000, h=900, seed=7):
    """造一张"像照片"的图：大尺度明暗 + 中等纹理。纯噪声会被 JPEG 和 CLAHE
    联手打散，模板匹配置信度直接掉到 0.05，测不出对齐本身。"""
    rng = np.random.default_rng(seed)
    low = cv2.resize(rng.random((h // 24, w // 24)).astype(np.float32),
                     (w, h), interpolation=cv2.INTER_CUBIC) * 120
    mid = cv2.GaussianBlur(rng.normal(0, 1, (h, w)).astype(np.float32),
                           (0, 0), 2.2) * 26
    im = np.clip(low + mid + 60, 0, 255).astype(np.uint8)
    for _ in range(40):                       # 下缘放些硬结构当匹配锚点
        x, y = int(rng.integers(0, w - 60)), int(rng.integers(int(h * .68), h - 30))
        cv2.rectangle(im, (x, y), (x + int(rng.integers(10, 60)), y + 20),
                      int(rng.integers(0, 256)), -1)
    return cv2.cvtColor(im, cv2.COLOR_GRAY2BGR)


def write_shifted(base, shifts, d, ext="png"):
    """按真实位移生成序列；warp 用 +s，对齐估出来的偏移应是 -s。"""
    os.makedirs(d, exist_ok=True)
    h, w = base.shape[:2]
    for i, (sx, sy) in enumerate(shifts):
        f = cv2.warpAffine(base, np.float32([[1, 0, sx], [0, 1, sy]]), (w, h),
                           borderMode=cv2.BORDER_REPLICATE)
        cv2.imwrite(os.path.join(d, f"clip_{i:04d}.{ext}"), f)
    return [os.path.join(d, f"clip_{i:04d}.{ext}") for i in range(len(shifts))]


def write_rotated(base, angles, d):
    """逐帧绕中心旋转 angle 度；ECC 估出来的补偿矩阵应逐帧抵消它。"""
    os.makedirs(d, exist_ok=True)
    h, w = base.shape[:2]
    files = []
    for i, ang in enumerate(angles):
        m = cv2.getRotationMatrix2D((w / 2.0, h / 2.0), ang, 1.0)
        f = cv2.warpAffine(base, m, (w, h), flags=cv2.INTER_LANCZOS4,
                           borderMode=cv2.BORDER_REFLECT_101)
        p = os.path.join(d, f"rot_{i:04d}.png")
        cv2.imwrite(p, f)
        files.append(p)
    return files


def corners_inside(mats, crop, w, h):
    """裁切框四角逆变换回源图后是否都在画面内。"""
    x1, y1, x2, y2 = crop
    rect = np.array([[x1, y1], [x2, y1], [x1, y2], [x2, y2]], np.float64)
    for v in mats:
        inv = align._inverse(align.as_matrix(v))
        if inv.shape == (3, 3):
            ph = np.hstack([rect, np.ones((4, 1))]) @ inv.T
            q = ph[:, :2] / ph[:, 2:3]
        else:
            q = rect @ inv[:, :2].T + inv[:, 2]
        if (q < -0.5).any() or (q[:, 0] > w + 0.5).any() \
                or (q[:, 1] > h + 0.5).any():
            return False
    return True


def residual_ecc(ref_gray, path):
    """估计输出相对基准图还剩多少欧氏变换，返回 (角度°, 平移px, 相关系数)。"""
    img = cv2.imread(path)
    g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY).astype(np.float32) / 255.0
    m = np.eye(2, 3, dtype=np.float32)
    cc, wm = cv2.findTransformECC(
        ref_gray, g, m, cv2.MOTION_EUCLIDEAN,
        (cv2.TERM_CRITERIA_EPS | cv2.TERM_CRITERIA_COUNT, 200, 1e-5), None, 5)
    ang = math.degrees(math.atan2(float(wm[1, 0]), float(wm[0, 0])))
    tr = float(np.hypot(wm[0, 2], wm[1, 2]))
    return ang, tr, float(cc)


def test_measure():
    # 相机下坠时画面往上走，所以 dy 取负；画面往下走的方向由
    # test_downward 单独守（模板底部留了护带才测得出来）。
    drift = [(0.0, 0.0)]
    for i in range(1, 14):
        drift.append((drift[-1][0] + 0.5, drift[-1][1] - 0.3))
    drift.append((drift[-1][0] + 12.0, drift[-1][1] - 8.0))   # 相机被碰了一下
    files = write_shifted(make_base(), drift, TMP)

    offs, scores, issues, dt = align.measure_offsets(files, 0, est="template")
    check("估计无异常帧", not issues, str(issues))
    err = max(abs(offs[i][0] + drift[i][0]) for i in range(len(drift)))
    err = max(err, max(abs(offs[i][1] + drift[i][1]) for i in range(len(drift))))
    check("逐帧位移误差 < 0.5px", err < 0.5, f"最大 {err:.3f}px  用时 {dt:.1f}s")
    check("置信度全部在线", min(scores.values()) > 0.6,
          f"最低 {min(scores.values()):.3f}")

    sm, kx, ky = align.smooth_offsets(offs, scores, 3, 1.0, 0.6)
    check("真实抖动被保留", kx == 1 and ky == 1, f"dx {kx} / dy {ky}")
    jump = max(abs(sm[i][0] - sm[i - 1][0]) for i in range(1, len(drift)))
    check("跳变帧位移仍在", jump > 8, f"最大相邻差 {jump:.2f}px")
    # 抹掉抖动的后果：裁切框只会缩 14px，而不是把抖动摊平
    # 位移符号约定：要施加的偏移 = -真实画面位移。本序列画面往左上漂，
    # 所以偏移是 (<=0, >=0) → 右边和上边被吃掉，左/下保持到边。
    x1, y1, x2, y2 = align.common_crop(sm, 1000, 900)
    check("裁切随位移收缩", x1 == 0 and 1000 - x2 >= 15 and y1 >= 10 and y2 == 900,
          f"({x1},{y1})-({x2},{y2})")

    _, p2o, _ = align.smooth_offsets(
        {i: (-d[0], -d[1]) for i, d in enumerate(drift)},
        {i: 0.2 for i in range(len(drift))}, 3, 1.0, 0.6)
    check("低置信度的离群按误匹配平滑掉", p2o == 0, f"保留 {p2o} 帧")


def test_downward():
    """画面往下走（模板朝画面底边移动）：模板底部护带留出的观测余量要够用。

    模板贴底时这个方向会被搜索窗夹住，所有帧的 dy 都测成 0，统一裁切也
    跟着少裁一条——翻到成片里就是持续的下沉。
    """
    drift = [(0.0, 0.0), (0.5, 0.9), (1.0, 1.8), (-0.5, 2.7), (0.0, 3.5)]
    files = write_shifted(make_base(), drift, os.path.join(TMP, "down"))
    offs, scores, issues, _ = align.measure_offsets(files, 0, est="template")
    check("向下位移无异常帧", not issues, str(issues))
    err = max(max(abs(offs[i][0] + t[0]), abs(offs[i][1] + t[1]))
              for i, t in enumerate(drift))
    check("向下位移误差 < 0.5px", err < 0.5, f"最大 {err:.3f}px")
    check("向下位移置信度在线", min(scores.values()) > 0.6,
          f"最低 {min(scores.values()):.3f}")


def test_phase():
    """相位相关估计器：位移是反号约定，测出来也要能对上。"""
    truth = [(0.0, 0.0)] + [(i * 0.9, -i * 0.4) for i in range(1, 6)]
    files = write_shifted(make_base(), truth, os.path.join(TMP, "phase"))
    offs, scores, issues, _ = align.measure_offsets(
        files, 0, est="phase", roi=(0.0, 0.6, 1.0, 1.0))
    check("相位相关无异常帧", not issues, str(issues))
    err = max(max(abs(offs[i][0] + t[0]), abs(offs[i][1] + t[1]))
              for i, t in enumerate(truth))
    check("相位相关误差 < 0.3px", err < 0.3, f"最大 {err:.3f}px")


def test_ecc():
    """旋转序列：ECC 补偿矩阵要抵消旋转，残余明显小于模板匹配。"""
    w, h = 700, 600
    base = make_base(w, h, seed=11)
    angles = [0.1 * i for i in range(8)]
    files = write_rotated(base, angles, os.path.join(TMP, "rot"))
    mats = {}
    offs, scores, issues, dt = align.measure_offsets(
        files, 0, est="ecc", motion="euclid", mats=mats)
    check("ECC 无异常帧", not issues, str(issues))
    check("ECC 置信度在线", min(scores.values()) > 0.9,
          f"最低 {min(scores.values()):.3f}")
    err = max(abs(align.rotation_deg(mats[i]) - angles[i])
              for i in range(len(angles)))
    check("ECC 旋转角误差 < 0.03°", err < 0.03, f"最大 {err:.4f}°  {dt:.1f}s")

    crop = align.common_crop(mats, w, h)
    check("ECC 裁切四角都在画面内",
          corners_inside([mats[i] for i in range(len(files))], crop, w, h),
          f"{crop}")
    out, failed = align.apply_offsets(
        files, os.path.join(TMP, "rot_out"), offs, crop, (h, w), mats=mats)
    check("ECC 对齐输出全部写出", not failed and len(out) == len(files),
          str(failed))

    x1, y1, x2, y2 = crop
    bg = cv2.cvtColor(base[y1:y2, x1:x2], cv2.COLOR_BGR2GRAY) \
        .astype(np.float32) / 255.0
    angs, trs = [], []
    for idx in (3, len(files) - 1):
        a, t, cc = residual_ecc(bg, out[idx])
        angs.append(abs(a))
        trs.append(t)
    check("ECC 残余旋转 < 0.02°", max(angs) < 0.02, f"最大 {max(angs):.4f}°")
    check("ECC 残余平移 < 0.5px", max(trs) < 0.5, f"最大 {max(trs):.3f}px")

    sm, _kx, _ky = align.smooth_offsets(offs, scores, 3, 1.0, 0.6)
    tr_mats = align.retranslate(mats, sm)
    derr = max(abs(align.rotation_deg(tr_mats[i])
                   - align.rotation_deg(mats[i])) for i in range(len(files)))
    check("平滑只改平移不改旋转", derr < 1e-6, f"最大差 {derr:.2e}")

    offs_t, _st, _it, _ = align.measure_offsets(files, 0, est="template")
    crop_t = align.common_crop(offs_t, w, h)
    out_t, _ = align.apply_offsets(
        files, os.path.join(TMP, "rot_out_t"), offs_t, crop_t, (h, w))
    xt1, yt1, xt2, yt2 = crop_t
    bgt = cv2.cvtColor(base[yt1:yt2, xt1:xt2], cv2.COLOR_BGR2GRAY) \
        .astype(np.float32) / 255.0
    ang_t, _tr_t, _cc = residual_ecc(bgt, out_t[-1])
    check("ECC 残余旋转显著小于模板匹配", max(angs) * 5 < abs(ang_t),
          f"ECC {max(angs):.4f}° vs 模板 {abs(ang_t):.3f}°")


def test_feature():
    """特征点估计器：平移+旋转+亮度渐变都抓得回来，矩阵与 ECC 同约定。

    warpAffine 默认语义下传进去的矩阵就是 ref→cur 前向映射，所以期望的
    施加矩阵 = 它的逆；逐帧整矩阵对比，平移与旋转一起验（带旋转时平移列
    里含绕中心的补偿项，只比平移列和位移是不对的）。
    """
    w, h = 1600, 1200
    base = make_base(w, h, seed=21)
    shifts = [(0.0, 0.0), (9.0, -4.0), (18.0, -8.0), (27.0, -12.0)]
    angles = [0.0, 0.8, 1.6, 2.4]
    d = os.path.join(TMP, "feat")
    os.makedirs(d, exist_ok=True)
    files, truth = [], []
    for i, ((sx, sy), ang) in enumerate(zip(shifts, angles)):
        m = cv2.getRotationMatrix2D((w / 2.0, h / 2.0), ang, 1.0)
        m[0, 2] += sx
        m[1, 2] += sy
        truth.append(m.copy())
        f = cv2.warpAffine(base, m, (w, h), flags=cv2.INTER_LANCZOS4,
                           borderMode=cv2.BORDER_REFLECT_101)
        f = np.clip(f.astype(np.float32) * (1.0 + 0.06 * i), 0, 255) \
            .astype(np.uint8)                    # 模拟日落渐变的亮度漂移
        p = os.path.join(d, f"feat_{i:04d}.png")
        cv2.imwrite(p, f)
        files.append(p)
    mats = {}
    offs, scores, issues, _ = align.measure_offsets(
        files, 0, est="feature", mats=mats)
    check("特征点无异常帧", not issues, str(issues))
    check("特征点矩阵 2×3",
          all(np.asarray(mats[i]).shape == (2, 3) for i in range(len(files))))
    err_t = err_r = 0.0
    for i, m in enumerate(truth):
        want = align._inverse(m.astype(np.float64))   # 施加矩阵 = 前向映射的逆
        got = mats[i].astype(np.float64)
        err_t = max(err_t, float(np.hypot(*(got[:2, 2] - want[:2, 2]))))
        err_r = max(err_r, abs(align.rotation_deg(got)
                               - align.rotation_deg(want)))
    check("特征点矩阵平移误差 < 1px", err_t < 1.0, f"最大 {err_t:.3f}px")
    check("特征点旋转角误差 < 0.1°", err_r < 0.1, f"最大 {err_r:.4f}°")
    check("特征点置信度在线", min(scores.values()) > 0.5,
          f"最低 {min(scores.values()):.3f}")
    crop = align.common_crop(mats, w, h)
    check("特征点裁切四角都在画面内",
          corners_inside([mats[i] for i in range(len(files))], crop, w, h),
          f"{crop}")


def test_feature_guard():
    """防幻觉闸：平场帧不许编出位移，只有一小块纹理时也不许拿它当全局对齐。"""
    d = os.path.join(TMP, "feat_guard")
    os.makedirs(d, exist_ok=True)

    w, h = 800, 640
    ramp = np.tile(np.linspace(0, 255, w), (h, 1)).astype(np.uint8)
    files = []
    for i in range(3):
        p = os.path.join(d, f"flat_{i:04d}.png")
        cv2.imwrite(p, cv2.cvtColor(ramp, cv2.COLOR_GRAY2BGR))
        files.append(p)
    offs, _sc, issues, _ = align.measure_offsets(files, 0, est="feature")
    check("平场帧不编位移", all(offs[i] == (0.0, 0.0) for i in range(3)),
          f"issues {issues}")

    rng = np.random.default_rng(9)
    img = np.full((h, w, 3), 90, np.uint8)
    img[300:348, 380:428] = rng.integers(0, 255, (48, 48, 1), np.uint8)
    m1 = np.float32([[1, 0, 25], [0, 1, -14]])     # 那块纹理挪 (25, -14)px
    f1 = cv2.warpAffine(img, m1, (w, h), borderMode=cv2.BORDER_REPLICATE)
    files = []
    for i, f in enumerate((img, f1, f1)):
        p = os.path.join(d, f"patch_{i:04d}.png")
        cv2.imwrite(p, f)
        files.append(p)
    offs, _sc, issues, _ = align.measure_offsets(files, 0, est="feature")
    check("小块纹理的大位移判为异常",
          any(i == 1 for i, _ in issues) and offs[1] == (0.0, 0.0),
          f"issues {issues}  offs {list(offs.values())}")


def test_crop_translation():
    """纯平移的裁切结果必须与旧公式逐值一致（不能因矩阵化改变旧路径）。"""
    rng = np.random.default_rng(3)
    w, h = 800, 600
    shifts = [(float(rng.uniform(-40, 40)), float(rng.uniform(-30, 30)))
              for _ in range(50)]
    got = align.common_crop({i: s for i, s in enumerate(shifts)}, w, h)
    xs = [s[0] for s in shifts]
    ys = [s[1] for s in shifts]
    want = (max(0, math.ceil(max(xs))), max(0, math.ceil(max(ys))),
            min(w, math.floor(w + min(xs))), min(h, math.floor(h + min(ys))))
    check("平移裁切仍走旧公式", got == want, f"{got} vs {want}")


def test_ecc_homography():
    """单应估计：3×3 矩阵、裁切合法、输出能写出。"""
    w, h = 480, 360
    base = make_base(w, h, seed=5)
    d = os.path.join(TMP, "persp")
    os.makedirs(d, exist_ok=True)
    src = np.float32([[0, 0], [w, 0], [w, h], [0, h]])
    files = []
    for i in range(3):
        k = 0.01 * i
        dst = np.float32([[0, 0], [w * (1 - k), 0], [w, h * (1 - k)], [0, h]])
        m = cv2.getPerspectiveTransform(src, dst)
        f = cv2.warpPerspective(base, m, (w, h), flags=cv2.INTER_LANCZOS4,
                                borderMode=cv2.BORDER_REFLECT_101)
        p = os.path.join(d, f"persp_{i:04d}.png")
        cv2.imwrite(p, f)
        files.append(p)
    mats = {}
    offs, scores, issues, _ = align.measure_offsets(
        files, 0, est="ecc", motion="homography", mats=mats)
    check("单应无异常帧", not issues, str(issues))
    check("单应矩阵 3×3",
          all(np.asarray(mats[i]).shape == (3, 3) for i in range(len(files))))
    crop = align.common_crop(mats, w, h)
    check("单应裁切四角都在画面内",
          corners_inside([mats[i] for i in range(len(files))], crop, w, h),
          f"{crop}")
    out, failed = align.apply_offsets(
        files, os.path.join(TMP, "persp_out"), offs, crop, (h, w), mats=mats)
    check("单应输出全部写出", not failed and len(out) == len(files), str(failed))
    v = align.verify_residuals(out, roi=(0.0, 0.6, 1.0, 1.0))
    mx = max(float(np.hypot(*s)) for _, s, _ in v if s is not None)
    check("单应对齐后残余平移 < 0.5px", mx < 0.5, f"最大 {mx:.3f}px")


def test_apply():
    truth = [(0.0, 0.0), (3.0, 2.0), (6.0, 4.0)]
    files = write_shifted(make_base(), truth, os.path.join(TMP, "apply"), "jpg")
    offs = {i: (-t[0], -t[1]) for i, t in enumerate(truth)}
    out, failed = align.apply_offsets(
        files, os.path.join(TMP, "out"), offs,
        (6, 4, 994, 896), (900, 1000), out_ext="jpg")
    check("对齐输出全部写出", not failed and len(out) == 3, str(failed))
    im = cv2.imread(out[2])
    check("输出尺寸 = 裁切尺寸", im is not None and im.shape[:2] == (892, 988),
          "" if im is None else f"{im.shape[1]}x{im.shape[0]}")
    v = align.verify_residuals(out, roi=(0.0, 0.6, 1.0, 1.0))
    mx = max(float(np.hypot(*s)) for _, s, _ in v if s is not None)
    check("对齐后残余 < 0.5px", mx < 0.5, f"最大 {mx:.3f}px")


def main():
    if not os.path.isdir(TMP):
        os.makedirs(TMP, exist_ok=True)
    test_measure()
    test_downward()
    test_phase()
    test_ecc()
    test_feature()
    test_feature_guard()
    test_crop_translation()
    test_ecc_homography()
    test_apply()
    print(f"\n{TOTAL[0] - len(FAIL)}/{TOTAL[0]} 通过"
          + (f"  失败: {FAIL}" if FAIL else ""))
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
