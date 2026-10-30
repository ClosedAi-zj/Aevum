"""训练 lumen 的 AI 降噪小模型（纯 numpy，无外部权重）。

为什么是「自己训」：clean-room 下不能拿别人的模型权重，也不引入 PyTorch/ONNX
这类新依赖；这里用 numpy 手写 4 层 3×3 卷积的前向与反向，训练完把权重导成
Rust 的 const 数组，Rust 侧只做推理。

两个网络分开训，别合成一个：实测三通道合训时亮度梯度把主干占满，色度输出
始终停在恒等（5000 轮后色度 PSNR 仍是 0.00 dB）。两个 1/2 通道的小网各训各的，
参数少、收敛快，推理时按块先后跑，中间层缓冲复用。

- `--net luma`：1→C→C→C→1，输入 Rec.601 亮度，输出增量（`out = y + delta`）。
  权重写到 `crates/lumen-core/src/denoise_weights.rs`。
- `--net chroma`：2→C→C→C→2，输入 Cb = B − Y、Cr = R − Y，输出两路增量。
  权重写到 `crates/lumen-core/src/denoise_chroma_weights.rs`。

数据：本机真实延时帧（默认 `D:/work/project/pycharm/p/pythonProject4/aligned_lr3`），
随机裁 48×48；噪声按信号相关的高斯合成（σ = sqrt(a·y + b)），每样本随机取 a、b；
色度噪声幅度按亮度的一半（JPEG 源本身经过色度子采样，高频彩噪比亮度弱）。

用法：
    python tools/train_denoise.py --net chroma --iters 6000
    python tools/train_denoise.py --net luma --iters 6000
    python tools/train_denoise.py --net chroma --eval-only   # 只评估，不写权重

评估口径：留出 4 帧，σ 固定，比较「带噪 / 本模型 / fastNlMeansDenoising」。
"""

import argparse
import glob
import math
import os
import sys
import time

import numpy as np
from PIL import Image

C = 12            # 中间层通道数（推理成本与质量折中）
PATCH = 48        # 训练裁块边长
BATCH = 12

NETS = {
    'luma': (1, 1),
    'chroma': (2, 2),
}


def enable_ftz():
    """x86 反规格化浮点慢上百倍：训练后期激活值贴到 1e-38 附近时，单轮耗时
    会从 45ms 掉到 1s。打开 FTZ/DAZ 让这类数直接当 0（对 8 位输出无影响）。"""
    if sys.platform != 'win32':
        return
    import ctypes

    try:
        ctypes.CDLL('msvcrt')._controlfp(0x0300_0000, 0x0300_0000)
    except Exception:
        pass


def luma(rgb: np.ndarray) -> np.ndarray:
    return 0.299 * rgb[..., 0] + 0.587 * rgb[..., 1] + 0.114 * rgb[..., 2]


def feat(rgb: np.ndarray, mode: str) -> np.ndarray:
    """RGB → 网络输入。chroma 的 Cb/Cr 是有符号色差，灰上恒为 0。"""
    y = luma(rgb)
    if mode == 'luma':
        return y[..., None]
    return np.stack([rgb[..., 2] - y, rgb[..., 0] - y], axis=-1)


def im2col(x: np.ndarray) -> np.ndarray:
    """NHWC → (N*H*W, C*9)，3×3、步长 1、边界补零。"""
    n, h, w, c = x.shape
    p = np.pad(x, ((0, 0), (1, 1), (1, 1), (0, 0)))
    cols = np.empty((n * h * w, c * 9), dtype=np.float32)
    k = 0
    for ky in range(3):
        for kx in range(3):
            cols[:, k * c:(k + 1) * c] = p[:, ky:ky + h, kx:kx + w, :].reshape(-1, c)
            k += 1
    return cols


def col2im(dcols: np.ndarray, n: int, h: int, w: int, c: int) -> np.ndarray:
    p = np.zeros((n, h + 2, w + 2, c), dtype=np.float32)
    k = 0
    for ky in range(3):
        for kx in range(3):
            p[:, ky:ky + h, kx:kx + w, :] += dcols[:, k * c:(k + 1) * c].reshape(n, h, w, c)
            k += 1
    return p[:, 1:-1, 1:-1, :]


def he(rng, cout, cin):
    return (rng.standard_normal((cout, cin * 9)) * math.sqrt(2.0 / (cin * 9))).astype(np.float32)


