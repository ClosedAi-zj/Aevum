//! Lightroom / Camera Raw XMP 预设读写。
//!
//! 只做数据转换，不写回 `.lrcat`。`crs:` 键与 `grade::GradeParams` 的映射
//! 全部在 [`crate::xmp_map`] 的数据表里（键规格、缩放、条件、特例规则），
//! 这里只负责 XML 解析与曲线子元素这两类表表达不了的形状。
//! 蒙版、色阶等 PV2012 没有对应键的字段读写时保持无操作。
//!
//! XML 不引第三方库，用一个只认元素/属性/文本的最小解析器：命名空间按 URI
//! 解析，所以 `crs:` 前缀叫什么、属性顺序如何都不影响读取。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::grade::GradeParams;
use crate::project::Metadata;
use crate::xmp_map::{self, CRS_NS};

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const DC_NS: &str = "http://purl.org/dc/elements/1.1/";

#[derive(Debug, Error)]
pub enum XmpError {
    #[error("读写 XMP 失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("XMP 里没有 Camera Raw 参数")]
    Empty,
    #[error("解析失败：{0}")]
    Parse(String),
}

// ---------------------------------------------------------------- 最小 XML 树

#[derive(Debug, Default)]
struct Node {
    ns: Option<String>,
    local: String,
    attrs: Vec<Attr>,
    kids: Vec<Node>,
    /// 直接子文本（已解码实体），曲线的 `rdf:li` 靠它取坐标。
    text: String,
}

#[derive(Debug)]
struct Attr {
    ns: Option<String>,
    local: String,
    value: String,
}

impl Node {
    fn is(&self, ns: &str, local: &str) -> bool {
        self.local == local && self.ns.as_deref() == Some(ns)
    }

