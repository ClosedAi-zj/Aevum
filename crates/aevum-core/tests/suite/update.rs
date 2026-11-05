//! 内置更新器的判据：版本比较、发布解析、资产挑选、校验和、替换脚本、通道缺失。
//!
//! **一条都不联网**：所有网络函数都走可注入的 `Fetcher`，或者把「本机有哪些程序」
//! 换成返回 `None` 的查找器。真跑一次 GitHub 请求的判据在 CI 上会变成随机红。
//!
//! Judgements for the built-in updater: version comparison, release parsing, asset picking,
//! checksums, the replacement script, and the missing-channel case.
//!
//! **None of them touch the network**: every network-taking function goes through an
//! injectable `Fetcher`, or replaces the "what is installed here" probe with a locator that
//! returns `None`. A judgement that really hits GitHub would be flaky on CI.

use std::collections::BTreeMap;
use std::path::Path;

use aevum_core::update::{
    Release, ReleaseAsset, check_install_dir, check_release, check_remote, download_verified,
    expand_exe_with, fetch_with, install_script, is_newer, parse_release, parse_version,
    pick_asset, rollback_script, sha256_of, verify,
};

/// 固定载荷与它的 SHA-256（用 Python 的 `hashlib` 独立算出，不是拿被测实现自己算的）。
/// 这样 `verify` 的通过/失败才是在跟**外部真值**比，而不是自己跟自己对。
///
/// A fixed payload and its SHA-256, computed independently with Python's `hashlib` rather than
/// with the implementation under test. That way `verify` is checked against an **external
/// truth**, not against itself.
const PAYLOAD: &[u8] = b"Aevum v1.9.0 payload\n";
const PAYLOAD_SHA: &str = "bfa00fa878bb0ec471a3b5779d6f29cc520e422772bea8e2a72cdec179859dc6";
const ASSET: &str = "Aevum-Studio-v1.9.0-windows-x64.zip";

/// 造一份形状与 GitHub `/releases/latest` 一致的 JSON。
///
/// Build JSON shaped like GitHub's `/releases/latest`.
fn release_json(tag: &str, prerelease: bool, assets: &[(&str, &str)]) -> String {
    let assets: Vec<serde_json::Value> = assets
        .iter()
        .map(|(name, url)| {
            serde_json::json!({
                "name": name,
                "browser_download_url": url,
                "size": 1234,
            })
        })
        .collect();
    serde_json::json!({
        "tag_name": tag,
        "name": "恒晷 v1.9.0",
        "body": "更新说明",
        "prerelease": prerelease,
        "assets": assets,
    })
    .to_string()
}