class Net:
    def __init__(self, cin, cout, seed=7):
        rng = np.random.default_rng(seed)
        self.cin = cin
        self.cout = cout
        self.w = [he(rng, C, cin), he(rng, C, C), he(rng, C, C), he(rng, cout, C)]
        self.b = [np.zeros(C, np.float32), np.zeros(C, np.float32),
                  np.zeros(C, np.float32), np.zeros(cout, np.float32)]

    def forward(self, x):
        """x: (N,H,W,cin) → (out, cache)。out 是 (N*H*W,cout) 的残差。"""
        acts = [x]
        masks = []
        z = x
        for i in range(4):
            cols = im2col(z)
            z = cols @ self.w[i].T + self.b[i]
            if i < 3:
                masks.append(z > 0)
                z = np.maximum(z, 0.0)
                z = z.reshape(x.shape[0], x.shape[1], x.shape[2], C)
            acts.append(z)
        y_last = x.reshape(-1, self.cout)
        out = y_last + z
        return out, acts, masks

    def backward(self, dout, acts, masks, h, w):
        n = acts[0].shape[0]
        dw = [None] * 4
        db = [None] * 4
        dz = dout
        for i in (3, 2, 1, 0):
            dw[i] = dz.T @ im2col(acts[i])
            db[i] = dz.sum(axis=0)
            dcols = dz @ self.w[i]
            dprev = col2im(dcols, n, h, w, acts[i].shape[3])
            if i > 0:
                dz = dprev.reshape(-1, C) * masks[i - 1]
        return dw, db


def load_frames(root, count, edge_skip=64, side=1024):
    files = sorted(glob.glob(os.path.join(root, '*.jpg')) +
                   glob.glob(os.path.join(root, '*.jpeg')) +
                   glob.glob(os.path.join(root, '*.png')))
    if not files:
        sys.exit(f'没有找到训练图像：{root}')
    idx = np.linspace(0, len(files) - 1, count).round().astype(int)
    out = []
    for i in idx:
        im = Image.open(files[i]).convert('RGB')
        rgb = np.asarray(im, dtype=np.float32) / 255.0
        if edge_skip:
            rgb = rgb[edge_skip:-edge_skip, edge_skip:-edge_skip]
        # 24MP × 32 张会吃掉十 GB 内存；训练只要 48×48 的块，留一块 1024² 的真实细节就够。
        if side and (rgb.shape[0] > side or rgb.shape[1] > side):
            y0 = (rgb.shape[0] - side) // 2
            x0 = (rgb.shape[1] - side) // 2
            # 必须拷贝：切片是视图，留着视图就等于把整张 24MP 的基数组钉在内存里
            # （52 张 ≈ 15GB，直接换页到磁盘）。
            rgb = np.ascontiguousarray(rgb[y0:y0 + side, x0:x0 + side])
        out.append(rgb)
    return out


def noise_sigma(y, rng):
    """信号相关噪声：亮度按 σ = sqrt(a·y + b)，色度幅度减半。"""
    a = 10 ** rng.uniform(-5.0, -3.0)    # 信号相关项
    b = 10 ** rng.uniform(-5.0, -2.6)    # 读出噪声项
    return np.sqrt(a * np.clip(y, 0.0, 1.0) + b)


def crops(frames, n, rng, mode):
    """返回 (净特征, 对应亮度)：噪声强度从亮度算，色度模式也要它。"""
    ys, fs = [], []
    for _ in range(n):
        f = frames[rng.integers(len(frames))]
        y = int(rng.integers(0, f.shape[0] - PATCH))
        x = int(rng.integers(0, f.shape[1] - PATCH))
        p = f[y:y + PATCH, x:x + PATCH]
        if rng.random() < 0.5:
            p = p[::-1]
        if rng.random() < 0.5:
            p = p[:, ::-1]
        ys.append(np.ascontiguousarray(luma(p)))
        fs.append(np.ascontiguousarray(feat(p, mode)))
    return np.stack(fs), np.stack(ys)


def noisy_pair(clean, y, rng, mode):
    sig = noise_sigma(y, rng)
    if mode == 'chroma':
        sig = sig * 0.5
    return clean + rng.standard_normal(clean.shape) * sig[..., None]


def train(frames, iters, mode, lr=1.5e-3, tag=''):
    rng = np.random.default_rng(3)
    cin, cout = NETS[mode]
    net = Net(cin, cout)
    m = [np.zeros_like(t) for t in net.w + net.b]
    v = [np.zeros_like(t) for t in net.w + net.b]
    step = 0
    t0 = time.time()
    for it in range(1, iters + 1):
        clean, y = crops(frames, BATCH, rng, mode)
        noisy = noisy_pair(clean, y, rng, mode)
        out, acts, masks = net.forward(noisy.astype(np.float32))
        target = clean.reshape(-1, cout)
        dout = (out - target) * (2.0 / out.size)
        dw, db = net.backward(dout, acts, masks, PATCH, PATCH)
        step += 1
        allw = net.w + net.b
        dall = dw + db
        for i, g in enumerate(dall):
            m[i] = 0.9 * m[i] + 0.1 * g
            v[i] = 0.999 * v[i] + 0.001 * g * g
            mh = m[i] / (1 - 0.9 ** step)
            vh = v[i] / (1 - 0.999 ** step)
            allw[i] -= (lr * mh / (np.sqrt(vh) + 1e-8)).astype(np.float32)
        if it % 250 == 0 or it == 1:
            psnr = 10 * math.log10(1.0 / max(float(np.mean((out - target) ** 2)), 1e-12))
            print(f'{tag} iter {it:5d}  loss-PSNR {psnr:5.2f}dB  '
                  f'{time.time() - t0:6.1f}s', flush=True)
    return net


