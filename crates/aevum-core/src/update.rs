//! 内置更新器的**纯逻辑**部分：解析发布信息、比版本、挑资产、校验和、生成替换脚本。
//!
//! 为什么单独一个模块：更新这件事的风险不在「下载」上，而在「判断」上——版本号比错了会
//! 降级，资产挑错了会下到别的平台的包，校验值对不上却放行则等于没有校验。这些判断都是
//! 纯函数，可以在没有网络、没有 exe、没有证书的机器上逐条跑通。真正的网络与文件替换
//! 留在最边上（[`fetch`] / [`install_script`]），并且都能被测试替换或只断言文本。
//!
//! 为什么零新依赖：分发链是最后一个还没被依赖审计覆盖的角落，为了「检查更新」引一个
//! HTTP 客户端（连带 TLS 栈）不划算，且会让离线构建多一份要审的代码。Windows 10 1803+
//! 自带 `curl.exe`，找不到就退到 PowerShell 的 `Invoke-WebRequest`——两者都是系统组件。
//! 两条通道都没有时**明确报错**，不静默失败：用户需要知道「这台机器装不了更新」。
//!
//! 为什么替换用一段 CMD 而不是 `std::fs::rename`：Windows 不允许覆盖正在执行的可执行
//! 文件，而更新总是由「正在执行的那个 exe」发起的。唯一稳的顺序是「先退出，再由另一个
//! 进程搬文件」，那个「另一个进程」就是这段脚本。它只生成文本，由调用方决定何时启动。
//!
//! The **pure logic** of the built-in updater: parse release metadata, compare versions,
//! pick an asset, verify checksums, and generate the replacement script.
//!
//! Why a separate module: the risk in updating is not downloading but *deciding* -- a wrong
//! version comparison downgrades users, a wrong asset pick downloads another platform, and a
//! checksum that is not enforced is no checksum at all. All of that is pure and can be tested
//! on a machine with no network, no exe and no certificate. Real I/O stays at the edges
//! ([`fetch`] / [`install_script`]) and every edge is either injectable or asserted as text.
//!
//! Why zero new dependencies: the distribution chain is the last corner not covered by the
//! dependency audit, and pulling an HTTP client (plus a TLS stack) just to check for updates
//! is not worth it; it would also add code to review for offline builds. Windows 10 1803+
//! ships `curl.exe`, and PowerShell's `Invoke-WebRequest` is the fallback -- both are OS
//! components. When neither exists we **fail loudly** instead of silently: the user needs to
//! know that this machine cannot install updates.
//!
//! Why replacement is a CMD script instead of `std::fs::rename`: Windows refuses to overwrite
//! a running executable, and an update is always started by the executable that is running.
//! The only reliable order is "exit first, then let another process move the file", and that
//! other process is this script. We only generate the text; the caller decides when to run it.

use std::path::{Path, PathBuf};

/// GUI 进程里 spawn 的 curl/powershell/tarp 都是 console 程序：默认会闪一个
/// 黑色控制台窗（用户实测「cmd 控制台弹出」）。CREATE_NO_WINDOW (0x0800_0000)
/// 压掉它——只对 Windows 生效，其余平台原样。
///
/// curl/powershell/tar spawned from a GUI process are console programs: they flash
/// a black console window by default (the user-reported "cmd console popup").
/// CREATE_NO_WINDOW (0x0800_0000) suppresses it -- Windows-only, a no-op elsewhere.
pub trait NoConsoleWindow {
    fn no_console_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl NoConsoleWindow for std::process::Command {
    fn no_console_window(&mut self) -> &mut Self {
        // 0x0800_0000 = CREATE_NO_WINDOW
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000)
    }
}

#[cfg(not(windows))]
impl NoConsoleWindow for std::process::Command {
    fn no_console_window(&mut self) -> &mut Self {
        self
    }
}

use std::process::Command;

use serde::Deserialize;

/// 一个可下载的发布资产。
///
/// One downloadable release asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    /// 文件名，与 `SHA256SUMS` 里那一行必须逐字一致。
    ///
    /// File name; must match the corresponding line in `SHA256SUMS` exactly.
    pub name: String,
    /// 直链（GitHub 上是 `browser_download_url`）。
    ///
    /// Direct URL (`browser_download_url` on GitHub).
    pub url: String,
    /// 字节数，只用于界面显示与磁盘空间预检，不参与任何安全判断。
    ///
    /// Size in bytes; used for display and disk-space pre-checks only, never for security.
    pub size: u64,
}

/// 一次发布。
///
/// One release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// 形如 `v1.9.0` 的标签，必须能解析成三段版本号。
    ///
    /// Tag such as `v1.9.0`; it must parse as a three-part version.
    pub tag: String,
    /// 发布标题（可能为空）。
    ///
    /// Release title (may be empty).
    pub name: String,
    /// 发布说明正文，由 UI 层轻量渲染；未识别语法按纯文本逐字保留。
    ///
    /// The release notes body, lightly rendered in the UI; unrecognised syntax is kept
    /// verbatim as plain text.
    pub body: String,
    /// 资产列表，解析时保证非空。
    ///
    /// Asset list, guaranteed non-empty after parsing.
    pub assets: Vec<ReleaseAsset>,
    /// 是否为预发布。预发布**不主动提示**，用户要装得自己勾选。
    ///
    /// Whether this is a pre-release. Pre-releases are **not** offered by default; the user
    /// has to opt in.
    pub prerelease: bool,
}

/// 「查到了什么、能不能装、为什么不能装」的一次性结论，直接喂给界面。
///
/// 为什么不返回 `Result`：检查更新失败（断网、限流、JSON 变了）不是异常，是常态。
/// 界面需要的是一行能显示的话，而不是一个必须 `match` 的错误类型——否则每个调用点都
/// 要自己编一句中文，最后四处的措辞还不一样。
///
/// The one-shot conclusion for the UI: what was found, whether it can be installed, and why
/// not.
///
/// Why this is not a `Result`: a failed update check (offline, rate limited, changed JSON) is
/// the normal case, not an exception. The UI needs one displayable line, not an error type it
/// must `match` -- otherwise every call site invents its own wording and the four copies drift.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateCheck {
    /// 当前版本（`CARGO_PKG_VERSION`）。
    ///
    /// The current version (`CARGO_PKG_VERSION`).
    pub current: String,
    /// 远端版本；连不上或解析失败时为 `None`。
    ///
    /// The remote version, or `None` when the check failed.
    pub latest: Option<String>,
    /// 要下载的资产；没有匹配项（或不需要更新）时为 `None`。
    ///
    /// The asset to download; `None` when nothing matched or no update is needed.
    pub asset: Option<ReleaseAsset>,
    /// 一句话说明，**永远非空**，界面直接显示，不用再拼。
    ///
    /// One-line explanation, **always non-empty**, displayed as is.
    pub note: String,
}

impl UpdateCheck {
    /// 有没有可装的新版本（版本更新 + 找到资产，两者都满足）。
    ///
    /// Whether there is something to install (a newer version *and* a matched asset).
    pub fn installable(&self) -> bool {
        self.asset.is_some()
    }
}

/// 可注入的下载通道。测试用它跑完整流程而**不碰网络**。
///
/// Injectable download channel. Tests use it to run the whole flow **without touching the
/// network**.
pub type Fetcher = dyn Fn(&str) -> Result<Vec<u8>, String>;

/// 本机可执行文件查找器（产 [`fetch_with`] 的测试缝）。
///
/// Executable lookup on this machine (the test seam for [`fetch_with`]).
pub type Locator = dyn Fn(&str) -> Option<PathBuf>;

