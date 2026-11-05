//! README 五张截图的资产层与可复现记录判据（D-C1 / D-C2）。
//!
//! # 为什么是「读文件」而不是「再截一遍」
//!
//! 截图需要一个真的窗口（eframe），无显示器的 CI / Session 0 开不出来（台账 R4）。
//! 所以这里的判据只碰**资产与记录**：图在不在、够不够大、是不是合法 PNG、README 引用
//! 与文件是否一一对应、文档记的命令与脚本写的是否逐字一致。内容对不对（画面里是不是
//! 真照片、面板是不是展开在焦点合成组）是人工判据，归 `docs/README-截图.md` 的 checklist。
//!
//! # 为什么判据要同时咬住脚本与文档
//!
//! 只咬资产，改了脚本参数、文档没跟，下一个人照文档跑就截出另一组图；只咬文档，
//! 删了脚本没人知道。两边一起咬，`git status` 干净（samples/ 不新增文件）才有意义。
//!
//! # README screenshot criteria (D-C1 / D-C2)
//!
//! # Why "read files" instead of "shoot again"
//!
//! Shooting needs a real window (eframe), which a headless CI / Session 0 cannot open
//! (ledger item R4). So these criteria only touch **assets and records**: the images exist,
//! are large enough, are valid PNGs, README references match files one to one, and the
//! commands recorded in the docs match the script verbatim. Whether the *content* is right
//! (real photos, the focus group expanded) is a human criterion owned by the checklist in
//! `docs/README-截图.md`.
//!
//! # Why the criteria bite the script and the docs together
//!
//! Bite the assets alone and someone can change the script's arguments without the docs --
//! the next person following the docs shoots a different set. Bite the docs alone and a
//! deleted script goes unnoticed. Only with both does "git status stays clean (no new files
//! under samples/)" mean anything.

use std::path::{Path, PathBuf};

/// 仓库根（crates/aevum-ui 上两级）。
///
/// Repository root (two levels above crates/aevum-ui).
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .canonicalize()
        .expect("仓库根")
}

/// README 引用的五张图，与 `scripts/shots-readme.sh`、`docs/README-截图*.md` 三处必须
/// 逐字一致——改名字就是三处一起改，这里故意不做成「以某一方为准」。
///
/// The five shots referenced by the README; the script and the two docs must agree verbatim.
/// Renaming means changing all three at once -- deliberately not "one side is the truth".
const SHOTS: [&str; 5] = ["develop", "sequence", "deflicker", "compose", "render"];

/// staging 用的四张真实样张（`samples/cameras/` 里的真名）。进 staging 后三张配角
/// 改名排在 PANA180 之后（自然序保证它当第一帧），映射见 `scripts/shots-readme.sh`。
///
/// The four real samples used for staging (real names under `samples/cameras/`). Inside
/// staging the three companions are renamed to sort after PANA180 (natural order makes it
/// the first frame); the mapping lives in `scripts/shots-readme.sh`.
const SAMPLES: [&str; 4] = [
    "PANA180.RW2",
    "PANA1631.RW2",
    "PANA0450.RW2",
    "PANA0198.JPG",
];

/// D-C1：五张图都在、都够大、都是合法 PNG、尺寸达标、README 引用无断链。
///
/// 删掉任何一张、或者改了名没改 README，这里当场红。
///
/// D-C1: all five exist, are large enough, are valid PNGs, meet the size floor, and the
/// README references have no broken links.
///
/// Delete any one of them, or rename one without the README, and this goes red on the spot.
#[test]
fn readme_shots_exist_are_real_pngs_and_match_the_readme_links() {
    let root = root();
    for name in SHOTS {
        let path = root.join("assets/shots").join(format!("{name}.png"));
        let meta = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("缺 {name} 的截图（{path:?}）：{e}"));
        // 60KB 线照 scripts/shots.sh：一张只有底色的空图过不了脚本，也过不了这里。
        //
        // The 60 KB line follows scripts/shots.sh: a flat one-colour image passes neither
        // the script nor this.
        assert!(
            meta.len() > 60_000,
            "{name}.png 只有 {} 字节，疑似空白页",
            meta.len()
        );
        // PNG 魔数：脚本只查了字节数，这里补上「真的是一张 PNG」。
        //
        // PNG magic: the script only counts bytes, so this adds "it really is a PNG".
        let bytes = std::fs::read(&path).expect("读截图");
        assert!(
            bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            "{name}.png 不是合法 PNG"
        );
        let (w, h) = image::image_dimensions(&path).expect("读尺寸");
        assert!(
            w >= 1600 && h >= 900,
            "{name}.png 是 {w}×{h}，低于 1600×900 的下限"
        );
    }
    // 断链：README 与 README.en 里每一个 assets/shots/ 引用都必须真的存在，且五张图
    // 一张不落地全被引用——少一张就是「README 展示的界面不全」。
    //
    // Broken links: every assets/shots/ reference in README and README.en must exist, and
    // all five shots must be referenced -- a missing one means the README shows an incomplete UI.
    for readme in ["README.md", "README.en.md"] {
        let text = std::fs::read_to_string(root.join(readme))
            .unwrap_or_else(|e| panic!("读 {readme}：{e}"));
        let referenced: Vec<String> = text
            .match_indices("assets/shots/")
            .map(|(i, _)| {
                let rest = &text[i..];
                let end = rest
                    .find(|c: char| c.is_whitespace() || c == ')' || c == '"')
                    .unwrap_or(rest.len());
                rest[..end].to_owned()
            })
            .collect();
        assert!(!referenced.is_empty(), "{readme} 一张截图都没引用");
        for r in &referenced {
            assert!(root.join(r).is_file(), "{readme} 引用了不存在的截图：{r}");
        }
        for name in SHOTS {
            let want = format!("assets/shots/{name}.png");
            assert!(
                referenced.contains(&want),
                "{readme} 没引用 {want}，README 展示的界面不全"
            );
        }
    }
}