def psnr(a, b):
    return 10 * math.log10(1.0 / max(float(np.mean((a - b) ** 2)), 1e-12))


def infer(net, x):
    out, _, _ = net.forward(x.astype(np.float32))
    return out.reshape(x.shape)


def evaluate(net, frames, mode):
    import cv2
    rng = np.random.default_rng(11)
    side = 512
    for sigma in (0.01, 0.02, 0.04):
        rows = []
        for f in frames:
            y0 = (f.shape[0] - side) // 2
            x0 = (f.shape[1] - side) // 2
            clean = feat(f[y0:y0 + side, x0:x0 + side], mode)
            amp = sigma * 0.5 if mode == 'chroma' else sigma
            noisy = clean + rng.standard_normal(clean.shape) * amp
            den = infer(net, noisy[None])[0]
            if mode == 'luma':
                nlm = cv2.fastNlMeansDenoising(
                    (np.clip(noisy[..., 0], 0, 1) * 255).astype(np.uint8),
                    None, 3, 7, 21).astype(np.float32) / 255.0
                nlm = nlm[..., None]
            else:
                # NLM 只吃 8bit 非负值：两路色差各自 +0.5 再减回。
                nlm = np.stack([
                    cv2.fastNlMeansDenoising(
                        (np.clip(noisy[..., c] + 0.5, 0, 1) * 255).astype(np.uint8),
                        None, 3, 7, 21).astype(np.float32) / 255.0 - 0.5
                    for c in (0, 1)], axis=-1)
            rows.append((psnr(noisy, clean), psnr(den, clean), psnr(nlm, clean)))
        n, d, l = np.mean(rows, axis=0)
        print(f'σ={sigma:.2f}  带噪 {n:5.2f}dB  本模型 {d:5.2f}dB ({d - n:+.2f})  '
              f'NLM {l:5.2f}dB ({l - n:+.2f})')


def emit(net, path, mode):
    names = ['W1', 'W2', 'W3', 'W4']
    cin, cout = NETS[mode]
    dims = [(C, cin), (C, C), (C, C), (cout, C)]
    # 用 float32 的最短往返表示：写 7 位小数会让 clippy 的 excessive_precision 报错。
    fmt = lambda v: str(np.float32(v))
    title = ('亮度' if mode == 'luma' else '色度（Cb = B−Y、Cr = R−Y）')
    lines = ['//! 由 `tools/train_denoise.py` 生成，别手改。',
             '//!',
             f'//! 4 层 3×3 卷积（12 通道）的{title}残差降噪器权重：每层 `(Cout, Cin*9)` 行主序，',
             '//! 卷积核顺序为 ky 外层、kx 内层。生成口径见脚本头部注释。',
             '', f'pub const CHANNELS: usize = {C};', '']
    for i, (name, (cout_i, cin_i)) in enumerate(zip(names, dims)):
        flat = net.w[i].reshape(-1)
        body = ', '.join(fmt(v) for v in flat)
        lines.append(f'pub const {name}: [f32; {flat.size}] = [{body}];')
        lines.append('')
    for i in range(4):
        flat = net.b[i].reshape(-1)
        body = ', '.join(fmt(v) for v in flat)
        lines.append(f'pub const B{i + 1}: [f32; {flat.size}] = [{body}];')
        lines.append('')
    with open(path, 'w', encoding='utf-8', newline='\n') as f:
        f.write('\n'.join(lines))
    print(f'写出 {path}（{sum(net.w[i].size + net.b[i].size for i in range(4))} 个参数）')


def main():
    enable_ftz()
    ap = argparse.ArgumentParser()
    ap.add_argument('--net', choices=sorted(NETS), default='luma')
    ap.add_argument('--root', default=r'D:/work/project/pycharm/p/pythonProject4/aligned_lr3')
    ap.add_argument('--frames', type=int, default=32)
    ap.add_argument('--holdout', type=int, default=4)
    ap.add_argument('--iters', type=int, default=6000)
    ap.add_argument('--out', default=None)
    ap.add_argument('--eval-only', action='store_true')
    args = ap.parse_args()
    out = args.out or (f'crates/lumen-core/src/denoise_{args.net}_weights.rs'
                       if args.net == 'chroma' else 'crates/lumen-core/src/denoise_weights.rs')
    frames = load_frames(args.root, args.frames + args.holdout)
    holdout, train_frames = frames[:args.holdout], frames[args.holdout:]
    print(f'网络 {args.net}，训练帧 {len(train_frames)} 张，留出 {len(holdout)} 张，'
          f'裁块 {PATCH}×{PATCH}，通道 {C}')
    net = train(train_frames, args.iters, args.net)
    evaluate(net, holdout, args.net)
    if not args.eval_only:
        emit(net, out, args.net)


if __name__ == '__main__':
    main()