/// 当前版本。真源在 [`crate::brand::version`]，这里只是给更新链一个贴合语义的别名。
///
/// 整个工作区只有 `brand.rs` 读那个编译期版本宏（判据 `brand::only_the_brand_module_…`
/// 盯着）：更新链、命令行、MCP 三处报的必须是同一个数，分散在各处的宏调用迟早会漏改一个。
///
/// The current version. The single source is [`crate::brand::version`]; this is just a
/// semantically apt alias for the update chain.
///
/// Across the whole workspace only `brand.rs` reads that compile-time version macro (the
/// `brand::only_the_brand_module_…` test watches it): the update chain, the CLI and MCP must
/// all report the same number, and scattered macro calls will eventually leave one stale.
pub fn current_version() -> &'static str {
    crate::brand::version()
}

/// 查「最新发布」的地址。写在这里而不是散在界面层：换仓库、换镜像只需要改一处。
///
/// The endpoint for "latest release". Kept here rather than in the UI: moving the repo or
/// pointing at a mirror should be a one-line change.
pub const API_LATEST: &str = "https://api.github.com/repos/ClosedAi-zj/Aevum/releases/latest";

/// 与 [`pick_asset`] 配套的资产后缀，和 `scripts/package.sh` 打出来的名字一致。
///
/// The asset suffix that goes with [`pick_asset`], matching the name `scripts/package.sh`
/// produces.
pub const ASSET_SUFFIX: &str = "windows-x64.zip";

/// SHA256SUMS 的文件名（发布资产之一）。
///
/// The name of the `SHA256SUMS` release asset.
pub const SUMS_ASSET: &str = "SHA256SUMS";

/// 实际使用的检查地址：环境变量 `AEVUM_UPDATE_API` 可以整体替换（离线自建镜像、内网
/// 分发、以及测试）。
///
/// 空值与纯空白一律当成没设：设成空字符串就悄悄指到空白页，比直接报错更难查。
///
/// The endpoint actually used: `AEVUM_UPDATE_API` replaces it wholesale (self-hosted
/// mirrors, intranet distribution, and tests).
///
/// Empty or whitespace-only counts as unset: silently pointing at a blank page is harder to
/// diagnose than an outright failure.
pub fn latest_url() -> String {
    let key = format!("{}UPDATE_API", crate::brand::ENV_PREFIX);
    match std::env::var(&key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => API_LATEST.to_string(),
    }
}

/// 解析 GitHub `/releases/latest` 的 JSON。
///
/// 三层校验，每层都返回**能读懂**的错误而不是 serde 的原始报错：
/// 1. JSON 本身不合语法 → 说「不是合法 JSON」，并带上 serde 的第一句；
/// 2. 关键字段缺失 → 点名是哪个字段（`tag_name` / `assets` / 资产里的 `name` 等）；
/// 3. `tag_name` 不是 `vX.Y.Z` → 说清「标签不是三段版本号」，因为拿它比版本会静默走偏。
///
/// 为什么全用 `Option` 再手工判空：`serde` 的 `missing field` 报错说的是 JSON 字段名，
/// 用户和日志看的是「这个版本没有 tag_name」；后者能直接指向发布流程的 bug。
///
/// Parse the JSON of GitHub's `/releases/latest`.
///
/// Three validation layers, each returning a **readable** error instead of raw serde output:
/// 1. malformed JSON → say so, with serde's first sentence;
/// 2. missing key fields → name the field (`tag_name` / `assets` / an asset's `name`, ...);
/// 3. `tag_name` that is not `vX.Y.Z` → say the tag is not a three-part version, because
///    comparing against it would silently go wrong.
///
/// Why every field is an `Option` and checked by hand: serde's `missing field` error speaks
/// JSON field names, while a user or a log wants "this release has no tag_name"; the latter
/// points straight at the release-pipeline bug.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let raw: RawRelease =
        serde_json::from_str(json).map_err(|e| format!("发布信息不是合法 JSON：{e}"))?;

    let tag = raw
        .tag_name
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "发布信息缺少 tag_name".to_string())?;
    if parse_version(&tag).is_none() {
        return Err(format!(
            "发布标签 {tag} 不是 vX.Y.Z 形式的三段版本号，无法与当前版本比较"
        ));
    }

    let assets = raw
        .assets
        .ok_or_else(|| format!("发布 {tag} 没有 assets 字段"))?;
    if assets.is_empty() {
        return Err(format!("发布 {tag} 的资产列表是空的，没有可下载的文件"));
    }

    let mut out = Vec::with_capacity(assets.len());
    for (i, a) in assets.into_iter().enumerate() {
        let name = a
            .name
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("发布 {tag} 的第 {} 个资产没有 name", i + 1))?;
        let url = a
            .browser_download_url
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("发布 {tag} 的资产 {name} 没有 browser_download_url"))?;
        out.push(ReleaseAsset {
            name,
            url,
            // 缺 size 不算错：老发布、手工上传的资产都可能没有。填 0，界面显示「大小未知」。
            // A missing size is not an error: old releases and hand-uploaded assets may lack
            // it. Record 0 and let the UI say "size unknown".
            size: a.size.unwrap_or(0),
        });
    }

    Ok(Release {
        tag,
        name: raw.name.unwrap_or_default(),
        body: raw.body.unwrap_or_default(),
        assets: out,
        // 缺 prerelease 字段按正式版处理，但解析到的 `true` 一律保留——宁可少提示一次，
        // 也不要把 beta 当正式版推给所有人。
        // A missing `prerelease` is treated as stable, but an explicit `true` is preserved:
        // better to under-notify once than to push a beta to everyone.
        prerelease: raw.prerelease.unwrap_or(false),
    })
}

/// `v1.8.0` / `1.8.0` → `(1, 8, 0)`；其余一律 `None`。
///
/// 严格三段：`v1.8` 与 `v1.8.0-beta` 都拒绝。理由是二者含义不同——前者是残缺，
/// 后者是预发布（由 [`Release::prerelease`] 单独判断）——用同一个函数混着返回，
/// 调用方就没法区分「这个标签坏了」和「这是个 beta」。
///
/// 每段必须**全是 ASCII 数字**：`u32::from_str` 会接受 `+8` 这种写法，直接用它会让
/// `v1.+8.0` 通过；先判字符集合再解析。
///
/// 比较用的是 `(u32, u32, u32)` 元组，逐段数值比较，因此 `1.10.0 > 1.9.0`。
/// 这正是字符串比较会错的经典坑：`"1.10.0" < "1.9.0"` 按字节成立，使用者会永远收不到
/// 1.10 之后的更新。
///
/// `v1.8.0` / `1.8.0` → `(1, 8, 0)`; anything else is `None`.
///
/// Strictly three parts: `v1.8` and `v1.8.0-beta` are both rejected. Their meanings differ --
/// the first is truncated, the second is a pre-release (judged by [`Release::prerelease`]
/// separately) -- and folding them into one return value would leave callers unable to tell
/// "broken tag" from "this is a beta".
///
/// Every part must be **all ASCII digits**: `u32::from_str` accepts `+8`, which would let
/// `v1.+8.0` through, so the character set is checked before parsing.
///
/// Comparison uses a `(u32, u32, u32)` tuple, so `1.10.0 > 1.9.0`. This is exactly the classic
/// trap of string comparison: `"1.10.0" < "1.9.0"` is true byte-wise, and users would never
/// see an update after 1.10.
pub fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let s = tag.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    let mut parts = s.split('.');
    let major = parse_part(parts.next()?)?;
    let minor = parse_part(parts.next()?)?;
    let patch = parse_part(parts.next()?)?;
    // 第四段存在（`1.8.0.1`）也拒绝：多一段说明发布流程用了别的版本口径。
    // A fourth part (`1.8.0.1`) is rejected too: it means the release pipeline uses a
    // different versioning convention.
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// 单段版本号：非空、全 ASCII 数字、能装进 `u32`。
///
/// One version component: non-empty, all ASCII digits, fits in `u32`.
fn parse_part(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// 候选版本是否比当前版本新。任一侧解析不了都返回 `false`（宁可不提示，也不误提示）。
///
/// Whether the candidate is newer. If either side fails to parse the answer is `false` --
/// better to stay quiet than to offer a bogus update.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(cur), Some(cand)) => cand > cur,
        _ => false,
    }
}