/// 用一张 URL → 字节的表造一个假下载通道；表里没有的 URL 直接报错（说明测试里拼错了）。
///
/// A fake download channel backed by a URL → bytes table; unknown URLs error out so a typo in
/// a test surfaces instead of silently returning something.
fn table_fetcher(table: Vec<(&'static str, Vec<u8>)>) -> impl Fn(&str) -> Result<Vec<u8>, String> {
    let map: BTreeMap<String, Vec<u8>> =
        table.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    move |url: &str| {
        map.get(url)
            .cloned()
            .ok_or_else(|| format!("测试里没有为 {url} 准备字节"))
    }
}

// ---------------------------------------------------------------------------
// 1. 版本比较
// ---------------------------------------------------------------------------

/// `1.10.0 > 1.9.0` 是**字符串比较**会错的经典坑：按字节 `"1.10.0" < "1.9.0"`。
/// 这一条用数值比较实现时必绿、用字符串比较时必红。
///
/// `1.10.0 > 1.9.0` is the classic trap of **string** comparison: byte-wise `"1.10.0" <
/// "1.9.0"`. This goes green with numeric comparison and red with string comparison.
#[test]
fn version_compare_handles_multi_digit_and_prerelease() {
    assert!(is_newer("1.8.0", "v1.10.0"), "v1.10.0 必须比 1.8.0 新");
    assert!(is_newer("1.9.0", "1.10.0"), "10 要按十算，不是按字符算");
    assert!(is_newer("1.10.0", "1.10.1"));
    assert!(is_newer("0.9.9", "1.0.0"));
    assert!(!is_newer("1.10.0", "1.9.0"), "不能把降级当成更新");
    assert!(!is_newer("1.8.0", "v1.8.0"), "带不带 v 是同一个版本");

    // 三段解析：`v` 前缀可省，多一位、少一段、带后缀都拒绝。
    assert_eq!(parse_version("v1.8.0"), Some((1, 8, 0)));
    assert_eq!(parse_version("1.8.0"), Some((1, 8, 0)));
    assert_eq!(parse_version("v1.10.42"), Some((1, 10, 42)));
    assert_eq!(parse_version("v1.8"), None, "两段不是完整版本");
    assert_eq!(
        parse_version("v1.8.0-beta"),
        None,
        "预发布由 prerelease 单独判断"
    );
    assert_eq!(parse_version("v1.8.0.1"), None);
    assert_eq!(parse_version("v1.+8.0"), None, "u32 会接受 +8，必须自己挡");
    assert_eq!(parse_version("vv1.8.0"), None);
    assert_eq!(parse_version(""), None);

    // 解析不了的一侧一律不提示更新（宁可不升，也不要拿乱码版本号去比）。
    assert!(!is_newer("1.8.0", "nightly"));
    assert!(!is_newer("nightly", "1.9.0"));
    assert!(!is_newer("1.8.0", "1.8.0-beta"));
}

// ---------------------------------------------------------------------------
// 2. 发布解析
// ---------------------------------------------------------------------------

/// 残缺 JSON 与空资产必须报**可读错误**，且不能 panic（`unwrap` 在这种输入上必炸）。
///
/// Broken JSON and empty assets must produce a **readable error** and never panic (`unwrap`
/// would explode on exactly this input).
#[test]
fn parse_release_rejects_broken_json_and_empty_assets() {
    let good = release_json("v1.9.0", false, &[(ASSET, "https://example.invalid/a.zip")]);
    let rel = parse_release(&good).expect("完整发布应当解析成功");
    assert_eq!(rel.tag, "v1.9.0");
    assert_eq!(rel.assets.len(), 1);
    assert_eq!(rel.assets[0].name, ASSET);
    assert!(!rel.prerelease);

    // 不是 JSON
    let e = parse_release("{ not json").expect_err("残缺 JSON 必须报错");
    assert!(e.contains("JSON"), "错误要说清是 JSON 坏了，实际：{e}");

    // 空对象：缺 tag_name
    let e = parse_release("{}").expect_err("缺 tag_name 必须报错");
    assert!(e.contains("tag_name"), "要点名缺的字段，实际：{e}");

    // 标签不是三段版本号
    let bad_tag = release_json("v1.9", false, &[(ASSET, "https://example.invalid/a.zip")]);
    let e = parse_release(&bad_tag).expect_err("两段标签必须报错");
    assert!(e.contains("v1.9"), "错误里要带上那个标签，实际：{e}");

    // 空资产列表
    let empty = release_json("v1.9.0", false, &[]);
    let e = parse_release(&empty).expect_err("空 assets 必须报错");
    assert!(e.contains("空"), "要说清是资产列表空，实际：{e}");

    // 资产缺 url
    let no_url = serde_json::json!({
        "tag_name": "v1.9.0",
        "assets": [{ "name": ASSET }],
    })
    .to_string();
    let e = parse_release(&no_url).expect_err("资产缺 url 必须报错");
    assert!(e.contains("browser_download_url"), "实际：{e}");

    // 预发布要原样保留（界面靠它决定不主动提示）
    let pre = release_json("v2.0.0", true, &[(ASSET, "https://example.invalid/a.zip")]);
    assert!(parse_release(&pre).expect("预发布也要能解析").prerelease);
}

// ---------------------------------------------------------------------------
// 3. 挑资产
// ---------------------------------------------------------------------------

/// 按后缀挑：命中返回对应资产，没命中返回 `None`（不能「随便挑一个」）。
///
/// Pick by suffix: a hit returns that asset, a miss returns `None` (never "just take one").
#[test]
fn pick_asset_matches_suffix_and_returns_none_otherwise() {
    let rel = Release {
        tag: "v1.9.0".to_string(),
        name: String::new(),
        body: String::new(),
        prerelease: false,
        assets: vec![
            ReleaseAsset {
                name: "Aevum-Studio-v1.9.0-windows-arm64.zip".to_string(),
                url: "https://example.invalid/arm64.zip".to_string(),
                size: 1,
            },
            ReleaseAsset {
                name: "Aevum-Studio-v1.9.0-windows-x64.zip".to_string(),
                url: "https://example.invalid/x64.zip".to_string(),
                size: 2,
            },
            ReleaseAsset {
                name: "SHA256SUMS".to_string(),
                url: "https://example.invalid/sums".to_string(),
                size: 3,
            },
        ],
    };

    let a = pick_asset(&rel, "windows-x64.zip").expect("x64 包必须挑到");
    assert_eq!(a.name, "Aevum-Studio-v1.9.0-windows-x64.zip");
    assert_eq!(a.size, 2, "不能挑成 arm64");

    assert!(
        pick_asset(&rel, "linux-x64.tar.gz").is_none(),
        "没有就是 None"
    );
    assert!(
        pick_asset(&rel, "windows-x64").is_none(),
        "后缀要完整匹配，不能只匹配前半段"
    );

    // 大小写不敏感：发布名大小写由打包脚本决定，不能靠人记。
    assert!(pick_asset(&rel, "WINDOWS-X64.ZIP").is_some());
    assert_eq!(
        pick_asset(&rel, "SHA256SUMS").map(|a| a.name.as_str()),
        Some("SHA256SUMS")
    );
}

// ---------------------------------------------------------------------------
// 4. 校验和
// ---------------------------------------------------------------------------

/// 三种情况各一条：正确通过、改一个字节失败、清单里没有该条目**也是失败**。
/// 第三条是重点：把「找不到」当成「不用校验」是这类代码最常见的漏洞。
///
/// Three cases: passes, one byte changed fails, and a missing manifest entry **also fails**.
/// The third is the point: treating "not found" as "no checksum needed" is the most common
/// hole in this kind of code.
#[test]
fn verify_detects_tampering_and_missing_entry() {
    let sums = format!("{PAYLOAD_SHA}  {ASSET}\n");

    verify(PAYLOAD, &sums, ASSET).expect("哈希一致必须通过");

    // 大小写不同的十六进制也是同一个值（`sha256sum` 与 PowerShell 输出习惯不同）
    let upper = format!("{}  {ASSET}\n", PAYLOAD_SHA.to_ascii_uppercase());
    verify(PAYLOAD, &upper, ASSET).expect("十六进制大小写不影响结论");

    // 改一个字节 —— 用长度不变但内容不同的载荷，避免「长度变了所以失败」这种假绿。
    let mut tampered = PAYLOAD.to_vec();
    tampered[0] = b'B';
    let e = verify(&tampered, &sums, ASSET).expect_err("被篡改的字节必须失败");
    assert!(e.contains("校验失败"), "实际：{e}");
    assert!(e.contains(PAYLOAD_SHA), "错误里要带期望值，方便对账：{e}");

    // 清单里没有这个文件 —— 必须是错误，不能因为「没找到」就放行。
    let other = format!("{PAYLOAD_SHA}  Aevum-Studio-v1.9.0-linux-x64.tar.gz\n");
    let e = verify(PAYLOAD, &other, ASSET).expect_err("清单里没有该文件必须失败");
    assert!(e.contains("清单里没有"), "实际：{e}");

    // 空清单同理
    assert!(verify(PAYLOAD, "", ASSET).is_err(), "空清单不能算通过");

    // 期望值不是 64 位十六进制 → 失败，而不是拿它去比
    let short = format!("deadbeef  {ASSET}\n");
    assert!(
        verify(PAYLOAD, &short, ASSET).is_err(),
        "残缺的校验值不能放行"
    );

    // 清单行格式：`*` 二进制标记要能剥掉
    let starred = format!("{PAYLOAD_SHA} *{ASSET}\n");
    verify(PAYLOAD, &starred, ASSET).expect("`*` 标记要能被剥掉");

    // sha256_of 本身对齐已知向量（空串），防止 verify 与实现一起错
    assert_eq!(
        sha256_of(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

// ---------------------------------------------------------------------------
// 5. 检查 → 下载 → 校验（全假 fetch）
// ---------------------------------------------------------------------------

/// 完整流程用假通道跑，一次网络都不碰。
///
/// The full flow on a fake channel, without a single network call.
#[test]
fn check_and_download_flow_never_touches_network() {
    const API: &str = "https://example.invalid/releases/latest";
    const ZIP_URL: &str = "https://example.invalid/Aevum-Studio-v1.9.0-windows-x64.zip";
    const SUMS_URL: &str = "https://example.invalid/SHA256SUMS";

    let json = release_json(
        "v1.9.0",
        false,
        &[(ASSET, ZIP_URL), ("SHA256SUMS", SUMS_URL)],
    );

    // 纯函数入口先单独验一遍：手上有 JSON 时不需要走网络（MCP / CLI 的自检就用这条）。
    let offline = check_release("1.8.0", &json, "windows-x64.zip");
    assert!(offline.installable(), "{}", offline.note);
    assert_eq!(offline.latest.as_deref(), Some("v1.9.0"));

    let fetch = table_fetcher(vec![
        (API, json.into_bytes()),
        (ZIP_URL, PAYLOAD.to_vec()),
        (SUMS_URL, format!("{PAYLOAD_SHA}  {ASSET}\n").into_bytes()),
    ]);

    let check = check_remote(&fetch, "1.8.0", API, "windows-x64.zip");
    assert!(
        check.installable(),
        "1.8.0 应当能升到 1.9.0：{}",
        check.note
    );
    assert_eq!(check.current, "1.8.0");
    assert_eq!(check.latest.as_deref(), Some("v1.9.0"));
    let asset = check.asset.clone().expect("应当挑到 zip 资产");
    assert_eq!(asset.name, ASSET);

    let bytes = download_verified(&fetch, &asset, SUMS_URL).expect("校验应当通过");
    assert_eq!(bytes, PAYLOAD, "拿到的内容必须与发布的一致");

    // 已经是最新：不提供下载，但 note 仍要非空（界面直接显示）
    let same = check_remote(&fetch, "1.9.0", API, "windows-x64.zip");
    assert!(!same.installable());
    assert!(same.note.contains("最新"), "实际：{}", same.note);

    // 通道报错被折进 note，不 panic
    let broken: &dyn Fn(&str) -> Result<Vec<u8>, String> = &|_: &str| Err("断网".to_string());
    let failed = check_remote(broken, "1.8.0", API, "windows-x64.zip");
    assert!(!failed.installable());
    assert!(failed.note.contains("断网"), "实际：{}", failed.note);

    // 下载到被篡改的内容 → 校验必须拦住
    let evil = table_fetcher(vec![
        (ZIP_URL, b"evil".to_vec()),
        (SUMS_URL, format!("{PAYLOAD_SHA}  {ASSET}\n").into_bytes()),
    ]);
    assert!(
        download_verified(&evil, &asset, SUMS_URL).is_err(),
        "内容与清单不符时不能返回字节"
    );
}

// ---------------------------------------------------------------------------
// 6. 替换脚本的文本
// ---------------------------------------------------------------------------

/// 真替换在单测里没法安全执行（会把测试进程的 exe 换掉），所以判据断言**文本**：
/// 五步齐全、顺序正确、正文里一个路径都没有（路径全走 `%~1`/`%~2`/`%~3` 参数）。
///
/// 「正文不含路径」不是洁癖：批处理按 OEM 代码页解码，UTF-8 的中文安装路径写进正文会
/// 变成乱码，`if exist` 与 `move` 全部落空而错误又被 `>NUL` 吞掉——用户看到的是「点了
/// 更新，什么都没发生」。2026-11-05 在 `E:\tmp\...\恒晷 程序\` 上实测到过这一幕。
///
/// A real replacement cannot run safely in a unit test (it would swap the test binary), so
/// the judgement asserts the **text**: all five steps, in order, and **no path in the body**
/// (paths arrive as `%~1`/`%~2`/`%~3`).
///
/// "No path in the body" is not tidiness: a batch file is decoded with the OEM code page, so
/// a UTF-8 Chinese install path inline becomes mojibake, `if exist` and `move` both miss,
/// and `>NUL` swallows the errors -- the user sees "clicked update, nothing happened".
/// Measured on `E:\tmp\...\恒晷 程序\` on 2026-11-05.
#[test]
fn install_script_has_five_steps_and_takes_paths_as_arguments() {
    let script = install_script();

    // 三个参数被显式接住：路径只要写进正文就会踩 OEM 代码页的坑。
    for needle in [
        r#"set "NEW=%~1""#,
        r#"set "TARGET=%~2""#,
        r#"set "IMAGE=%~3""#,
    ] {
        assert!(script.contains(needle), "缺少参数接收：{needle}\n{script}");
    }
    assert!(
        !script.contains(":\\"),
        "正文里不能出现盘符路径（会被 OEM 代码页弄成乱码）\n{script}"
    );

    // 系统工具用绝对路径：PATH 上有个 Unix `find`（装了 Git/MSYS）时，`find /I` 会解析
    // 到它并直接报错，等待循环于是形同虚设——程序还开着就开始替换。
    for tool in ["tasklist.exe", "find.exe", "ping.exe"] {
        let needle = format!(r#""%SystemRoot%\System32\{tool}""#);
        assert!(
            script.contains(&needle),
            "系统工具必须走绝对路径：{needle}\n{script}"
        );
    }

    // 五步的关键片段（顺序在下面单独钉）
    for (what, needle) in [
        ("等待退出", "IMAGENAME eq %IMAGE%"),
        ("等待循环", "goto wait"),
        ("改名旧文件", r#"move /Y "%TARGET%" "%TARGET%.old""#),
        ("搬入新文件", r#"move /Y "%NEW%" "%TARGET%""#),
        ("启动新文件", r#"start "" "%TARGET%""#),
    ] {
        assert!(
            script.contains(needle),
            "缺少「{what}」这一步：{needle}\n{script}"
        );
    }

    // 顺序：等退出 → 改名 → 搬入 → 启动。顺序错了脚本照样能生成，但会在真实的更新里
    // 把正在运行的 exe 覆盖失败，所以必须在这里钉死。
    let pos = |needle: &str| {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("缺少 {needle}"))
    };
    assert!(pos(":wait") < pos(r#""%TARGET%.old""#));
    assert!(pos(r#"move /Y "%NEW%" "%TARGET%""#) < pos(r#"start "" "%TARGET%""#));

    // 备份**不删**：新版起不来时，回滚脚本要靠这份 `.old` 回到上一版。
    //
    // The backup is **kept**: when the new build does not start, the rollback script needs
    // `.old` to go back.
    assert!(
        !script.contains(r#"del /F /Q "%TARGET%.old""#),
        "备份不能被删掉，否则没有回滚路径\n{script}"
    );
    assert!(
        script.contains(":started"),
        "启动分支要保留（备份保留不等于跳过启动）"
    );
    assert!(
        script.contains("(goto) 2>NUL & del /F /Q \"%~f0\""),
        "自删要能让 cmd 先松手"
    );

    // 失败要能回滚，而不是留下一个没有 exe 的安装目录
    assert!(script.contains(":rollback"), "必须有回滚分支");

    // 脚本必须是纯 ASCII：中文注释/路径在 GBK 代码页下会乱码吞行。
    assert!(
        script.is_ascii(),
        "批处理脚本只能含 ASCII，否则中文系统上会乱码吞行"
    );
}

// ---------------------------------------------------------------------------
// 7. 没有下载通道
// ---------------------------------------------------------------------------

/// 两条通道都没有时，报的必须是「本机没有可用的下载通道」，且有下一步建议；
/// 不能是空指针 / panic / 空字节。
///
/// When neither channel exists the error must say "no usable download channel" and suggest a
/// next step; not a panic, not empty bytes.
#[test]
fn fetch_without_any_channel_says_so() {
    let none = |_: &str| None;
    let e = fetch_with("https://example.invalid/a.zip", &none).expect_err("没有通道必须报错");
    assert!(
        e.contains("本机没有可用的下载通道"),
        "错误要直说没有通道，实际：{e}"
    );
    // 空字节与「下载成功」无法区分，所以这条错误必须存在，且要指一条出路。
    assert!(e.contains("curl") && e.contains("powershell"), "实际：{e}");

    // 空地址先挡掉：否则会拿一个空 URL 去调系统工具
    let any = |_: &str| Some(std::path::PathBuf::from("curl.exe"));
    assert!(fetch_with("   ", &any).is_err(), "空地址必须报错");

    // 通道存在但起不来（路径是假的）→ 报「调用 curl 失败」，不是 panic
    let bogus = |_: &str| Some(std::path::PathBuf::from("aevum-no-such-downloader-xyz.exe"));
    let e = fetch_with("https://example.invalid/a.zip", &bogus).expect_err("起不来必须报错");
    assert!(e.contains("调用 curl 失败"), "实际：{e}");
}

// ---------------------------------------------------------------------------
// 8. 端到端：JSON → 资产 → 下载 → 校验 → 脚本
// ---------------------------------------------------------------------------

/// 四步串起来跑一遍：解析发布 → 挑到 windows-x64 → 假下载并校验 → 生成替换脚本。
/// 这是「更新器能不能用」的最小闭环，任何一环断掉这条都会红。
///
/// The four steps chained: parse the release, pick windows-x64, fake-download and verify,
/// then generate the replacement script. This is the smallest closed loop that answers "does
/// the updater work"; breaking any link turns it red.
#[test]
fn end_to_end_release_to_script() {
    const ZIP_URL: &str = "https://example.invalid/Aevum-Studio-v1.9.0-windows-x64.zip";
    const SUMS_URL: &str = "https://example.invalid/SHA256SUMS";

    let json = release_json(
        "v1.9.0",
        false,
        &[
            (
                "Aevum-Studio-v1.9.0-linux-x64.tar.gz",
                "https://example.invalid/l.tgz",
            ),
            (ASSET, ZIP_URL),
            ("SHA256SUMS", SUMS_URL),
        ],
    );

    let rel = parse_release(&json).expect("发布应当解析成功");
    let asset = pick_asset(&rel, "windows-x64.zip").expect("必须挑到 Windows 包");
    assert_eq!(asset.name, ASSET, "不能挑成 linux 包");

    let fetch = table_fetcher(vec![
        (ZIP_URL, PAYLOAD.to_vec()),
        (SUMS_URL, format!("{PAYLOAD_SHA}  {ASSET}\n").into_bytes()),
    ]);
    let bytes = download_verified(&fetch, asset, SUMS_URL).expect("校验必须通过");
    assert_eq!(bytes, PAYLOAD);

    // 下载到磁盘（临时目录），脚本里引用的是这个真实路径
    let dir = std::env::temp_dir().join(format!("aevum-update-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let staged = dir.join(format!("aevum-{}.exe", rel.tag));
    std::fs::write(&staged, &bytes).expect("写临时文件");
    assert_eq!(sha256_of(&bytes), PAYLOAD_SHA);

    let script = install_script();
    assert!(
        script.contains(r#"set "NEW=%~1""#),
        "新 exe 走参数进来（正文里写路径会被 OEM 代码页弄成乱码）"
    );
    assert!(
        !script.contains(&staged.display().to_string()),
        "下载路径不能写进脚本正文"
    );
    assert!(script.contains(":started"), "校验通过后才走到启动");

    // 失败的闭环：清单与发布不是同一次 → 必须在校验处停住
    let crossed = table_fetcher(vec![
        (ZIP_URL, PAYLOAD.to_vec()),
        (
            SUMS_URL,
            format!("{}  {ASSET}\n", "0".repeat(64)).into_bytes(),
        ),
    ]);
    assert!(
        download_verified(&crossed, asset, SUMS_URL).is_err(),
        "清单对不上时不能生成可安装的结论"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- 下载 / 解包 / 回滚

/// 解包判据用的 zip：顶层目录里同时放 `aaa.exe` 与 `aevum.exe`，子目录里放 `ffmpeg.exe`。
///
/// 三者都在，才能区分「按名字挑」和「挑第一个 exe」——只放一个的话，随便怎么实现都能绿。
///
/// The fixture zip used by the unpack criterion: `aaa.exe` and `aevum.exe` side by side in
/// the top directory, `ffmpeg.exe` in a subdirectory.
///
/// Having all three is what distinguishes "pick by name" from "pick the first exe"; with a
/// single file any implementation passes.
fn demo_zip() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/update-demo.zip")
}

fn no_program(_: &str) -> Option<std::path::PathBuf> {
    None
}

/// 真实查找器：只用来定位系统自带组件；找不到就让判据自己红，不静默跳过。
///
/// The real locator, used only to find OS-provided components; a miss turns the criterion
/// red rather than skipping it silently.
fn real_program(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = &["", ".exe"];
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 没有下载通道时必须**明确报错**，而且不能留下半截文件。
///
/// 静默成功是这里最贵的错：上层会以为拿到了一份完整的 zip，然后在解包或校验处报一个
/// 与真实原因无关的错，用户看到的错误永远指不到「这台机器装不了更新」。
///
/// With no download channel the call must **fail loudly** and leave no partial file behind.
///
/// Silent success is the expensive bug here: the caller would believe it holds a complete
/// zip and fail later at unpack or checksum with an unrelated message, so the user never
/// learns that this machine simply cannot install updates.
#[test]
fn download_without_a_channel_fails_and_leaves_no_partial_file() {
    let dir = std::env::temp_dir().join(format!("aevum-dl-nofail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let dest = dir.join("partial.bin");
    // 先放一份「上次没下完」的文件：调用必须把它清掉，不能接着写。
    // Seed a leftover partial file: the call must clear it, not append to it.
    std::fs::write(&dest, b"stale").expect("写残留文件");

    let asset = ReleaseAsset {
        name: "Aevum-Studio-v9.9.9-windows-x64.zip".to_string(),
        url: "https://example.invalid/x.zip".to_string(),
        size: 630,
    };
    let mut calls = 0usize;
    let err = aevum_core::update::download_to_file_with(&asset, &dest, &no_program, &mut |_, _| {
        calls += 1
    })
    .expect_err("没有通道时必须报错");
    assert!(
        err.contains("没有可用的下载通道"),
        "错误要说清是通道问题，而不是别的原因：{err}"
    );
    assert!(!dest.exists(), "半截/残留文件必须被清掉");
    assert_eq!(calls, 0, "通道都不存在时不该假装有进度");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 解包：从 zip 顶层目录里按名字挑出 `aevum.exe`，不挑 `aaa.exe`，也不挑 `tools/ffmpeg.exe`。
///
/// Unpack: pick `aevum.exe` by name from the top directory of the zip -- not `aaa.exe`, and
/// not `tools/ffmpeg.exe`.
#[test]
fn expand_picks_the_named_binary_not_the_first_one() {
    let work = std::env::temp_dir().join(format!("aevum-unpack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let exe = expand_exe_with(&demo_zip(), &work, "aevum.exe", &real_program)
        .expect("系统自带 tar 或 Expand-Archive 时应当能解包");
    assert_eq!(
        exe.file_name().map(|n| n.to_string_lossy().into_owned()),
        Some("aevum.exe".to_string()),
        "挑错了文件：{}",
        exe.display()
    );
    assert_eq!(
        std::fs::read(&exe).expect("解出的文件要可读"),
        b"MZ demo-main",
        "解出的必须是我们放进去的那一份"
    );
    // 目录要先清空：上一版的残留不能被当成「刚解出来的」。
    // The work dir is cleared first: a leftover from a previous run must not pass as fresh.
    let stale = work.join("Aevum-Studio-v9.9.9/aevum.exe");
    std::fs::write(&stale, b"stale").expect("写残留");
    let again = expand_exe_with(&demo_zip(), &work, "aevum.exe", &real_program).expect("再解一次");
    assert_eq!(std::fs::read(&again).expect("可读"), b"MZ demo-main");

    let _ = std::fs::remove_dir_all(&work);
}

/// 解包通道也没有时必须报错，而不是返回一个不存在的路径。
///
/// With no unpack channel either, fail instead of returning a path that does not exist.
#[test]
fn expand_without_a_channel_fails() {
    let work = std::env::temp_dir().join(format!("aevum-unpack-none-{}", std::process::id()));
    let err = expand_exe_with(&demo_zip(), &work, "aevum.exe", &no_program)
        .expect_err("没有通道时必须报错");
    assert!(err.contains("无法解包"), "错误要指到解包通道：{err}");
    let _ = std::fs::remove_dir_all(&work);
}

/// 回滚脚本：把当前版本挪成 `*.failed`、把 `*.old` 搬回来、再启动；没有备份时明确退出 1。
///
/// 路径同样走参数（`%~1` 目标 exe、`%~2` 进程名），正文纯 ASCII 且不含盘符。
///
/// The rollback script: move the current build to `*.failed`, bring `*.old` back, start it;
/// exit 1 with a message when there is no backup.
///
/// Paths arrive as arguments too (`%~1` target exe, `%~2` image name); the body is ASCII and
/// carries no drive path.
#[test]
fn rollback_script_restores_the_backup_and_never_leaves_no_exe() {
    let script = rollback_script();

    for needle in [
        r#"set "TARGET=%~1""#,
        r#"set "IMAGE=%~2""#,
        r#"if not exist "%TARGET%.old""#,
        r#"move /Y "%TARGET%" "%TARGET%.failed""#,
        r#"move /Y "%TARGET%.old" "%TARGET%""#,
        r#"start "" "%TARGET%""#,
        r#""%SystemRoot%\System32\tasklist.exe" /FI "IMAGENAME eq %IMAGE%""#,
    ] {
        assert!(script.contains(needle), "回滚脚本缺少：{needle}\n{script}");
    }
    assert!(
        !script.contains(":\\"),
        "正文里不能出现盘符路径（会被 OEM 代码页弄成乱码）\n{script}"
    );
    // 顺序：先等退出 → 再挪走当前版本 → 再搬回备份 → 最后启动。
    let pos = |needle: &str| {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("缺少 {needle}"))
    };
    assert!(pos(":wait") < pos(".failed"));
    assert!(pos(".failed") < pos(r#"move /Y "%TARGET%.old" "%TARGET%""#));
    assert!(pos(r#"start "" "%TARGET%""#) > pos(".old"));
    assert!(script.is_ascii(), "批处理只能含 ASCII");
    // 所有删除动作都只能指向脚本自己：失败的版本必须留在盘上供排查。
    // Every deletion must target the script itself: the failed build stays for diagnosis.
    for line in script.lines().filter(|l| l.contains("del /F /Q")) {
        assert!(
            line.contains(r#"del /F /Q "%~f0""#),
            "除自删之外不许有删除动作：{line}"
        );
    }
}

/// 安装目录里的 `%` 与 `"` 必须被拒绝：它们在批处理里无法安全转义。
///
/// `%` and `"` in the install directory must be rejected: they cannot be escaped safely.
#[test]
fn unescapable_install_dirs_are_rejected() {
    assert!(check_install_dir(Path::new(r"C:\Program Files\Aevum Studio\aevum.exe")).is_ok());
    assert!(check_install_dir(Path::new(r"C:\Users\张三\AppData\Local\aevum.exe")).is_ok());
    for bad in [r"C:\100%\aevum.exe", r#"C:\a"b\aevum.exe"#] {
        let err = check_install_dir(Path::new(bad)).expect_err("必须拒绝");
        assert!(
            err.contains("无法安全转义"),
            "{bad} 的拒绝理由要说清：{err}"
        );
    }
}