    /// 这一段 Description 是否携带 Camera Raw 参数（属性或子元素形式）。
    fn has_crs(&self) -> bool {
        self.attrs.iter().any(|a| a.ns.as_deref() == Some(CRS_NS))
            || self
                .kids
                .iter()
                .any(|kid| kid.ns.as_deref() == Some(CRS_NS))
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn parse_err(msg: impl Into<String>) -> XmpError {
    XmpError::Parse(msg.into())
}

/// 把 `&amp; &#233; &#x1F600;` 之类的实体还原成文本；认不出的实体原样保留。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let decoded = after.find(';').and_then(|end| {
            let ent = &after[..end];
            if ent.is_empty()
                || ent.len() > 12
                || ent.contains('&')
                || ent.chars().any(char::is_whitespace)
            {
                return None;
            }
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    u32::from_str_radix(&ent[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            ch.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn resolve_name(raw: &str, nsmap: &HashMap<String, String>) -> (Option<String>, String) {
    match raw.split_once(':') {
        Some((prefix, local)) => (nsmap.get(prefix).cloned(), local.to_owned()),
        None => (
            nsmap.get("").cloned().filter(|uri| !uri.is_empty()),
            raw.to_owned(),
        ),
    }
}

fn attach(stack: &mut [(Node, HashMap<String, String>)], root: &mut Node, node: Node) {
    match stack.last_mut() {
        Some((parent, _)) => parent.kids.push(node),
        None => root.kids.push(node),
    }
}

/// 解析整份 XML。只保留元素、属性、文本，足够 XMP 使用。
fn parse_xml(src: &str) -> Result<Node, XmpError> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut root = Node {
        local: "#document".to_owned(),
        ..Node::default()
    };
    let mut stack: Vec<(Node, HashMap<String, String>)> = Vec::new();

    while i < b.len() {
        if b[i] != b'<' {
            let start = i;
            while i < b.len() && b[i] != b'<' {
                i += 1;
            }
            let text = decode_entities(&src[start..i]);
            if let Some((node, _)) = stack.last_mut() {
                node.text.push_str(&text);
            }
            continue;
        }

        if src[i..].starts_with("<!--") {
            match src[i..].find("-->") {
                Some(pos) => i += pos + 3,
                None => return Err(parse_err("注释没有闭合")),
            }
        } else if src[i..].starts_with("<![CDATA[") {
            i += 9;
            let end = src[i..]
                .find("]]>")
                .map(|pos| i + pos)
                .ok_or_else(|| parse_err("CDATA 没有闭合"))?;
            if let Some((node, _)) = stack.last_mut() {
                node.text.push_str(&src[i..end]);
            }
            i = end + 3;
        } else if src[i..].starts_with("<?") {
            match src[i..].find("?>") {
                Some(pos) => i += pos + 2,
                None => return Err(parse_err("处理指令没有闭合")),
            }
        } else if src[i..].starts_with("<!") {
            // DOCTYPE 之类：跳过，允许内部子集里的方括号。
            let mut j = i + 2;
            let mut depth = 0i32;
            while j < b.len() {
                match b[j] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    b'>' if depth <= 0 => {
                        j += 1;
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            i = j;
        } else if src[i..].starts_with("</") {
            let end = src[i..]
                .find('>')
                .map(|pos| i + pos)
                .ok_or_else(|| parse_err("结束标签没有闭合"))?;
            i = end + 1;
            let (node, _) = stack.pop().ok_or_else(|| parse_err("多余的结束标签"))?;
            attach(&mut stack, &mut root, node);
        } else {
            // 开始标签
            i += 1;
            let name_start = i;
            while i < b.len() && !is_ws(b[i]) && b[i] != b'>' && b[i] != b'/' {
                i += 1;
            }
            if i == name_start {
                return Err(parse_err("标签名为空"));
            }
            let raw_name = &src[name_start..i];

            let mut raw_attrs: Vec<(String, String)> = Vec::new();
            let mut self_close = false;
            loop {
                while i < b.len() && is_ws(b[i]) {
                    i += 1;
                }
                if i >= b.len() {
                    return Err(parse_err("标签没有闭合"));
                }
                if b[i] == b'/' {
                    self_close = true;
                    while i < b.len() && b[i] != b'>' {
                        i += 1;
                    }
                    if i < b.len() {
                        i += 1;
                    }
                    break;
                }
                if b[i] == b'>' {
                    i += 1;
                    break;
                }
                let attr_start = i;
                while i < b.len() && !is_ws(b[i]) && b[i] != b'=' && b[i] != b'>' && b[i] != b'/' {
                    i += 1;
                }
                if i == attr_start {
                    return Err(parse_err("属性名为空"));
                }
                let attr_name = &src[attr_start..i];
                while i < b.len() && is_ws(b[i]) {
                    i += 1;
                }
                if i >= b.len() || b[i] != b'=' {
                    return Err(parse_err("属性缺少 ="));
                }
                i += 1;
                while i < b.len() && is_ws(b[i]) {
                    i += 1;
                }
                if i >= b.len() || (b[i] != b'"' && b[i] != b'\'') {
                    return Err(parse_err("属性值缺少引号"));
                }
                let quote = b[i];
                i += 1;
                let value_start = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                if i >= b.len() {
                    return Err(parse_err("属性值没有闭合"));
                }
                let value = &src[value_start..i];
                i += 1;
                raw_attrs.push((attr_name.to_owned(), decode_entities(value)));
            }

            let mut nsmap = stack.last().map(|(_, map)| map.clone()).unwrap_or_default();
            for (name, value) in &raw_attrs {
                if name == "xmlns" {
                    nsmap.insert(String::new(), value.clone());
                } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                    nsmap.insert(prefix.to_owned(), value.clone());
                }
            }

            let (ns, local) = resolve_name(raw_name, &nsmap);
            let mut node = Node {
                ns,
                local,
                ..Node::default()
            };
            for (name, value) in raw_attrs {
                if name == "xmlns" || name.starts_with("xmlns:") {
                    continue;
                }
                let (attr_ns, attr_local) = match name.split_once(':') {
                    Some((prefix, local)) => (nsmap.get(prefix).cloned(), local.to_owned()),
                    None => (None, name),
                };
                node.attrs.push(Attr {
                    ns: attr_ns,
                    local: attr_local,
                    value,
                });
            }

            if self_close {
                attach(&mut stack, &mut root, node);
            } else {
                stack.push((node, nsmap));
            }
        }
    }

    if !stack.is_empty() {
        return Err(parse_err("元素没有闭合"));
    }
    Ok(root)
}

/// 收集所有 `rdf:Description`，同时记录它是否直接挂在 `rdf:RDF` 下。
fn collect_descriptions<'a>(node: &'a Node, under_rdf: bool, out: &mut Vec<(bool, &'a Node)>) {
    for kid in &node.kids {
        let is_desc =
            kid.local == "Description" && (kid.ns.is_none() || kid.ns.as_deref() == Some(RDF_NS));
        if is_desc {
            out.push((under_rdf, kid));
        }
        collect_descriptions(kid, kid.is(RDF_NS, "RDF"), out);
    }
}

fn find_descendant<'a>(node: &'a Node, local: &str) -> Option<&'a Node> {
    for kid in &node.kids {
        if kid.local == local && (kid.ns.as_deref() == Some(CRS_NS) || kid.ns.is_none()) {
            return Some(kid);
        }
        if let Some(found) = find_descendant(kid, local) {
            return Some(found);
        }
    }
    None
}

fn collect_li_text(node: &Node, out: &mut Vec<String>) {
    for kid in &node.kids {
        if kid.local == "li" && (kid.ns.as_deref() == Some(RDF_NS) || kid.ns.is_none()) {
            out.push(kid.text.clone());
        } else {
            collect_li_text(kid, out);
        }
    }
}

fn parse_curve(node: &Node) -> Option<Vec<[f64; 2]>> {
    let mut texts = Vec::new();
    collect_li_text(node, &mut texts);
    let mut pts = Vec::new();
    for text in texts {
        let Some((x, y)) = text.split_once(',') else {
            continue;
        };
        pts.push([
            round_half_even(clip(num(x), 0.0, 255.0)),
            round_half_even(clip(num(y), 0.0, 255.0)),
        ]);
    }
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut out: Vec<[f64; 2]> = Vec::new();
    for pt in pts {
        if let Some(last) = out.last_mut()
            && pt[0] == last[0]
        {
            *last = pt;
            continue;
        }
        out.push(pt);
    }
    (out.len() >= 2).then_some(out)
}

// ---------------------------------------------------------------- 数值

// `num`/`clip`/`round_half_even` 与映射表共用一份实现（`xmp_map`），曲线解析
// 还要用它们；导出的数字格式化也只有一个口径。

fn num(raw: &str) -> f64 {
    xmp_map::num(raw)
}

fn clip(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// Python 的 `round()` 是银行家舍入（.5 取偶），曲线的整数坐标和 `_fmt`
/// 都依赖它，这里保持一致。
fn round_half_even(v: f64) -> f64 {
    xmp_map::round_half_even(v)
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------- 读

/// 读一个 XMP 预设/旁车文件，缺失的键保持默认值。
///
/// 键到参数的换算全部在 `xmp_map::SPECS` 表里（见模块说明）；这里只负责
/// XML 解析、挑出带 crs 的 Description、把属性与无子节点的子元素并成一张
/// 键值表，然后交给表——外加两类表管不到的特例：
/// 1. **曲线**（`ToneCurvePV2012*`）：`rdf:Seq` 子元素，走 `parse_curve`；
/// 2. **未识别键**：表与包元数据没认领的 `crs:` 键收进 `params.xmp_extra`
///    原样保留，跨命名空间（dc:、photoshop: 等）不收。
pub fn read(path: &Path) -> Result<GradeParams, XmpError> {
    let bytes = std::fs::read(path)?;
    let text =
        String::from_utf8(bytes).map_err(|err| parse_err(format!("文件不是 UTF-8：{err}")))?;
    let root = parse_xml(&text)?;

    let mut descs: Vec<(bool, &Node)> = Vec::new();
    collect_descriptions(&root, false, &mut descs);

    // 优先外层 rdf:Description（Lightroom 预设就这一层）；相机旁车可能把
    // crs 参数放在第二个 Description 里，所以没有带 crs 的外层时退回全局搜索。
    let direct: Vec<&Node> = descs
        .iter()
        .filter(|(under_rdf, node)| *under_rdf && node.has_crs())
        .map(|(_, node)| *node)
        .collect();
    let chosen: Vec<&Node> = if direct.is_empty() {
        descs
            .iter()
            .filter(|(_, node)| node.has_crs())
            .map(|(_, node)| *node)
            .collect()
    } else {
        direct
    };
    if chosen.is_empty() {
        return Err(XmpError::Empty);
    }

    let mut attrs: HashMap<&str, &str> = HashMap::new();
    for desc in &chosen {
        for attr in &desc.attrs {
            if attr.ns.as_deref() == Some(CRS_NS) {
                attrs
                    .entry(attr.local.as_str())
                    .or_insert(attr.value.as_str());
            }
        }
    }
    // 相机旁车有时把参数写成子元素（RDF property element），也一并接受；
    // 曲线、蒙版、Name 等带子节点的元素跳过，它们另有处理或与参数无关。
    for desc in &chosen {
        for kid in &desc.kids {
            if kid.ns.as_deref() != Some(CRS_NS) || !kid.kids.is_empty() {
                continue;
            }
            let text = kid.text.trim();
            if !text.is_empty() {
                // 数值/布尔/WhiteBalance 之类的字符串都留着，读取时按需解析。
                attrs.entry(kid.local.as_str()).or_insert(text);
            }
        }
    }
    let get = |key: &str| attrs.get(key).copied();

    let mut p = GradeParams::default();

    // 标量映射全在表里：应用顺序 = 表序（优先级靠「后者覆盖」表达）。
    xmp_map::apply(&mut p, &get);

    // 表与包元数据没认领的 crs 键原样收进 xmp_extra：随参数走历史与工程落盘，
    // 导出时回写，往返不丢。
    for (key, value) in &attrs {
        if !xmp_map::has_spec(key) && !xmp_map::is_reserved(key) {
            p.xmp_extra.insert((*key).to_owned(), (*value).to_owned());
        }
    }

    // 曲线是子元素特例：`rdf:Seq` 里的 `rdf:li` 坐标对，表表达不了这种形状。
    let read_curve = |tag: &str| -> Option<Vec<[f64; 2]>> {
        chosen
            .iter()
            .find_map(|desc| find_descendant(desc, tag))
            .and_then(parse_curve)
    };
    if let Some(pts) = read_curve("ToneCurvePV2012") {
        p.curve_rgb = pts;
    }
    if let Some(pts) = read_curve("ToneCurvePV2012Red") {
        p.curve_r = pts;
    }
    if let Some(pts) = read_curve("ToneCurvePV2012Green") {
        p.curve_g = pts;
    }
    if let Some(pts) = read_curve("ToneCurvePV2012Blue") {
        p.curve_b = pts;
    }

    // 蒙版（MaskGroupBasedCorrections 等）属于另一套局部模型，这里不读。
    Ok(p)
}

// ---------------------------------------------------------------- 写

fn push_attr(attrs: &mut Vec<(String, String)>, key: &str, value: String) {
    attrs.push((format!("crs:{key}"), value));
}

/// 曲线点写成 Adobe 的整数坐标；不足两点时退回对角线。
fn curve_points(pts: &[[f64; 2]]) -> Vec<[i64; 2]> {
    let mut out: Vec<[i64; 2]> = pts
        .iter()
        .filter(|p| p[0].is_finite() && p[1].is_finite())
        .map(|p| {
            [
                round_half_even(clip(p[0], 0.0, 255.0)) as i64,
                round_half_even(clip(p[1], 0.0, 255.0)) as i64,
            ]
        })
        .collect();
    out.sort_by_key(|p| p[0]);
    let mut dedup: Vec<[i64; 2]> = Vec::new();
    for pt in out {
        if let Some(last) = dedup.last_mut()
            && pt[0] == last[0]
        {
            *last = pt;
            continue;
        }
        dedup.push(pt);
    }
    if dedup.len() < 2 {
        vec![[0, 0], [255, 255]]
    } else {
        dedup
    }
}

fn push_curve(xml: &mut String, tag: &str, pts: &[[f64; 2]]) {
    xml.push_str("   <crs:");
    xml.push_str(tag);
    xml.push_str(">\n    <rdf:Seq>\n");
    for [x, y] in curve_points(pts) {
        xml.push_str("     <rdf:li>");
        xml.push_str(&format!("{x}, {y}"));
        xml.push_str("</rdf:li>\n");
    }
    xml.push_str("    </rdf:Seq>\n   </crs:");
    xml.push_str(tag);
    xml.push_str(">\n");
}

/// 全部 `crs:` 参数属性（曲线与预设名是子元素，不在这里）。
/// 预设文件与照片旁车共用这一份序列化，两边的 `crs:` 口径必须一致。
///
/// 包元数据四件套在前；标量参数按 `xmp_map::SPECS` 表序反算；最后原样补写
/// `xmp_extra`（表键与包元数据优先，防双写）。
fn crs_attrs(params: &GradeParams) -> Vec<(String, String)> {
    let mut attrs: Vec<(String, String)> = Vec::new();

    push_attr(&mut attrs, "PresetType", "Normal".to_owned());
    push_attr(&mut attrs, "Version", "17.3.1".to_owned());
    push_attr(&mut attrs, "ProcessVersion", "15.4".to_owned());
    push_attr(&mut attrs, "HasSettings", "True".to_owned());

    attrs.extend(xmp_map::emit_attrs(params));

    for (key, value) in &params.xmp_extra {
        // 表内键已经按表写出、包元数据由固定头写出，都别再写一遍。
        if !xmp_map::has_spec(key) && !xmp_map::is_reserved(key) {
            push_attr(&mut attrs, key, value.clone());
        }
    }

    attrs
}

/// 曲线的四条子元素（RGB 与三个单通道）。
fn crs_curves(params: &GradeParams) -> String {
    let mut xml = String::new();
    push_curve(&mut xml, "ToneCurvePV2012", &params.curve_rgb);
    push_curve(&mut xml, "ToneCurvePV2012Red", &params.curve_r);
    push_curve(&mut xml, "ToneCurvePV2012Green", &params.curve_g);
    push_curve(&mut xml, "ToneCurvePV2012Blue", &params.curve_b);
    xml
}

/// XMP 包骨架：命名空间声明、属性行、`Description` 里的子元素。
/// 预设与照片旁车都从这里出，两者的差别只在命名空间与子元素。
fn packet(namespaces: &[(&str, &str)], attrs: &[(String, String)], body: &str) -> String {
    let mut xml = String::with_capacity(4096);
    xml.push_str("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    xml.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Aevum Studio\">\n");
    xml.push_str(" <rdf:RDF xmlns:rdf=\"");
    xml.push_str(RDF_NS);
    xml.push_str("\">\n");
    xml.push_str("  <rdf:Description rdf:about=\"\"\n");
    for (prefix, uri) in namespaces {
        xml.push_str("    xmlns:");
        xml.push_str(prefix);
        xml.push_str("=\"");
        xml.push_str(uri);
        xml.push_str("\"\n");
    }
    for (key, value) in attrs {
        xml.push_str("    ");
        xml.push_str(key);
        xml.push_str("=\"");
        xml.push_str(&escape_attr(value));
        xml.push_str("\"\n");
    }
    xml.push_str("  >\n");
    xml.push_str(body);
    xml.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
    xml.push_str("<?xpacket end=\"w\"?>\n");
    xml
}

/// 写一个只含 `crs:` 参数的 XMP 预设文件。
pub fn write(path: &Path, params: &GradeParams) -> Result<(), XmpError> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Aevum Preset".to_owned());

    let mut body = crs_curves(params);
    // 预设名给 Lightroom 的列表用；读入端不依赖它。
    body.push_str("   <crs:Name>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">");
    body.push_str(&escape_text(&name));
    body.push_str("</rdf:li>\n    </rdf:Alt>\n   </crs:Name>\n");

    let xml = packet(&[("crs", CRS_NS)], &crs_attrs(params), &body);
    std::fs::write(path, xml.as_bytes())?;
    Ok(())
}

/// 照片旁车的内容：都柏林核心的四项文档信息 + 这一张的 `crs:` 调色参数。
///
/// 空字段一个标签都不写，跟 EXIF 那边同口径——不往文件里塞空元素。
/// 与预设的差别只有 `dc:` 这一段，以及没有 `crs:Name`：旁车不是预设，
/// 不该出现在 Lightroom 的预设列表里。
pub fn sidecar_xml(params: &GradeParams, meta: &Metadata) -> String {
    let mut body = String::new();
    // `present()` 已经折叠过空白、截过上限、滤掉空项，键名就是都柏林核心的词，
    // 只有版权在 XMP 里叫 `dc:rights`（EXIF 那边叫 Copyright）。
    for (key, value) in meta.present() {
        let tag = if key == "copyright" { "rights" } else { key };
        if key == "creator" {
            push_seq(&mut body, tag, &value);
        } else {
            push_alt(&mut body, tag, &value);
        }
    }
    body.push_str(&crs_curves(params));
    packet(&[("crs", CRS_NS), ("dc", DC_NS)], &crs_attrs(params), &body)
}

/// `rdf:Alt` + `x-default`：标题、说明、版权这类语言备选值。
fn push_alt(xml: &mut String, tag: &str, value: &str) {
    xml.push_str("   <dc:");
    xml.push_str(tag);
    xml.push_str(">\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">");
    xml.push_str(&escape_text(value));
    xml.push_str("</rdf:li>\n    </rdf:Alt>\n   </dc:");
    xml.push_str(tag);
    xml.push_str(">\n");
}

/// `rdf:Seq`：创作者是有序列表（多位创作者按顺序排）。
fn push_seq(xml: &mut String, tag: &str, value: &str) {
    xml.push_str("   <dc:");
    xml.push_str(tag);
    xml.push_str(">\n    <rdf:Seq>\n     <rdf:li>");
    xml.push_str(&escape_text(value));
    xml.push_str("</rdf:li>\n    </rdf:Seq>\n   </dc:");
    xml.push_str(tag);
    xml.push_str(">\n");
}

/// 把旁车写到图片旁边（同目录、同名、`.xmp`），返回落盘路径。
pub fn write_sidecar(
    image: &Path,
    params: &GradeParams,
    meta: &Metadata,
) -> Result<PathBuf, XmpError> {
    let path = sidecar_for(image);
    // 和静帧同一套写法：先落临时名再改名占用。断点续跑会把「旁车在盘上」当成这一帧
    // 导完了一半判据，直接覆盖写的话被杀在中途留下半个 XML，之后每次续跑都跳过它。
    let tmp = {
        let name = path
            .file_name()
            .map(|n| format!(".{}.part", n.to_string_lossy()))
            .unwrap_or_else(|| ".aevum-xmp-part".to_owned());
        path.with_file_name(name)
    };
    match std::fs::write(&tmp, sidecar_xml(params, meta).as_bytes()) {
        Ok(()) => std::fs::rename(&tmp, &path).map_err(XmpError::Io)?,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(XmpError::Io(e));
        }
    }
    Ok(path)
}

/// 图片对应的旁车路径：同目录、同文件名、`.xmp` 后缀。
pub fn sidecar_for(image: &Path) -> PathBuf {
    image.with_extension("xmp")
}