/// 按文件名后缀挑资产（大小写不敏感）。
///
/// 后缀规则来自本仓的打包脚本：`scripts/package.sh` 生成
/// `Aevum-Studio-v<版本>-windows-x64.zip`，所以调用方应传 `windows-x64.zip`。
/// 不按前缀匹配是因为版本号在名字中间，按后缀才是唯一稳定的那一段。
///
/// 多个命中时取**第一个**：发布流程保证同名后缀只有一个；如果真出现两个，那是发布侧
/// 的 bug，这里不做「猜哪个更好」的模糊判断。
///
/// Pick an asset by file-name suffix (case-insensitive).
///
/// The suffix rule comes from this repo's packaging script: `scripts/package.sh` produces
/// `Aevum-Studio-v<version>-windows-x64.zip`, so callers should pass `windows-x64.zip`.
/// Matching on the suffix rather than the prefix is deliberate: the version sits in the
/// middle of the name, and the suffix is the only stable part.
///
/// On multiple hits the **first** one wins: the release pipeline guarantees one asset per
/// suffix, and two would be a release-side bug rather than something to guess about here.
pub fn pick_asset<'a>(rel: &'a Release, suffix: &str) -> Option<&'a ReleaseAsset> {
    let want = suffix.to_ascii_lowercase();
    rel.assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase().ends_with(&want))
}

/// 字节的 SHA-256，小写十六进制。
///
/// 为什么自己实现而不是引 `sha2`：本 crate 的依赖面已经审过一轮，为了一个 120 行的
/// 定长哈希再引一个 crate（以及它的 30 个下游）不划算，而 SHA-256 的规范是冻结的。
/// 实现严格照 FIPS 180-4：64 轮、大端填充、长度按**位**计。
///
/// 判据里有官方测试向量（空串与 `abc`），改坏任何一处常量或移位都会立刻变红。
///
/// SHA-256 of `bytes`, lowercase hex.
///
/// Why implement it instead of depending on `sha2`: this crate's dependency surface has
/// already been audited once, and adding a crate (plus its 30 downstreams) for a 120-line
/// fixed-size hash is a bad trade; the SHA-256 spec is frozen. The implementation follows
/// FIPS 180-4 exactly: 64 rounds, big-endian padding, length counted in **bits**.
///
/// The tests carry the official vectors (empty input and `abc`), so breaking any constant or
/// rotation turns them red immediately.
pub fn sha256_of(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // 填充：先补 0x80，再补 0 到 56 (mod 64)，最后 8 字节大端位长。长度用 u64 且按位
    // 计算——按字节算会让所有非 8 倍数长度的输入错位。
    // Padding: 0x80, then zeros to 56 (mod 64), then the 8-byte big-endian bit length. The
    // length is a u64 in **bits**; using bytes would mis-place every input whose length is
    // not a multiple of 8.
    let mut msg = Vec::with_capacity(bytes.len() + 72);
    msg.extend_from_slice(bytes);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(bytes.len() as u64).wrapping_mul(8).to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            let b = [
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ];
            w[i] = u32::from_be_bytes(b);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            // 这里必须用补前的 t1：用 t2 或更新后的 e 都会让整条链错位。
            // t1 must come from the pre-update values; using t2 or the updated e mis-aligns
            // the whole chain.
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// 用 `SHA256SUMS` 文本校验一段字节。
///
/// 清单格式与 `scripts/package.sh` 写出的完全一致：`<64 位十六进制>  <文件名>`，
/// 也是 `sha256sum -c` 认的格式。行首的 `*`（二进制标记）会被剥掉。
///
/// **找不到条目 = 失败**，不是「没有校验值所以放行」。这是本函数存在的唯一理由：
/// 一份被截断或者被人替换过的清单，最容易的形态就是「里面没有这一行」。
/// 同理，期望值不是 64 位十六进制也直接失败，而不是拿它去比。
///
/// 比较大小写不敏感：不同工具输出的十六进制大小写不一致，这不是安全问题。
///
/// Verify a byte buffer against `SHA256SUMS` text.
///
/// The manifest format is exactly what `scripts/package.sh` writes -- `<64 hex>  <name>`,
/// the same format `sha256sum -c` accepts -- and a leading `*` (binary marker) is stripped.
///
/// **A missing entry is a failure**, not "there is no checksum so let it pass". That is the
/// whole point of this function: the easiest shape of a truncated or swapped manifest is
/// "this line is not in it". Likewise an expected value that is not 64 hex digits fails
/// outright instead of being compared.
///
/// Comparison is case-insensitive: tools disagree on hex case, and that is not a security
/// property.
pub fn verify(bytes: &[u8], sums_text: &str, asset_name: &str) -> Result<(), String> {
    let want_name = asset_name.trim();
    let mut found: Option<&str> = None;
    for line in sums_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // 文件名可能含空格，所以只按第一个空白切一次，剩下的整段都是名字。
        // File names may contain spaces, so split on the first whitespace only and keep the
        // rest as the name.
        let Some((hex, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = rest.trim().trim_start_matches('*');
        if name.eq_ignore_ascii_case(want_name) {
            found = Some(hex);
            break;
        }
    }

    let Some(expected) = found else {
        return Err(format!(
            "清单里没有这个文件：{want_name}（不能当作校验通过）"
        ));
    };
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "清单里 {want_name} 的校验值不是 64 位十六进制：{expected}"
        ));
    }

    let actual = sha256_of(bytes);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "校验失败：{want_name} 期望 {}，实际 {actual}（文件被改过或下载不完整）",
            expected.to_ascii_lowercase()
        ))
    }
}

/// 真实下载：`curl.exe` → PowerShell 兜底。
///
/// 这是 [`fetch_with`] 的薄壳，把「本机有哪些程序」换成真实的 PATH 查找。测试只测
/// [`fetch_with`]，因此**任何一条判据都不会联网**。
///
/// Real download: `curl.exe` first, PowerShell as the fallback.
///
/// A thin shell over [`fetch_with`] that uses the real PATH lookup. Tests exercise
/// [`fetch_with`] only, so **no test ever touches the network**.
pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    fetch_with(url, &locate)
}

