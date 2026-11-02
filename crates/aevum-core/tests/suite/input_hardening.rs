//! 不可信输入的解析加固：解压炸弹与超限清单。
//!
//! # 为什么这两条要单独成判据
//!
//! 「坏文件不许崩」和「坏文件不许吃光内存」是两件事。`raw::guard` 管住了第一件
//! （panic 收成错误），这里管第二件：一张 200 字节的 TIFF 可以声明 100000×100000，
//! 一块合法的插件清单文件可以有一部电影的体积。**两者都不需要攻击者**——下载到一半、
//! 磁盘出坏道、作者脚本写错，都做得到。
//!
//! 判据的形式是「返回 `Err` 而不是 panic / 不把整个进程拖死」。这条断言是有牙的：
//! 谁把 `ImageReader` 的默认 `Limits` 换成 `no_limits()`，那 400 GB 的分配请求会让
//! 测试进程直接死在这儿（氧尽或分配器 abort），而不是悄悄通过。
//!
//! # Untrusted input hardening: decompression bombs and oversized manifests
//!
//! # Why these two deserve their own assertions
//!
//! "A bad file must not crash" and "a bad file must not eat all memory" are different
//! promises. `raw::guard` owns the first (a panic becomes an error); this file owns the
//! second: a 200-byte TIFF may declare 100000×100000, and a legitimate-looking plugin
//! manifest may be the size of a feature film. **Neither needs an attacker** -- a
//! half-finished download, a bad sector or a slip in an author's script all get there.
//!
//! The assertion takes the form "returns `Err` instead of panicking or dragging the whole
//! process down". It has teeth: whoever swaps `ImageReader`'s default `Limits` for
//! `no_limits()` gets a 400 GB allocation request that kills the test process right here
//! (out of memory or allocator abort) instead of passing quietly.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::metrics;
use aevum_core::plugin::{self, Registry, Source};

/// 临时目录，`Drop` 时删掉。测试产物落在 `tests/_tmp_hardening/`（`.gitignore` 覆盖）。
///
/// A temporary directory, removed on `Drop`. Test output goes to `tests/_tmp_hardening/`,
/// which `.gitignore` covers.
struct TmpDir {
    path: PathBuf,
}