/// D-C2：文档记全五条的样张与命令，脚本与文档逐字一致，脚本对 `samples/` 只读。
///
/// 改了脚本不改文档（或反过来）→ 红；脚本往 `samples/` 里写 → 红。
///
/// D-C2: the docs record the samples and commands for all five, the script matches the docs
/// verbatim, and the script only reads `samples/`.
///
/// Change the script without the docs (or the other way round) → red; a script that writes
/// into `samples/` → red.
#[test]
fn the_shot_script_and_the_doc_agree_and_the_script_never_writes_samples() {
    let root = root();
    let script_path = root.join("scripts/shots-readme.sh");
    let script = std::fs::read_to_string(&script_path)
        .unwrap_or_else(|e| panic!("读脚本 {script_path:?}：{e}"));

    // 脚本里的五条 `shot <名> <参数…>`：`shot() {` 是函数定义，不算调用。
    //
    // The five `shot <name> <args…>` lines: `shot() {` is the function definition, not a call.
    let mut calls: Vec<(String, String)> = Vec::new();
    for line in script.lines() {
        let t = line.trim_start();
        if !t.starts_with("shot ") {
            continue;
        }
        let rest = t["shot ".len()..].trim();
        let mut it = rest.splitn(2, char::is_whitespace);
        let name = it.next().unwrap_or("").to_owned();
        let args = it.next().unwrap_or("").trim().to_owned();
        assert!(
            SHOTS.contains(&name.as_str()),
            "脚本里出现了清单之外的截图名：{name}"
        );
        calls.push((name, args));
    }
    assert_eq!(
        calls.len(),
        SHOTS.len(),
        "脚本该有恰好五条 shot 调用，实得 {calls:?}"
    );

    // staging 必须开在 target/ 下（gitignore 已盖住），否则 git status 会脏。
    //
    // Staging must live under target/ (already gitignored), otherwise git status gets dirty.
    assert!(
        script.contains("target/readme-shots"),
        "脚本的 staging 目录不在 target/ 下，git status 会被弄脏"
    );

    // 对 samples/ 只读：出现 samples/ 的每一行都不许是重定向目标，也不许是 cp 的目的地。
    //
    // samples/ is read-only: no line may redirect into it or copy into it.
    for line in script.lines() {
        assert!(
            !line.contains("> samples/") && !line.contains(">samples/"),
            "脚本往 samples/ 写东西：{line}"
        );
        assert!(
            !line.contains("samples/ \"$") && !line.contains("cp \"$src\" samples/"),
            "脚本把文件拷进 samples/：{line}"
        );
    }

    // 四张样张真实存在，且文档记录了它们。
    //
    // 但仓库用白名单把 `samples/cameras/*` 排除了（只放行两个 README，清单与校验值在
    // `samples/cameras/README.md`），所以干净的 clone 里一张都没有——那是素材本来就不
    // 入库，不是缺陷。因此分两种情形：
    //   · 一张都没有 → 跳过素材检查（脚本与文档的结构判据照跑，那部分与本机素材无关）；
    //   · 有但缺了一部分 → 本地素材被删或改名了，仍然红。
    // 一刀切地要求文件必须在，会让这条判据只能在开发机上成立，CI 上永远红。
    //
    // The four samples really exist, and the docs record them.
    //
    // The repo whitelists `samples/cameras/*` away (only the two READMEs are tracked; the
    // manifest and checksums live in `samples/cameras/README.md`), so a clean clone has none
    // of them -- the material is simply out of tree, which is not a defect. Two cases:
    //   * none present -> skip the material check (the script/doc structure checks above are
    //     independent of local material and still ran);
    //   * some present but not all -> the local material was deleted or renamed, still red.
    // Demanding the files unconditionally would make this criterion hold only on a dev
    // machine and stay red on CI forever.
    let missing: Vec<&str> = SAMPLES
        .iter()
        .filter(|s| !root.join("samples/cameras").join(s).is_file())
        .copied()
        .collect();
    let present = SAMPLES.len() - missing.len();
    if present == 0 {
        eprintln!(
            "跳过样张检查：samples/cameras/ 下一张样张都没有。仓库不跟踪它们（见 .gitignore \
             的白名单与 samples/cameras/README.md 的清单），截图脚本要真跑得按那份清单在本机 \
             放置素材。这条跳过只影响素材这一段，脚本与文档的结构判据已经跑过。"
        );
    } else {
        assert!(
            missing.is_empty(),
            "样张不全，缺 {missing:?}——本地素材被删或改名了"
        );
    }

    let doc =
        std::fs::read_to_string(root.join("docs/README-截图.md")).expect("读 docs/README-截图.md");
    let doc_en = std::fs::read_to_string(root.join("docs/README-截图.en.md"))
        .expect("读 docs/README-截图.en.md");
    for (name, args) in &calls {
        let path = format!("assets/shots/{name}.png");
        for (lang, text) in [("zh", &doc), ("en", &doc_en)] {
            assert!(text.contains(&path), "{lang} 文档没记 {path}");
            assert!(
                text.contains(args),
                "{lang} 文档记的命令与脚本不一致：脚本 `{args}`"
            );
        }
    }
    for s in SAMPLES {
        for (lang, text) in [("zh", &doc), ("en", &doc_en)] {
            assert!(text.contains(s), "{lang} 文档没记样张 {s}");
        }
    }
}