/// 在 PATH 里找一个可执行文件。
///
/// 为什么不直接 `Command::new("curl")` 靠系统解析：我们需要**先知道**用的是哪条通道，
/// 才能在没有通道时给出明确错误；命令跑起来才发现「程序不存在」，错误会被吞成
/// `io::Error(2)`，用户看到的是「系统找不到指定的文件」，完全指不到更新器上。
///
/// Look up an executable on PATH.
///
/// Why not let `Command::new("curl")` resolve it: we need to know **which** channel we are
/// using in order to report a clear error when neither exists. Discovering it only when the
/// command fails turns the problem into `io::Error(2)`, and the user reads "the system cannot
/// find the file specified" with no pointer to the updater.
fn locate(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &["", ".exe"] } else { &[""] };
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let cand = dir.join(format!("{prog}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 用给定的查找器探测通道并下载（测试缝）。
///
/// 顺序：`curl` → `powershell`。两条都没有就返回
/// 「本机没有可用的下载通道」，**绝不放行**——静默返回空字节会让上层以为下载成功，
/// 然后在校验那一步报一个与真实原因无关的错。
///
/// 两条通道的字节回传方式不同，原因写在各自分支里。
///
/// Probe the channels with the given locator and download (test seam).
///
/// Order: `curl`, then `powershell`. If neither exists the error is
/// "本机没有可用的下载通道"; returning empty bytes would make the caller believe the download
/// succeeded and then fail at the checksum step for an unrelated reason.
///
/// The two channels hand bytes back differently; the reason is documented at each branch.
pub fn fetch_with(url: &str, find: &Locator) -> Result<Vec<u8>, String> {
    if url.trim().is_empty() {
        return Err("下载地址是空的".to_string());
    }

    if let Some(curl) = find("curl") {
        // curl 直接把字节写 stdout，管道不过代码页，二进制安全；加 -f 让 4xx/5xx 变成
        // 非零退出码，否则一份 404 的 HTML 会被当成「下载成功」写进 exe。
        // curl writes bytes straight to stdout, which bypasses the console code page and is
        // binary safe; -f turns 4xx/5xx into a non-zero exit so a 404 HTML page cannot pose
        // as a successful download.
        let out = Command::new(&curl)
            .args(["-fsSL", "--max-time", "300", "-o", "-", url])
            .stdin(std::process::Stdio::null())
            // 检查更新也走这里：漏掉这一句，点一次「检查更新」就闪一次黑框。
            //
            // The check path runs through here too: omitting this flashes a black console
            // window on every "check for updates" click.
            .no_console_window()
            .output()
            .map_err(|e| format!("调用 curl 失败（{}）：{e}", curl.display()))?;
        if !out.status.success() {
            return Err(format!(
                "curl 下载失败（退出码 {}）：{}",
                out.status.code().unwrap_or(-1),
                first_line(&out.stderr)
            ));
        }
        if out.stdout.is_empty() {
            return Err(format!("curl 返回了 0 字节：{url}"));
        }
        return Ok(out.stdout);
    }

    if let Some(ps) = find("powershell") {
        // PowerShell 必须落盘再读：`Invoke-WebRequest` 的 stdout 走控制台编码（中文系统
        // 是 GBK），二进制会被逐字节改写，exe 直接损坏。落盘顺带绕开大文件卡在内存里。
        // PowerShell has to write to disk and then be read back: `Invoke-WebRequest`'s stdout
        // goes through the console code page (GBK on Chinese Windows) and mangles binary
        // byte by byte, corrupting the exe. The temp file also keeps large downloads off the
        // pipe.
        let tmp = std::env::temp_dir().join(format!(
            "aevum-dl-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // -TimeoutSec 300 与 curl 路的 --max-time 300 对齐：没有超时的话，一个挂死的
        // 服务器会让 Command::output() 无限等待，更新流程整个卡死。
        //
        // -TimeoutSec 300 mirrors the curl path's --max-time 300: without a timeout a
        // hung server would leave Command::output() waiting forever and stall the update.
        let script = format!(
            "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -UseBasicParsing \
             -TimeoutSec 300 -Uri '{}' -OutFile '{}'",
            url.replace('\'', "''"),
            tmp.display()
        );
        let out = Command::new(&ps)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .no_console_window()
            .output()
            .map_err(|e| format!("调用 PowerShell 失败（{}）：{e}", ps.display()))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "PowerShell 下载失败（退出码 {}）：{}",
                out.status.code().unwrap_or(-1),
                first_line(&out.stderr)
            ));
        }
        // 读回失败也必须删掉临时文件，否则每次失败都在系统临时目录留一份残留。
        //
        // A failed read-back must still delete the temp file, otherwise every failure
        // leaves a stray blob behind in the system temp directory.
        let bytes = std::fs::read(&tmp).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("下载完成但读不回临时文件：{e}")
        })?;
        let _ = std::fs::remove_file(&tmp);
        if bytes.is_empty() {
            return Err(format!("PowerShell 返回了 0 字节：{url}"));
        }
        return Ok(bytes);
    }

    Err(format!(
        "本机没有可用的下载通道：既没有 curl.exe（Windows 10 1803+ 自带），也没有 powershell。\
         请手动下载 {url} 解压覆盖，或者升级到自带 curl 的系统版本"
    ))
}

/// 只取 stderr 的第一行非空内容，避免把整段堆栈塞进界面。
///
/// Take the first non-empty stderr line so the UI is not flooded with a stack trace.
fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("（没有输出）")
        .to_string()
}

/// 给定发布 JSON 文本，判断该不该更新。纯函数，不做任何 I/O。
///
/// 四种结论都写进 [`UpdateCheck::note`]，界面不需要再判一遍：
/// 解析失败 / 已有最新 / 远端是预发布 / 没有匹配当前平台的资产。
///
/// Given the release JSON text, decide whether to update. Pure, no I/O.
///
/// All four outcomes land in [`UpdateCheck::note`] so the UI never has to re-derive them:
/// parse failure, already current, remote is a pre-release, no asset for this platform.
pub fn check_release(current: &str, release_json: &str, suffix: &str) -> UpdateCheck {
    let mut out = UpdateCheck {
        current: current.to_string(),
        latest: None,
        asset: None,
        note: String::new(),
    };

    let rel = match parse_release(release_json) {
        Ok(r) => r,
        Err(e) => {
            out.note = format!("检查更新失败：{e}");
            return out;
        }
    };
    out.latest = Some(rel.tag.clone());

    if !is_newer(current, &rel.tag) {
        out.note = format!("已是最新版本（当前 {current}，远端 {}）", rel.tag);
        return out;
    }
    if rel.prerelease {
        // 预发布不自动提供：用户的机器上可能只有这一台，beta 的代价由他自己决定。
        // Pre-releases are never offered automatically: this may be the user's only machine,
        // and the cost of a beta is his to accept.
        out.note = format!(
            "远端 {} 是预发布版本，默认不提示；需要时请到发布页手动下载",
            rel.tag
        );
        return out;
    }
    match pick_asset(&rel, suffix) {
        Some(a) => {
            out.asset = Some(a.clone());
            out.note = format!(
                "发现新版本 {}（当前 {current}），可下载 {}",
                rel.tag, a.name
            );
        }
        None => {
            out.note = format!(
                "远端 {} 没有以 {suffix} 结尾的资产，可能是另一个平台的发布",
                rel.tag
            );
        }
    }
    out
}

/// 联网检查更新：拉 `api_url` 的 JSON，再交给 [`check_release`]。
///
/// 下载通道的错误也折进 `note`，因为这个函数的返回值是「给界面看的结论」，不是错误。
///
/// Online check: fetch the JSON at `api_url` and hand it to [`check_release`].
///
/// Channel errors are folded into `note` too, because the return value is "a conclusion for
/// the UI", not an error.
pub fn check_remote(fetch: &Fetcher, current: &str, api_url: &str, suffix: &str) -> UpdateCheck {
    match fetch(api_url) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => check_release(current, &text, suffix),
            Err(_) => UpdateCheck {
                current: current.to_string(),
                latest: None,
                asset: None,
                note: "检查更新失败：发布信息不是 UTF-8 文本".to_string(),
            },
        },
        Err(e) => UpdateCheck {
            current: current.to_string(),
            latest: None,
            asset: None,
            note: format!("检查更新失败：{e}"),
        },
    }
}