impl TmpDir {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("时钟正常")
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/_tmp_hardening");
        let path = root.join(format!("{tag}_{}_{stamp}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self { path }
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::ops::Deref for TmpDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

/// 一张只有 110 字节、却声明 100000×100000 的 TIFF。
///
/// 结构是合法的最小 TIFF：II 头 + 一个 IFD + 八个必要条目，条带数据一个字都没有
/// （就算解到那一步也读不到东西）。真正的问题是宽高：`100000² × 3` 字节是 30 GB。
///
/// A 110-byte TIFF that declares 100000×100000.
///
/// The structure is a legal minimal TIFF: an `II` header, one IFD and the eight required
/// entries, with no strip data at all (so even reaching that step reads nothing). The real
/// problem is the dimensions: `100000² × 3` bytes is 30 GB.
fn declared_size_bomb(w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(128);
    b.extend_from_slice(b"II"); // 小端
    b.extend_from_slice(&42u16.to_le_bytes());
    b.extend_from_slice(&8u32.to_le_bytes()); // IFD0 在偏移 8
    let entries: [(u16, u16, u32); 8] = [
        (256, 4, w),       // ImageWidth  (LONG)
        (257, 4, h),       // ImageLength (LONG)
        (258, 3, 8),       // BitsPerSample (SHORT)
        (259, 3, 1),       // Compression = none
        (262, 3, 1),       // Photometric = BlackIsZero
        (273, 4, 200),     // StripOffsets（指向文件外，解不到那一步）
        (278, 4, h),       // RowsPerStrip
        (279, 4, 100_000), // StripByteCounts（自报的数，没人信）
    ];
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, ty, value) in entries {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&ty.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes()); // count
        // SHORT 值贴在偏移字段的低两字节上（小端），LONG 直接是值。
        b.extend_from_slice(&value.to_le_bytes());
    }
    b.extend_from_slice(&0u32.to_le_bytes()); // 没有下一个 IFD
    b
}

/// 声明尺寸的解压炸弹必须被拒，而不是被「尝试满足」。
///
/// 判据是「两个同构靶子的拒绝理由必须不同」：一个是 100000×100000（30 GB），另一个是
/// 1000×1（3 KB，远在各项上限之下）。后者会被读到「数据不够」这一层，前者必须在
/// **尺寸/内存上限**处就被拒。
///
/// 为什么比较理由而不是比较字符串：比字符串会把 image crate 的措辞焊进判据。
/// 比「两者不同」抓的是因果关系——把上限拿掉，100000×100000 也会掉到「数据不够」，
/// 两条理由就一样了，判据自然红。
///
/// 判据同时有牙：谁把 `Limits` 换成 `no_limits()`，这里的 30 GB 分配请求会让测试进程
/// 当场死掉（氧尽 / 分配器 abort），而不是悄悄通过。
///
/// A declared-size decompression bomb must be refused, not "attempted".
///
/// The assertion is "two isomorphic fixtures must be refused for different reasons": one at
/// 100000×100000 (30 GB) and one at 1000×1 (3 KB, far below every cap). The latter reaches
/// the "not enough data" layer; the former must be stopped at the **size/memory cap**.
///
/// Why compare reasons rather than strings: string comparison welds the image crate's wording
/// into the assertion. "The two differ" captures the causality -- remove the caps and
/// 100000×100000 also falls through to "not enough data", the reasons become equal and the
/// assertion goes red on its own.
///
/// The assertion also has teeth: whoever swaps `Limits` for `no_limits()` gets a 30 GB
/// allocation request that kills the test process right here (out of memory, or allocator
/// abort) instead of passing quietly.
#[test]
fn a_declared_size_bomb_is_refused_not_attempted() {
    let dir = TmpDir::new("bomb");
    let bomb = declared_size_bomb(100_000, 100_000);
    let tiny = declared_size_bomb(1000, 1);
    assert!(
        bomb.len() < 256 && tiny.len() < 256,
        "靶子要保持「小文件」这个前提，实际 {} / {} 字节",
        bomb.len(),
        tiny.len()
    );

    let bomb_path = dir.join("bomb.tif");
    std::fs::write(&bomb_path, &bomb).expect("写靶子");
    let tiny_path = dir.join("tiny.tif");
    std::fs::write(&tiny_path, &tiny).expect("写对照");

    let bomb_err = metrics::decode(&bomb_path)
        .expect_err("声明 100000×100000 的文件不该解出来")
        .to_string();
    let tiny_err = metrics::decode(&tiny_path)
        .expect_err("数据没写完，1000×1 也不该解出来")
        .to_string();
    assert_ne!(
        bomb_err, tiny_err,
        "两个靶子的拒绝理由一样（`{bomb_err}`），说明尺寸上限没起作用"
    );
    eprintln!("声明尺寸炸弹：{bomb_err}；对照 1000×1：{tiny_err}");
}

/// 超过体积上限的插件清单连读都不读，直接记一条错。
///
/// A plugin manifest over the size cap is not even read; it records one error instead.
#[test]
fn an_oversized_manifest_is_refused_by_size_not_parsed() {
    let dir = TmpDir::new("fat_manifest");
    let plugin_dir = dir.join("fat");
    std::fs::create_dir_all(&plugin_dir).expect("建插件目录");
    // 一份**语法完全合法**的清单，只是被灌到超过上限：这样「被拒」只可能来自体积，
    // 不可能来自语法。上限 +1 KB 即可越过，不必真写 1 MB 的垃圾。
    // A **perfectly legal** manifest, only padded past the cap, so "refused" can only come
    // from size and never from syntax. One kilobyte past the cap is enough.
    let cap = plugin::MAX_MANIFEST_BYTES as usize;
    let mut text = String::from(
        "[plugin]\nid = \"nik.fat\"\nname = \"Fat\"\nversion = \"1.0\"\nkind = \"filter\"\n\n[menu]\npath = \"plug/fat\"\nlabel = \"胖\"\n\n# ",
    );
    text.push_str(&"x".repeat(cap + 1024 - text.len()));
    std::fs::write(plugin_dir.join(plugin::MANIFEST), &text).expect("写清单");
    assert!(text.len() as u64 > plugin::MAX_MANIFEST_BYTES);

    let reg = Registry::scan(&dir, Source::User);
    assert!(reg.all().is_empty(), "超限清单不该被当成插件收进来");
    let errors = reg.errors();
    assert_eq!(errors.len(), 1, "应当恰好记一条错：{errors:?}");
    let (path, msg) = &errors[0];
    assert!(
        path.ends_with(plugin::MANIFEST),
        "错误要指到具体文件：{path:?}"
    );
    assert!(
        msg.contains("上限") && msg.contains(&cap.to_string()),
        "错误要说清是体积超限、并给出上限值：{msg}"
    );
}

/// 反向：正常大小的清单照旧入库，加固没有把功能一起关掉。
///
/// The other direction: a normal-sized manifest still loads, so the hardening did not switch
/// the feature off along with the risk.
#[test]
fn a_normal_manifest_still_loads_after_the_cap() {
    let dir = TmpDir::new("ok_manifest");
    let plugin_dir = dir.join("ok");
    std::fs::create_dir_all(&plugin_dir).expect("建插件目录");
    std::fs::write(
        plugin_dir.join(plugin::MANIFEST),
        "[plugin]\nid = \"nik.ok\"\nname = \"Ok\"\nversion = \"1.0\"\nkind = \"filter\"\n\n[menu]\npath = \"plug/ok\"\nlabel = \"好\"\n",
    )
    .expect("写清单");
    let reg = Registry::scan(&dir, Source::User);
    assert_eq!(reg.errors(), &[], "正常清单不该报错");
    assert_eq!(reg.all().len(), 1, "正常清单要被收进来");
}

/// 单边上限必须**卡在文档写的那个数上**，不能悄悄漂走。
///
/// 判据同样靠「理由必须不同」：`65536×1` 越过上限（65616 字节的像素数据），
/// `65535×1` 恰好还在限内——后者会一路读到「数据不够」，前者必须停在尺寸上。
/// 把常量改大，两条理由就一样了，判据自然红。**这不是在测 image crate**，是在测
/// 「我们传下去的那个数」还在文档说的位置。
///
/// The per-side cap must sit on **the number the docs claim**, and must not drift silently.
///
/// This assertion also leans on "the reasons must differ": `65536×1` is over the cap and
/// `65535×1` is exactly inside it, so the latter reads all the way to "not enough data" while
/// the former must stop at the size. Raise the constant and the two reasons become equal,
/// turning this red. **This is not testing the image crate**; it is testing that the number we
/// pass down still sits where the docs say.
#[test]
fn the_side_cap_sits_at_the_documented_number() {
    let dir = TmpDir::new("side_cap");
    let over = dir.join("over.tif");
    std::fs::write(&over, declared_size_bomb(65_536, 1)).expect("写越界靶子");
    let inside = dir.join("inside.tif");
    std::fs::write(&inside, declared_size_bomb(65_535, 1)).expect("写限内靶子");

    let over_err = metrics::decode(&over)
        .expect_err("65636 字节的声明不该放行")
        .to_string();
    let inside_err = metrics::decode(&inside)
        .expect_err("数据没写完，限内也不该解出来")
        .to_string();
    assert_ne!(
        over_err, inside_err,
        "65536 与 65535 给出同一个理由（`{over_err}`），单边上限不在 65535 上"
    );
}
