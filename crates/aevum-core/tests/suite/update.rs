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
    Release, ReleaseAsset, check_release, check_remote, download_verified, fetch_with,
    install_script, is_newer, parse_release, parse_version, pick_asset, sha256_of, verify,
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
/// 五步齐全、顺序正确、带空格的路径被引号包住。
///
/// A real replacement cannot run safely in a unit test (it would swap the test binary), so
/// the judgement asserts the **text**: all five steps, in order, with spaced paths quoted.
#[test]
fn install_script_has_five_steps_and_quotes_spaces() {
    let target = Path::new(r"C:\Program Files\Aevum Studio\aevum.exe");
    let new = Path::new(r"C:\Users\Some User\AppData\Local\Temp\aevum-1.9.0.exe");
    let script = install_script(new, target);

    // 带空格的路径必须整段被引号包住，否则 `move` 会把空格当成参数分隔符。
    for p in [
        r#""C:\Program Files\Aevum Studio\aevum.exe""#,
        r#""C:\Users\Some User\AppData\Local\Temp\aevum-1.9.0.exe""#,
    ] {
        assert!(script.contains(p), "路径没有被引号包住：{p}\n{script}");
    }

    // 五步的关键片段
    let steps: [(&str, &str); 5] = [
        ("等待退出", "tasklist /FI \"IMAGENAME eq aevum.exe\""),
        ("等待循环", "goto wait"),
        (
            "改名旧文件",
            r#"move /Y "C:\Program Files\Aevum Studio\aevum.exe" "C:\Program Files\Aevum Studio\aevum.exe.old""#,
        ),
        (
            "搬入新文件",
            r#"move /Y "C:\Users\Some User\AppData\Local\Temp\aevum-1.9.0.exe" "C:\Program Files\Aevum Studio\aevum.exe""#,
        ),
        (
            "启动新文件",
            r#"start "" "C:\Program Files\Aevum Studio\aevum.exe""#,
        ),
    ];
    for (what, needle) in steps {
        assert!(
            script.contains(needle),
            "缺少「{what}」这一步：{needle}\n{script}"
        );
    }

    // 顺序：等退出 → 改名 → 搬入 → 启动。顺序错了脚本照样能生成，但会在真实的
    // 更新里把正在运行的 exe 覆盖失败，所以必须在这里钉死。
    let pos = |needle: &str| {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("缺少 {needle}"))
    };
    assert!(pos(":wait") < pos(".old"));
    assert!(
        pos(r#"move /Y "C:\Users\Some User\AppData\Local\Temp\aevum-1.9.0.exe""#)
            < pos(r#"start "" "#)
    );

    // 清理与自删：`(goto)` 是让 cmd 释放脚本句柄的老写法，缺了它脚本永远删不掉自己。
    assert!(script.contains(r#"del /F /Q "C:\Program Files\Aevum Studio\aevum.exe.old""#));
    assert!(
        script.contains("(goto) 2>NUL & del /F /Q \"%~f0\""),
        "自删要能让 cmd 先松手"
    );

    // 失败要能回滚，而不是留下一个没有 exe 的安装目录
    assert!(script.contains(":rollback"), "必须有回滚分支");

    // 脚本必须是纯 ASCII：中文注释在 GBK 代码页下会吃字符。
    assert!(
        script.is_ascii(),
        "批处理脚本只能含 ASCII，否则中文系统上会乱码吞行"
    );

    // 不带空格的路径也不该被改坏（引号是幂等的）
    let plain = install_script(
        Path::new("D:\\tmp\\new.exe"),
        Path::new("D:\\app\\aevum.exe"),
    );
    assert!(plain.contains(r#""D:\app\aevum.exe""#));
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

    let script = install_script(&staged, &dir.join("aevum.exe"));
    assert!(
        script.contains(&format!("\"{}\"", staged.display())),
        "脚本里要引用刚下好的文件"
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