/// 「下载 + 校验」一步到位：先取清单，再取资产，最后比对。
///
/// 顺序不能反：先下资产再取清单，中间有时间窗口让两者来自不同的发布；先取清单再取资产
/// 也保证「清单里没有这一行」能在下载前就被发现（省一次流量，也少一次把坏文件写进
/// 临时目录的机会）。
///
/// 返回的字节**只在校验通过后**交给调用方，这是本函数唯一的契约。
///
/// Download then verify in one step: manifest first, asset second, compare last.
///
/// The order cannot be reversed: fetching the asset first leaves a window in which the two
/// come from different releases, and manifest-first also lets "this line is missing" be caught
/// before the download (one less transfer, one less chance to write a bad file to temp).
///
/// The returned bytes are handed over **only after** verification passes; that is the whole
/// contract.
pub fn download_verified(
    fetch: &Fetcher,
    asset: &ReleaseAsset,
    sums_url: &str,
) -> Result<Vec<u8>, String> {
    let sums = fetch(sums_url)?;
    let sums_text =
        String::from_utf8(sums).map_err(|_| "SHA256SUMS 不是 UTF-8 文本".to_string())?;
    let bytes = fetch(&asset.url)?;
    verify(&bytes, &sums_text, &asset.name)?;
    Ok(bytes)
}

/// 下载超时（秒）。三条通道共用一个数：`curl --max-time`、PowerShell `-TimeoutSec`、
/// 以及这里的轮询上限。分开写会出现「命令超时了但轮询还在等」的错位。
///
/// Download timeout in seconds, shared by all three channels (`curl --max-time`,
/// PowerShell `-TimeoutSec`, and the poll loop here). Separate values would let the poll
/// outlive the command it is watching.
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

/// 轮询下载进度的间隔。50 ms 对进度条足够顺滑，又不会把一个核抢走。
///
/// Progress poll interval: 50 ms is smooth enough for a bar and cheap enough for a core.
const PROGRESS_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// 下载到**文件**并回报进度（测试缝）。
///
/// 为什么不复用 [`fetch_with`]：那条路把整包读进内存，56 MB 的 zip 会在界面上表现为
/// 「点了没反应」——中间没有任何可观测的量。这里改成「交给 curl/PowerShell 落盘，
/// 我们只盯文件大小」，于是进度是**真实字节数**，不是估算出来的动画。
///
/// `on_progress(已写字节, 资产声明字节)`：资产没声明大小时第二个参数是 0，调用方应显示
/// 不确定进度而不是 0%。
///
/// 失败与超时都**删掉半截文件**：留着它会让下一次下载从错误的状态开始，也会让
/// 「文件存在」被误读成「已经下好」。
///
/// Download to a **file** while reporting progress (test seam).
///
/// Why not reuse [`fetch_with`]: that path reads the whole payload into memory, and a 56 MB
/// zip then looks like "the click did nothing" -- there is no observable quantity in between.
/// Here curl/PowerShell writes the file and we watch its size, so the progress is **real
/// bytes**, not an estimated animation.
///
/// `on_progress(written, declared_size)`: the second value is 0 when the asset does not
/// declare a size, and the caller should show an indeterminate bar rather than 0%.
///
/// Failures and timeouts **delete the partial file**: leaving it behind makes the next
/// attempt start from a wrong state and lets "the file exists" be misread as "already done".
pub fn download_to_file_with(
    asset: &ReleaseAsset,
    dest: &Path,
    find: &Locator,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    if asset.url.trim().is_empty() {
        return Err("下载地址是空的".to_string());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("建不了下载目录（{}）：{e}", parent.display()))?;
    }
    // 起始状态必须干净：上一次的半截文件会让进度条从中间开始，还会骗过大小检查。
    // Start clean: a leftover partial file starts the bar in the middle and fools size checks.
    let _ = std::fs::remove_file(dest);

    let child = if let Some(curl) = find("curl") {
        Command::new(&curl)
            .args(["-fsSL", "--max-time", "300", "-o"])
            .arg(dest)
            .arg(&asset.url)
            .stdin(std::process::Stdio::null())
            .no_console_window()
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("调用 curl 失败（{}）：{e}", curl.display()))?
    } else if let Some(ps) = find("powershell") {
        let script = format!(
            "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -UseBasicParsing \
             -TimeoutSec 300 -Uri '{}' -OutFile '{}'",
            asset.url.replace('\'', "''"),
            dest.display()
        );
        Command::new(&ps)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .no_console_window()
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("调用 PowerShell 失败（{}）：{e}", ps.display()))?
    } else {
        return Err(format!(
            "本机没有可用的下载通道：既没有 curl.exe（Windows 10 1803+ 自带），也没有 powershell。\
             请手动下载 {} 解压覆盖，或者升级到自带 curl 的系统版本",
            asset.url
        ));
    };

    let mut child = child;
    let started = std::time::Instant::now();
    let mut last = 0u64;
    loop {
        // 先报一次（可能是 0），让界面立刻从「准备中」变成「下载中 0 / N」。
        // Report once right away so the UI leaves "preparing" immediately.
        if let Ok(meta) = std::fs::metadata(dest) {
            last = meta.len();
        }
        on_progress(last, asset.size);
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    let n = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                    if n == 0 {
                        let _ = std::fs::remove_file(dest);
                        return Err("下载通道返回了 0 字节".to_string());
                    }
                    on_progress(n, asset.size.max(n));
                    return Ok(());
                }
                let err = child
                    .stderr
                    .take()
                    .map(|mut s| {
                        let mut buf = Vec::new();
                        let _ = std::io::Read::read_to_end(&mut s, &mut buf);
                        first_line(&buf)
                    })
                    .unwrap_or_else(|| "（没有输出）".to_string());
                let _ = std::fs::remove_file(dest);
                return Err(format!(
                    "下载失败（退出码 {}）：{err}",
                    status.code().unwrap_or(-1)
                ));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = std::fs::remove_file(dest);
                return Err(format!("等待下载进程失败：{e}"));
            }
        }
        if started.elapsed().as_secs() > DOWNLOAD_TIMEOUT_SECS {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(dest);
            return Err(format!(
                "下载超过 {} 秒仍未完成，已中止（网络太慢或服务器无响应）",
                DOWNLOAD_TIMEOUT_SECS
            ));
        }
        std::thread::sleep(PROGRESS_POLL);
    }
}

/// 真实下载通道版：[`download_to_file_with`] 的薄壳。
///
/// The real-channel version: a thin shell over [`download_to_file_with`].
pub fn download_to_file(
    asset: &ReleaseAsset,
    dest: &Path,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    download_to_file_with(asset, dest, &locate, on_progress)
}

/// 把一个 zip 里的可执行文件解到 `work` 下，返回它的路径（测试缝）。
///
/// 为什么要解包：发布资产是 zip（顶层目录 `Aevum-Studio-v<版本>/`），而替换脚本只认
/// 一个 exe 路径。中间这一步缺了，更新器就只能「下载成功、然后无事发生」。
///
/// 为什么不用 zip crate：更新链是唯一还没被依赖审计覆盖的角落，而 Windows 10 1803+
/// 自带 `tar.exe`（bsdtar，认 zip），退路是 PowerShell 的 `Expand-Archive`。两者都是
/// 系统组件，与 [`fetch`] 的取舍一致。
///
/// 找 exe 的规则：先找**同名**（安装目录可能改名，但镜像名跟着走），找不到再取第一个
/// `.exe`；只往下三层，避免万一解出一棵树时把 `tools/` 里的 ffmpeg 当主体。
///
/// Extract the executable from a zip into `work` and return its path (test seam).
///
/// Why unpack at all: the release asset is a zip (top-level `Aevum-Studio-v<version>/`)
/// while the replacement script takes a single exe path. Without this step the updater
/// "downloads successfully and then nothing happens".
///
/// Why no zip crate: the update chain is the last corner not covered by the dependency
/// audit, and Windows 10 1803+ ships `tar.exe` (bsdtar, which reads zip), with PowerShell's
/// `Expand-Archive` as the fallback -- both OS components, consistent with [`fetch`].
///
/// Exe lookup: prefer the **same file name** (the install dir may be renamed, the image name
/// follows it), else the first `.exe`; three levels deep at most so a full tree cannot make
/// `tools/ffmpeg.exe` look like the main binary.
pub fn expand_exe_with(
    zip: &Path,
    work: &Path,
    exe_name: &str,
    find: &Locator,
) -> Result<PathBuf, String> {
    if !zip.is_file() {
        return Err(format!("压缩包不存在：{}", zip.display()));
    }
    // 解包目录先清空：上一版残留的 aevum.exe 会被当成「刚解出来的」。
    // Clear the work dir first: an aevum.exe left by a previous run would pass as fresh.
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(work)
        .map_err(|e| format!("建不了解包目录（{}）：{e}", work.display()))?;

    let mut done = false;
    if let Some(tar) = find("tar") {
        let out = Command::new(&tar)
            .arg("-xf")
            .arg(zip)
            .arg("-C")
            .arg(work)
            .stdin(std::process::Stdio::null())
            .no_console_window()
            .output()
            .map_err(|e| format!("调用 tar 失败（{}）：{e}", tar.display()))?;
        if out.status.success() {
            done = true;
        }
    }
    if !done && let Some(ps) = find("powershell") {
        let script = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            zip.display().to_string().replace('\'', "''"),
            work.display().to_string().replace('\'', "''")
        );
        let out = Command::new(&ps)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .no_console_window()
            .output()
            .map_err(|e| format!("调用 PowerShell 解包失败（{}）：{e}", ps.display()))?;
        if !out.status.success() {
            return Err(format!(
                "解包失败（退出码 {}）：{}",
                out.status.code().unwrap_or(-1),
                first_line(&out.stderr)
            ));
        }
        done = true;
    }
    if !done {
        return Err("本机既没有 tar.exe 也没有 powershell，无法解包更新".to_string());
    }

    let found = find_exe(work, exe_name, 3)
        .ok_or_else(|| format!("压缩包里没有找到 {exe_name}（解包到 {}）", work.display()))?;
    // 防穿越：解包出来的东西必须真在 work 下，不能是符号链接指到别处。
    // Anti-traversal: the result must really live under `work`, not be a link out of it.
    let root = std::fs::canonicalize(work).map_err(|e| format!("解包目录无效：{e}"))?;
    let real = std::fs::canonicalize(&found).map_err(|e| format!("解出的文件无效：{e}"))?;
    if !real.starts_with(&root) {
        return Err(format!("解包结果跑出了临时目录：{}", found.display()));
    }
    Ok(found)
}

/// 真实通道版：[`expand_exe_with`] 的薄壳。
///
/// The real-channel version: a thin shell over [`expand_exe_with`].
pub fn expand_exe(zip: &Path, work: &Path, exe_name: &str) -> Result<PathBuf, String> {
    expand_exe_with(zip, work, exe_name, &locate)
}

/// 在 `dir` 往下 `depth` 层里找可执行文件，同名优先。
///
/// Find an executable under `dir` down to `depth` levels, preferring the exact name.
fn find_exe(dir: &Path, exe_name: &str, depth: usize) -> Option<PathBuf> {
    let mut any: Option<PathBuf> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if depth > 1 {
                dirs.push(path);
            }
            continue;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let Some(name) = name else { continue };
        if name.eq_ignore_ascii_case(exe_name) {
            return Some(path);
        }
        if name.to_ascii_lowercase().ends_with(".exe") && any.is_none() {
            any = Some(path);
        }
    }
    for sub in dirs {
        if let Some(hit) = find_exe(&sub, exe_name, depth - 1) {
            // 同名的优先于「随便一个 exe」：子树里可能只有 ffmpeg。
            // A name match beats "any exe": a subtree may hold nothing but ffmpeg.
            if hit
                .file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case(exe_name))
                .unwrap_or(false)
            {
                return Some(hit);
            }
            any.get_or_insert(hit);
        }
    }
    any
}

/// 生成「回滚到上一版」的批处理文本（不执行，只生成）。
///
/// 前提：[`install_script`] 会**保留** `*.old`，所以上一版永远在原地。这个脚本做的事
/// 与更新相反——把当前的 exe 改名成 `*.failed`（不删，留着排查），再把 `*.old` 搬回来
/// 并启动。当前版本起不来时，用户双击它就能回到上一版，不需要重装。
///
/// 参数：`%~1` 目标 exe，`%~2` 进程名（`tasklist` 的过滤器要它）。路径走参数而不是
/// 写进正文——理由与 [`install_script`] 里那段 OEM 代码页说明同源。
///
/// 与更新脚本一样：纯 ASCII、先等进程退出、失败不留下没有 exe 的目录。
///
/// Generate the "roll back to the previous version" batch text (generated, never run here).
///
/// Precondition: [`install_script`] **keeps** `*.old`, so the previous build is always
/// there. This script does the opposite of an update -- rename the current exe to
/// `*.failed` (kept for diagnosis), move `*.old` back, start it. When the current version
/// refuses to start, double-clicking this file returns to the previous build without a
/// reinstall.
///
/// Args: `%~1` target exe, `%~2` image name (the `tasklist` filter needs it). Paths travel as
/// arguments rather than inline, for the same OEM code-page reason documented on
/// [`install_script`].
///
/// Same rules as the update script: ASCII only, wait for exit first, never leave a directory
/// without an exe.
pub fn rollback_script() -> &'static str {
    // 正文里**不出现任何路径**：全部由命令行参数带进来（见 [`install_script`] 里那段
    // 关于 OEM 代码页的说明，这里是同一个坑的第二个出口）。
    //
    // No path appears **in the body** at all: everything arrives as command-line arguments
    // (see the OEM code-page note in [`install_script`]; this is the second exit of the same
    // trap).
    "@echo off\r\n\
         rem Aevum Studio rollback helper. Generated by aevum-core::update; do not edit.\r\n\
         rem Order: wait for exit -> current aside -> old back -> start.\r\n\
         rem Args: 1 = target exe, 2 = image name.\r\n\
         rem Paths arrive as arguments, never inline: cmd reads this file with the OEM code\r\n\
         rem page, so a non-ASCII path written inline becomes mojibake and every move fails\r\n\
         rem silently behind >NUL.\r\n\
         setlocal\r\n\
         set \"TARGET=%~1\"\r\n\
         set \"IMAGE=%~2\"\r\n\
         \r\n\
         if \"%TARGET%\"==\"\" goto noargs\r\n\
         \r\n\
         if not exist \"%TARGET%.old\" (\r\n\
         \x20   echo No previous version found: \"%TARGET%.old\"\r\n\
         \x20   (goto) 2>NUL & del /F /Q \"%~f0\"\r\n\
         \x20   exit /b 1\r\n\
         )\r\n\
         \r\n\
         rem 1/4 Wait until the running app has exited.\r\n\
         :wait\r\n\
         \"%SystemRoot%\\System32\\tasklist.exe\" /FI \"IMAGENAME eq %IMAGE%\" 2>NUL | \
         \"%SystemRoot%\\System32\\find.exe\" /I \"%IMAGE%\" >NUL\r\n\
         if not errorlevel 1 (\r\n\
         \x20   \"%SystemRoot%\\System32\\ping.exe\" -n 2 127.0.0.1 >NUL\r\n\
         \x20   goto wait\r\n\
         )\r\n\
         \r\n\
         rem 2/4 Keep the current build aside for diagnosis (do not delete it).\r\n\
         if exist \"%TARGET%\" move /Y \"%TARGET%\" \"%TARGET%.failed\" >NUL 2>&1\r\n\
         \r\n\
         rem 3/4 Put the previous build back.\r\n\
         move /Y \"%TARGET%.old\" \"%TARGET%\" >NUL 2>&1\r\n\
         if not exist \"%TARGET%\" (\r\n\
         \x20   if exist \"%TARGET%.failed\" move /Y \"%TARGET%.failed\" \"%TARGET%\" >NUL 2>&1\r\n\
         \x20   (goto) 2>NUL & del /F /Q \"%~f0\"\r\n\
         \x20   exit /b 1\r\n\
         )\r\n\
         \r\n\
         rem 4/4 Start the restored build, then this script.\r\n\
         start \"\" \"%TARGET%\"\r\n\
         (goto) 2>NUL & del /F /Q \"%~f0\"\r\n\
         exit /b 0\r\n\
         \r\n\
         :noargs\r\n\
         echo Missing arguments: target exe and image name.\r\n\
         (goto) 2>NUL & del /F /Q \"%~f0\"\r\n\
         exit /b 2\r\n"
}

/// 安装目录能不能安全地交给批处理。
///
/// `%` 与 `"` 在 CMD 里无法安全转义（`%` 会被当成变量引用），遇到就明确拒绝，而不是
/// 生成一个必然出错的脚本。
///
/// Whether an install directory can be handed to the batch script safely.
///
/// `%` and `"` cannot be escaped safely in CMD (`%` starts a variable reference). Reject
/// them explicitly instead of emitting a script that is guaranteed to misbehave.
pub fn check_install_dir(target_exe: &Path) -> Result<(), String> {
    let text = target_exe.display().to_string();
    if text.contains('%') || text.contains('"') {
        return Err(format!(
            "安装目录里含 % 或 \"，批处理无法安全转义，请把程序装到普通目录：{text}"
        ));
    }
    // 非 ASCII 路径（中文用户名）**不拒绝**：引号内的路径由文件系统解释，批处理照样
    // 搬得动；只有在 echo 出来时才会被 GBK 代码页弄乱，而本脚本不回显路径。
    //
    // Non-ASCII paths (a Chinese user name) are **not** rejected: inside quotes the
    // filesystem interprets them fine and the batch still moves them; only *echoed* text is
    // mangled by the GBK code page, and this script never echoes a path.
    Ok(())
}

/// 生成替换用的 Windows 批处理文本（不执行，只生成）。
///
/// 五步，顺序不能变：
/// 1. **等当前进程退出** —— Windows 不允许覆盖正在执行的 exe，哪怕只是改名；
/// 2. **把旧 exe 改名成 `*.old`** —— 单次 rename 是可回滚的，失败时原文件仍在原处；
/// 3. **把新 exe 搬到目标位置**，被占用就重试（杀软与索引器会短暂持有句柄）；
/// 4. **启动新 exe**；
/// 5. **保留备份**（`*.old` 留着，供 [`rollback_script`] 回滚）并清理自身。
///
/// 为什么是 CMD 而不是 PowerShell：这段脚本要在「刚刚退出、可能还没干净」的时刻尽快跑
/// 起来，CMD 的启动开销是 PowerShell 的十分之一，而它只做 `move` / `start` / `del` 三件
/// 事。脚本内部**只用 ASCII**：中文注释会在 GBK 代码页下变成乱码，甚至吃掉下一行的
/// 第一个字符。
///
/// 失败路径把 `*.old` 改回原名：更新失败时用户至少要还能打开旧版本，而不是面对一个
/// 空目录。
///
/// 路径里的 `%` 与 `"` 无法在批处理里安全转义，调用方应拒绝这种安装目录（正常的
/// `%ProgramFiles%` 与用户目录都不含它们）。
///
/// **三个路径全走命令行参数**（`%~1` 新 exe / `%~2` 目标 exe / `%~3` 进程名），正文一个
/// 路径都不写。这不是风格问题：批处理按 OEM 代码页解码，UTF-8 的中文路径写进正文就是
/// 乱码（2026-11-05 在 `E:\tmp\...\恒晷 程序\` 上实测：`if exist` 与 `move` 全部落空、
/// 十五次重试后静默回滚）；参数走命令行（UTF-16），不受代码页影响。
///
/// 系统工具一律写成 `%SystemRoot%\System32\...` 的绝对路径：PATH 上若有一个 Unix 版
/// `find`（开发者装了 Git/MSYS），`tasklist | find /I` 会解析到它，`/I` 直接报错 →
/// 等待循环失效 → 程序还开着就开始替换（同一台机器上实测过）。
///
/// 为什么等待用 `ping -n 2 127.0.0.1` 而不是 `timeout /t 1`：`timeout` 在 stdin 被重定向
/// 时（更新器一般是无控制台方式拉起这个脚本的）会立刻报错退出，`goto wait` 随即变成
/// 忙等，把一个核跑满；而 `ping` 的 1 秒是真的等 1 秒。
///
/// Generate the Windows batch text used for replacement (generated, never executed here).
///
/// Five steps, order fixed:
/// 1. **wait for the current process to exit** -- Windows refuses to overwrite a running exe,
///    even to rename it;
/// 2. **rename the old exe to `*.old`** -- a single rename is reversible, and if it fails the
///    original is still in place;
/// 3. **move the new exe into place**, retrying while an antivirus or indexer holds a handle;
/// 4. **start the new exe**;
/// 5. **keep the backup** (`*.old` stays on disk for [`rollback_script`]) and clean up this
///    script.
///
/// **All three paths arrive as command-line arguments** (`%~1` new exe, `%~2` target exe,
/// `%~3` image name); no path is written into the body. This is not style: a batch file is
/// decoded with the OEM code page, so a UTF-8 Chinese path inline becomes mojibake -- measured
/// on `E:\tmp\...\恒晷 程序\` on 2026-11-05, where `if exist` and `move` both missed and the
/// script silently rolled back after fifteen retries. Arguments travel on the command line
/// (UTF-16) and are immune to the code page.
///
/// System tools are always spelled `%SystemRoot%\System32\...`: with a Unix `find` on PATH
/// (any developer with Git/MSYS) `tasklist | find /I` resolves to the wrong program, `/I`
/// fails, the wait loop silently stops waiting, and the replacement starts while the app is
/// still running (also measured on this machine).
///
/// Why CMD and not PowerShell: this runs right after exit, possibly before things settle; CMD
/// starts about ten times faster than PowerShell and only needs `move` / `start` / `del`. The
/// script is **ASCII-only**: Chinese comments turn into mojibake under the GBK code page and
/// can even swallow the first character of the next line.
///
/// Why the sleep is `ping -n 2 127.0.0.1` and not `timeout /t 1`: `timeout` exits at once
/// when stdin is redirected (the updater spawns this script without a console), which turns
/// `goto wait` into a busy loop that pegs a core; `ping` really waits one second.
///
/// Why the sleep is `ping -n 2 127.0.0.1` and not `timeout /t 1`: `timeout` exits at once
/// when stdin is redirected (the updater spawns this script without a console), which turns
/// `goto wait` into a busy loop that pegs a core; `ping` really waits one second.
///
/// The failure path renames `*.old` back, so a failed update still leaves the user with a
/// working old version instead of an empty folder.
///
/// `%` and `"` inside paths cannot be escaped safely in batch; callers should reject such
/// install directories (normal `%ProgramFiles%` and user profiles never contain them).
pub fn install_script() -> &'static str {
    // 正文里**不出现任何路径**，三个参数由命令行带进来：`%~1` 新 exe、`%~2` 目标 exe、
    // `%~3` 进程名。原因不是洁癖，是实测出来的真故障：批处理文件由 `cmd` 按 **OEM 代码页**
    // （中文系统是 936）解码，而 Rust 写字符串是 UTF-8；安装路径里只要有一个汉字
    // （`D:\软件\恒晷\`、`C:\Users\张三\...`），写进正文后就是乱码，`if exist` 与 `move`
    // 全部找不到目标，而失败又都嵌在 `>NUL 2>&1` 里——用户看到的是「点了更新，什么都没
    // 发生」，十五次重试后脚本静默回滚。
    //
    // 参数走的是命令行（UTF-16），不受代码页影响；正文因此保持纯 ASCII。
    //
    // No path appears **in the body**: three arguments come from the command line -- `%~1`
    // the new exe, `%~2` the target exe, `%~3` the image name. The reason is not tidiness but
    // a failure measured on a real machine: `cmd` decodes a batch file with the **OEM code
    // page** (936 on Chinese Windows) while Rust writes UTF-8, so one Chinese character in
    // the install path (`D:\软件\...`, `C:\Users\张三\...`) becomes mojibake in the body and
    // both `if exist` and `move` miss their target -- with every error swallowed by
    // `>NUL 2>&1`. The user sees "clicked update, nothing happened", followed by a silent
    // rollback after fifteen retries.
    //
    // Arguments travel on the command line (UTF-16) and are immune to the code page, which
    // is why the body can stay pure ASCII.
    "@echo off\r\n\
         rem Aevum Studio update helper. Generated by aevum-core::update; do not edit.\r\n\
         rem Order: wait for exit -> rename old aside -> move new in -> start -> keep backup.\r\n\
         rem Args: 1 = new exe, 2 = target exe, 3 = image name.\r\n\
         rem Paths arrive as arguments, never inline: cmd reads this file with the OEM code\r\n\
         rem page, so a non-ASCII path written inline becomes mojibake and every move fails\r\n\
         rem silently behind >NUL.\r\n\
         setlocal\r\n\
         set \"NEW=%~1\"\r\n\
         set \"TARGET=%~2\"\r\n\
         set \"IMAGE=%~3\"\r\n\
         \r\n\
         if \"%NEW%\"==\"\" goto noargs\r\n\
         if \"%TARGET%\"==\"\" goto noargs\r\n\
         \r\n\
         rem 1/5 Wait until the running app has exited. Windows will not replace a live exe.\r\n\
         :wait\r\n\
         \"%SystemRoot%\\System32\\tasklist.exe\" /FI \"IMAGENAME eq %IMAGE%\" 2>NUL | \
         \"%SystemRoot%\\System32\\find.exe\" /I \"%IMAGE%\" >NUL\r\n\
         if not errorlevel 1 (\r\n\
         \x20   \"%SystemRoot%\\System32\\ping.exe\" -n 2 127.0.0.1 >NUL\r\n\
         \x20   goto wait\r\n\
         )\r\n\
         \r\n\
         rem 2/5 Rename the old exe aside: a failed rename leaves the original untouched.\r\n\
         if exist \"%TARGET%\" move /Y \"%TARGET%\" \"%TARGET%.old\" >NUL 2>&1\r\n\
         \r\n\
         rem 3/5 Move the new exe in, retrying while a scanner may still hold a handle.\r\n\
         set /a tries=0\r\n\
         :place\r\n\
         move /Y \"%NEW%\" \"%TARGET%\" >NUL 2>&1\r\n\
         if not exist \"%NEW%\" goto started\r\n\
         set /a tries+=1\r\n\
         if %tries% GEQ 15 goto rollback\r\n\
         \"%SystemRoot%\\System32\\ping.exe\" -n 2 127.0.0.1 >NUL\r\n\
         goto place\r\n\
         \r\n\
         :started\r\n\
         rem 4/5 Start the new version.\r\n\
         start \"\" \"%TARGET%\"\r\n\
         \r\n\
         rem 5/5 Keep the backup on purpose: a broken build is undone by going back to it.\r\n\
         (goto) 2>NUL & del /F /Q \"%~f0\"\r\n\
         exit /b 0\r\n\
         \r\n\
         :rollback\r\n\
         rem The move never succeeded: put the old exe back so the user still has a build.\r\n\
         if exist \"%TARGET%.old\" move /Y \"%TARGET%.old\" \"%TARGET%\" >NUL 2>&1\r\n\
         (goto) 2>NUL & del /F /Q \"%~f0\"\r\n\
         exit /b 1\r\n\
         \r\n\
         :noargs\r\n\
         echo Missing arguments: new exe, target exe, image name.\r\n\
         (goto) 2>NUL & del /F /Q \"%~f0\"\r\n\
         exit /b 2\r\n"
}

/// `serde_json` 的中间结构：全部 `Option`，缺字段由 [`parse_release`] 点名报错。
///
/// The intermediate `serde_json` shape: every field optional, so [`parse_release`] can name
/// the missing one.
#[derive(Deserialize)]
struct RawRelease {
    tag_name: Option<String>,
    name: Option<String>,
    body: Option<String>,
    prerelease: Option<bool>,
    assets: Option<Vec<RawAsset>>,
}

/// 中间结构里的资产。
///
/// An asset in the intermediate shape.
#[derive(Deserialize)]
struct RawAsset {
    name: Option<String>,
    browser_download_url: Option<String>,
    size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 的官方测试向量（FIPS 180-4 / RFC 6234）。常量写错、移位写错、
    /// 长度按字节算，这三种错都会在这一条上现形。
    ///
    /// The official SHA-256 vectors. Wrong constants, wrong rotations, or a byte-based length
    /// all show up here.
    #[test]
    fn sha256_matches_official_vectors() {
        assert_eq!(
            sha256_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 55 / 56 / 64 字节覆盖「填充刚好卡在块边界」的三处分支。
        // 55 / 56 / 64 bytes cover the three padding paths that straddle a block boundary.
        assert_eq!(
            sha256_of(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256_of(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_ne!(sha256_of(b"a"), sha256_of(b"b"));
    }

    #[test]
    fn version_compare_is_numeric_not_lexicographic() {
        assert!(is_newer("1.8.0", "v1.10.0"), "1.10.0 必须大于 1.9.0");
        assert!(is_newer("1.9.0", "1.10.0"));
        assert!(!is_newer("1.10.0", "1.9.0"), "不能反向下降");
        assert!(!is_newer("1.8.0", "1.8.0"));
    }
}
