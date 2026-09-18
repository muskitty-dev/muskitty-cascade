//! 文本相关属性的**使用值**语义（line-height / text-transform / white-space）。
//!
//! 这些属性的使用值必须被 layout（文本测量：决定换行与盒高）与 renderer
//! （字形绘制：决定内容与行位置）**逐字节一致**地计算出来——不一致会导致
//! 溢出或错位（T-3 曾因测量高度公式与绘制基线不一致产生"汉字纵向位移"）。
//! 故语义实现放在 cascade（两侧共同依赖的样式层），作为单一来源。
//!
//! # 规范依据
//!
//! - CSS Inline Layout Level 3 §4.2 `line-height`
//!   （`normal | <number> | <length-percentage>`；`<number>` 的计算值仍是数，
//!   作为**自身** font-size 的倍数参与使用值计算并被继承；`<percentage>`
//!   按自身 font-size 解析为长度）
//! - CSS Text Level 3 §2.1 `text-transform`
//!   （`none | capitalize | uppercase | lowercase`；转换在布局前生效，
//!   使用语言无关的全尺寸映射）
//! - CSS Text Level 3 §4 `white-space`（`normal | pre | nowrap | pre-wrap |
//!   break-spaces | pre-line`；§4.1.2 White Space Processing Rules Phase I
//!   的折叠/保留语义，§4.1.3 Segment Break Transformation Rules 的换行符
//!   语义——M-3 batch 3c）

use crate::style::ComputedStyle;
use muskitty_css::parser::ComponentValue;
use muskitty_css::tokenizer::Token;
use std::borrow::Cow;

/// `line-height: normal` 的 UA 相关取值（font-size 的倍数）。
///
/// CSS Inline L3 §4.2 把 `normal` 定义为"UA 相关，通常约 1.2"；
/// Chrome/Firefox 对本实现默认字体族接近该值。取 1.2 与旧实现
/// （`font_size * 1.2` 硬编码近似）保持视觉兼容。
pub const NORMAL_LINE_HEIGHT: f32 = 1.2;

/// 行高上限（px）：2^25，与 layout `style_map::MAX_LENGTH_PX` 同级量级。
///
/// 敌意 CSS 数值（`1e999px` → inf）不得以非有限值进入 cosmic-text
/// `Metrics`（审计 F-1 同类通路）。
const MAX_LINE_HEIGHT_PX: f32 = 33_554_432.0;

/// `line-height` 的使用值（px）。
///
/// 解析顺序（CSS Inline L3 §4.2）：
/// - px 长度 → 直接使用（`+inf`/超界钳制到 [`MAX_LINE_HEIGHT_PX`]）；
/// - 数 → `n × font_size`（倍数语义，随值继承，由各元素自身 font-size 折算）；
/// - 百分比 → `p% × font_size`（正常情况下已在计算值阶段归一化为 px，
///   见 `style_tree::normalize_line_height_percentage`，此处为防御性处理）；
/// - `normal` / 缺失 / 未知关键字 → [`NORMAL_LINE_HEIGHT`] × font_size；
/// - 负值 / NaN → 非法（规范上负 `line-height` 无效）→ 回退 `normal`。
///
/// `0` 是**合法**行高（§4.2 允许 0；行距为零、行重叠正是规范语义），原样返回；
/// cosmic-text 自行保证行盒不小于字形 ascent+descent。
///
/// `font_size` 由调用方给出（元素自身 font-size 的 px 使用值）。
pub fn used_line_height_px(style: &ComputedStyle, font_size: f32) -> f32 {
    let normal = || clamp_line_height(NORMAL_LINE_HEIGHT as f64 * font_size as f64);
    let Some(cv) = style.get("line-height") else {
        return normal();
    };
    for token in cv.tokens() {
        let value_px = match token {
            ComponentValue::PreservedToken(Token::Dimension(numeric, unit))
                if unit.eq_ignore_ascii_case("px") =>
            {
                Some(numeric.value)
            }
            ComponentValue::PreservedToken(Token::Number(numeric)) => {
                Some(numeric.value * font_size as f64)
            }
            ComponentValue::PreservedToken(Token::Percentage(numeric)) => {
                Some(numeric.value / 100.0 * font_size as f64)
            }
            // `normal` 及未知关键字
            ComponentValue::PreservedToken(Token::Ident(_)) => return normal(),
            _ => None,
        };
        if let Some(v) = value_px {
            if v.is_nan() || v < 0.0 {
                return normal();
            }
            return clamp_line_height(v);
        }
    }
    normal()
}

/// 行高数值钳制：NaN → 0（调用方据此判定非法），`±inf`/超界 → `[0, 上限]`。
fn clamp_line_height(v: f64) -> f32 {
    let v = v as f32; // f64 1e39 之类 → f32 inf，随后被 clamp 收进上限
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, MAX_LINE_HEIGHT_PX)
    }
}

/// 按 `text-transform` 改写文本（CSS Text L3 §2.1）。
///
/// 支持 `uppercase` / `lowercase` / `capitalize`；`none`、缺失、未知关键字
/// （含未实现的 `full-width` / `full-size-kana`）→ 原样借用。
///
/// 映射使用 Rust 的 `to_uppercase` / `to_lowercase`（Unicode 全尺寸映射，
/// 语言无关，与规范对 `text-transform` 的要求一致；`ß` → `SS`）。
/// `capitalize`：每个"词"（空白分隔）的首字符大写——规范 §2.1.1 对词边界与
/// 语言相关特例的完整定义依赖文本分段（UAX #29），此处按空白边界近似。
pub fn apply_text_transform<'a>(text: &'a str, keyword: Option<&str>) -> Cow<'a, str> {
    match keyword {
        Some(kw) if kw.eq_ignore_ascii_case("uppercase") => Cow::Owned(text.to_uppercase()),
        Some(kw) if kw.eq_ignore_ascii_case("lowercase") => Cow::Owned(text.to_lowercase()),
        Some(kw) if kw.eq_ignore_ascii_case("capitalize") => {
            let mut out = String::with_capacity(text.len());
            let mut at_word_start = true;
            for ch in text.chars() {
                if ch.is_whitespace() {
                    at_word_start = true;
                    out.push(ch);
                } else if at_word_start {
                    at_word_start = false;
                    out.extend(ch.to_uppercase());
                } else {
                    out.push(ch);
                }
            }
            if out == text {
                Cow::Borrowed(text)
            } else {
                Cow::Owned(out)
            }
        }
        _ => Cow::Borrowed(text),
    }
}

/// 从 [`ComputedStyle`] 取 `text-transform` 关键字（缺失 → `None`）。
pub fn text_transform_keyword(style: &ComputedStyle) -> Option<&str> {
    style.get("text-transform").and_then(|cv| cv.keyword())
}

// ── M-3 batch 3c: white-space ──────────────────────────────────────

/// `white-space` 的语义模型（CSS Text L3 §4 表格，M-3 batch 3c）。
///
/// 从关键字派生三个正交行为位，layout 测量与 renderer 绘制共用同一派生
/// （单一来源）：**折叠**（Phase I）、**保留换行**（segment break → 强制
/// 换行）、**允许软换行**（line breaking 在软换行点折行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhiteSpace {
    /// 空白折叠生效（space/tab 序列合一、按规则消失）。
    pub collapse: bool,
    /// 源文本换行符（segment break）保留为强制换行。
    pub preserve_newlines: bool,
    /// 允许在软换行点换行（否则单行溢出）。
    pub wrap: bool,
}

impl WhiteSpace {
    /// `white-space` 关键字 → 语义模型（CSS Text L3 §4 汇总表）。
    ///
    /// | 关键字 | New Lines | Spaces/Tabs | Wrapping |
    /// |--------|-----------|-------------|----------|
    /// | normal / 未知 | Collapse | Collapse | Wrap |
    /// | pre | Preserve | Preserve | No wrap |
    /// | nowrap | Collapse | Collapse | No wrap |
    /// | pre-wrap | Preserve | Preserve | Wrap |
    /// | break-spaces | Preserve | Preserve | Wrap |
    /// | pre-line | Preserve | Collapse | Wrap |
    ///
    /// 缺失 / 未知关键字按 `normal` 处理（§4 声明块外的非法值在 cascade
    /// 声明过滤阶段已回退 initial；此处防御性兜底）。
    pub fn from_keyword(keyword: Option<&str>) -> Self {
        match keyword {
            Some(kw) if kw.eq_ignore_ascii_case("pre") => Self {
                collapse: false,
                preserve_newlines: true,
                wrap: false,
            },
            Some(kw) if kw.eq_ignore_ascii_case("nowrap") => Self {
                collapse: true,
                preserve_newlines: false,
                wrap: false,
            },
            Some(kw) if kw.eq_ignore_ascii_case("pre-wrap") => Self {
                collapse: false,
                preserve_newlines: true,
                wrap: true,
            },
            // break-spaces：cosmic-text 0.13 无"每个空格后软换行点"API，
            // 按 pre-wrap 近似（换行机会密度差异，偏差已记录于计划文档）。
            Some(kw) if kw.eq_ignore_ascii_case("break-spaces") => Self {
                collapse: false,
                preserve_newlines: true,
                wrap: true,
            },
            Some(kw) if kw.eq_ignore_ascii_case("pre-line") => Self {
                collapse: true,
                preserve_newlines: true,
                wrap: true,
            },
            // normal / none / 缺失 / 未知关键字。
            _ => Self {
                collapse: true,
                preserve_newlines: false,
                wrap: true,
            },
        }
    }

    /// 该语义下是否保留**所有**空白字符（`pre`/`pre-wrap`/`break-spaces`）。
    fn preserve_spaces(self) -> bool {
        !self.collapse
    }
}

/// 从 [`ComputedStyle`] 取 `white-space` 关键字（缺失 → `None`，按 normal）。
pub fn white_space_keyword(style: &ComputedStyle) -> Option<&str> {
    style.get("white-space").and_then(|cv| cv.keyword())
}

/// 对单字符做 segment break 变换的 CJK 判定（CSS Text L3 §4.1.3）。
///
/// `Fullwidth`/`Wide`/`Halfwidth`（非 Hangul）两侧的 segment break 应删除
/// 而非转空格——中文/日文源码换行不引入空隙。Hangul 需要词分隔（排除）。
/// `Ambiguous` 宽度（§、° 等）不算；零宽空格/标点集合的完整表不做。
fn is_cjk_break_dropping(c: char) -> bool {
    let u = c as u32;
    // Hangul syllables + Jamo：词分隔文字，两侧换行保留空格（排除）。
    if (0x1100..=0x11FF).contains(&u) || (0xAC00..=0xD7A3).contains(&u) {
        return false;
    }
    // CJK 部首扩展 / 康熙部首 / 注音 / CJK 统一表意（含扩展 A / 兼容）。
    (0x2E80..=0x312F).contains(&u)      // 部首 + CJK 部首 + 注音
        || (0x3190..=0x322F).contains(&u)   // 象形会意 + 部首序号
        || (0x3248..=0x4DBF).contains(&u)   // 十六进制符号 + CJK 扩展 A
        || (0x4E00..=0x9FFF).contains(&u)   // CJK 统一表意文字
        || (0xA000..=0xA4CF).contains(&u)   // 彝文（Wide）
        || (0xF900..=0xFAFF).contains(&u)   // CJK 兼容表意
        || (0xFF00..=0xFF60).contains(&u) // 全角形式（Fullwidth/Halfwidth）
}

/// 白空间折叠（CSS Text L3 §4.1.2 Phase I，单文本节点近似）。
///
/// 输入应为**已应用 text-transform** 的文本（§4 Order：transform 在
/// Phase I 之后 Phase II 之前，但两者对空白均以转换后文本为准——本实现
/// 约定调用方先 transform 后折叠，与 layout/renderer 两侧一致）。
///
/// 每种 `white-space` 取值的处理：
/// - `normal`/`nowrap`：space/tab 序列折叠为单个空格；源换行符（segment
///   break）两侧按 §4.1.3——CJK 全宽字符相邻时删除，否则转空格；
///   换行符前后的空白随折叠一并消失。
/// - `pre-line`：同折叠规则，但换行符保留为强制换行（U+000A 原样输出）。
/// - `pre`/`pre-wrap`/`break-spaces`：全部空白原样保留（§4.1.2 第二条）。
///
/// 已知偏差（记录于计划文档）：跨文本节点的 IFC 级折叠不做（每个 Text
/// 节点独立折叠，节点首尾空白独立裁剪）；`full-width` 空格不折叠。
pub fn apply_white_space(text: &str, ws: WhiteSpace) -> Cow<'_, str> {
    if ws.preserve_spaces() {
        // pre / pre-wrap / break-spaces：空白全保留（换行符亦保留）。
        return Cow::Borrowed(text);
    }

    // normal / nowrap / pre-line 的折叠路径。
    // 快速通道：不含可折叠空白字符的文本原样借用（真实页面大多数文本节点）。
    if !text.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '\r')) {
        return Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    // 折叠中的空白序列状态（Phase I 单遍扫描近似）：
    // - `pending_space`：序列中已见空白字符；
    // - `pending_break`：序列中含 segment break（normal/nowrap 语义）；
    // - `after_break`（pre-line）：刚输出强制换行，紧随其后的空白按规范移除；
    // - `emitted_any`：本节点尚无字符输出 → 行首空白移除。
    let mut pending_space = false;
    let mut pending_break = false;
    let mut after_break = false;
    let mut emitted_any = false;
    // 空白序列前的字符（§4.1.3 segment break 判定用）。
    let mut prev_cjk: Option<char> = None;

    for ch in text.chars() {
        match ch {
            ' ' | '\t' => {
                pending_space = true;
            }
            '\n' if ws.preserve_newlines => {
                // pre-line：换行符保留为强制换行（§4.1.3 第一条）；紧邻其前
                // 的空白序列被移除（Phase I 第 1 步）。
                pending_space = false;
                pending_break = false;
                out.push('\n');
                after_break = true;
                prev_cjk = None;
                emitted_any = true;
            }
            '\n' => {
                // normal / nowrap：换行符并入空白序列（折叠语义）。
                pending_space = true;
                pending_break = true;
            }
            _ => {
                if pending_space {
                    if after_break {
                        // Phase I 第 1 步：紧随 segment break 的空白被移除。
                    } else if pending_break {
                        // §4.1.3：换行符按两侧字符判定——CJK 全宽相邻删除，
                        // 否则转空格。
                        let drop = prev_cjk.is_some_and(is_cjk_break_dropping)
                            && is_cjk_break_dropping(ch);
                        if !drop {
                            out.push(' ');
                        }
                    } else if emitted_any {
                        out.push(' ');
                    }
                    pending_space = false;
                    pending_break = false;
                }
                out.push(ch);
                after_break = false;
                prev_cjk = Some(ch);
                emitted_any = true;
            }
        }
    }
    // 尾部空白：行尾空白按 §4 表格移除（normal/nowrap/pre-line 的
    // end-of-line spaces = Remove），无需处理。

    if out.is_empty() {
        Cow::Borrowed("")
    } else {
        Cow::Owned(out)
    }
}

#[cfg(test)]
mod ws_tests {
    use super::*;

    fn kw(s: &str) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Ident(s.to_string()))
    }

    fn ws_style(value: Option<&str>) -> ComputedStyle {
        let mut cs = ComputedStyle::new();
        if let Some(v) = value {
            cs.set(
                "white-space",
                crate::ComputedValue::from_tokens(vec![kw(v)]),
            );
        }
        cs
    }

    #[test]
    fn white_space_keyword_reads_computed_style() {
        assert_eq!(white_space_keyword(&ws_style(Some("pre"))), Some("pre"));
        assert_eq!(white_space_keyword(&ws_style(None)), None);
    }

    // —— WhiteSpace 模型（§4 汇总表）——

    #[test]
    fn white_space_model_matches_spec_table() {
        // §4 表格：New Lines / Spaces and Tabs / Text Wrapping 三列。
        let cases = [
            ("normal", (true, false, true)),
            ("pre", (false, true, false)),
            ("nowrap", (true, false, false)),
            ("pre-wrap", (false, true, true)),
            ("break-spaces", (false, true, true)),
            ("pre-line", (true, true, true)),
        ];
        for (kw, (collapse, preserve_newlines, wrap)) in cases {
            let ws = WhiteSpace::from_keyword(Some(kw));
            assert_eq!(
                (ws.collapse, ws.preserve_newlines, ws.wrap),
                (collapse, preserve_newlines, wrap),
                "keyword {kw} model mismatch"
            );
        }
        // 缺失与未知关键字按 normal。
        for absent in [None, Some(""), Some("bogus")] {
            let ws = WhiteSpace::from_keyword(absent);
            assert_eq!(
                (ws.collapse, ws.preserve_newlines, ws.wrap),
                (true, false, true),
                "absent/unknown must behave as normal"
            );
        }
    }

    // —— normal / nowrap：折叠矩阵 ——

    #[test]
    fn collapse_runs_of_spaces_and_tabs() {
        let ws = WhiteSpace::from_keyword(Some("normal"));
        assert_eq!(apply_white_space("a  \t b", ws), "a b");
        // 单一 tab 亦折叠为空格。
        assert_eq!(apply_white_space("a\tb", ws), "a b");
    }

    #[test]
    fn segment_break_becomes_space_between_latin() {
        let ws = WhiteSpace::from_keyword(Some("normal"));
        // 拉丁两侧的源码换行 → 空格（英文"unbreak"语义）。
        assert_eq!(apply_white_space("foo\nbar", ws), "foo bar");
        // 换行两侧的缩进空白一并消失。
        assert_eq!(apply_white_space("foo  \n\t  bar", ws), "foo bar");
    }

    #[test]
    fn segment_break_dropped_between_cjk() {
        let ws = WhiteSpace::from_keyword(Some("normal"));
        // CJK 两侧的换行符删除（中文源码换行不留空隙）。
        assert_eq!(apply_white_space("這個\n段落", ws), "這個段落");
        // 一侧 CJK 一侧拉丁 → 转空格（无法确定无词分隔）。
        assert_eq!(apply_white_space("這個foo\nbar", ws), "這個foo bar");
        assert_eq!(apply_white_space("foo\n這個", ws), "foo 這個");
    }

    #[test]
    fn leading_trailing_whitespace_removed_when_collapsing() {
        let ws = WhiteSpace::from_keyword(Some("normal"));
        assert_eq!(apply_white_space("   leading", ws), "leading");
        assert_eq!(apply_white_space("trailing   ", ws), "trailing");
        assert_eq!(apply_white_space("  \n\t  ", ws), "");
    }

    #[test]
    fn nowrap_collapses_same_as_normal() {
        let ws = WhiteSpace::from_keyword(Some("nowrap"));
        assert_eq!(apply_white_space("a  \n\t b", ws), "a b");
    }

    // —— pre-line：折叠空白但保留换行 ——

    #[test]
    fn pre_line_preserves_newlines_but_collapses_spaces() {
        let ws = WhiteSpace::from_keyword(Some("pre-line"));
        assert_eq!(apply_white_space("foo  \n  bar", ws), "foo\nbar");
        assert_eq!(apply_white_space("a  b", ws), "a b");
        // 连续换行保留为多个强制换行。
        assert_eq!(apply_white_space("a\n\nb", ws), "a\n\nb");
        // CJK 侧的换行在 pre-line 同样保留（Preserve，§4 表格）。
        assert_eq!(apply_white_space("這個\n段落", ws), "這個\n段落");
    }

    // —— pre / pre-wrap / break-spaces：全部保留 ——

    #[test]
    fn preserve_values_keep_everything() {
        for kw in ["pre", "pre-wrap", "break-spaces"] {
            let ws = WhiteSpace::from_keyword(Some(kw));
            let src = "  a \t\n b  ";
            let out = apply_white_space(src, ws);
            assert_eq!(out, src, "keyword {kw} must preserve all whitespace");
            // 保留路径零拷贝。
            assert!(matches!(out, Cow::Borrowed(_)), "{kw} should borrow");
        }
    }

    #[test]
    fn collapse_borrows_when_nothing_to_collapse() {
        // 实现注记：无空白文本走快速通道原样借用；含空白的折叠路径
        // （词间单空格也需输出）必然分配。
        let ws = WhiteSpace::from_keyword(Some("normal"));
        let out = apply_white_space("cleantext", ws);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "cleantext");
        // 单空格折叠后仍保有一个空格（语义不变但需分配）。
        assert_eq!(apply_white_space("clean text", ws), "clean text");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muskitty_css::tokenizer::Numeric;

    fn px(v: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Dimension(
            Numeric {
                value: v,
                is_integer: false,
                has_sign: false,
            },
            "px".to_string(),
        ))
    }

    fn num(v: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Number(Numeric {
            value: v,
            is_integer: false,
            has_sign: false,
        }))
    }

    fn pct(v: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Percentage(Numeric {
            value: v,
            is_integer: false,
            has_sign: false,
        }))
    }

    fn kw(s: &str) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Ident(s.to_string()))
    }

    fn style_with(value: ComponentValue) -> ComputedStyle {
        let mut cs = ComputedStyle::new();
        cs.set(
            "line-height",
            crate::ComputedValue::from_tokens(vec![value]),
        );
        cs
    }

    #[test]
    fn line_height_px_is_used_as_is() {
        let cs = style_with(px(40.0));
        assert_eq!(used_line_height_px(&cs, 16.0), 40.0);
    }

    #[test]
    fn line_height_number_multiplies_own_font_size() {
        let cs = style_with(num(2.0));
        assert_eq!(used_line_height_px(&cs, 16.0), 32.0);
        // 同一倍数在不同 font-size 的元素上给出不同行高（数继承语义）
        assert_eq!(used_line_height_px(&cs, 32.0), 64.0);
    }

    #[test]
    fn line_height_percentage_resolves_against_font_size() {
        let cs = style_with(pct(150.0));
        assert_eq!(used_line_height_px(&cs, 16.0), 24.0);
    }

    #[test]
    fn line_height_normal_missing_and_unknown_fall_back() {
        assert_eq!(
            used_line_height_px(&style_with(kw("normal")), 16.0),
            1.2 * 16.0
        );
        assert_eq!(used_line_height_px(&ComputedStyle::new(), 20.0), 1.2 * 20.0);
        assert_eq!(
            used_line_height_px(&style_with(kw("bogus")), 10.0),
            1.2 * 10.0
        );
    }

    #[test]
    fn line_height_invalid_values_fall_back_to_normal() {
        // 负值 / NaN 非法（规范：负 line-height 无效）→ normal
        for bad in [num(-1.5), px(-3.0), num(f64::NAN), px(f64::NAN)] {
            let cs = style_with(bad);
            let lh = used_line_height_px(&cs, 16.0);
            assert!(lh.is_finite() && lh > 0.0, "unexpected line-height {lh}");
            assert_eq!(lh, 1.2 * 16.0);
        }
    }

    #[test]
    fn line_height_zero_is_valid() {
        // §4.2: 0 是合法行高（行重叠正是规范语义），不当作非法值回退
        assert_eq!(used_line_height_px(&style_with(px(0.0)), 16.0), 0.0);
        assert_eq!(used_line_height_px(&style_with(num(0.0)), 16.0), 0.0);
    }

    #[test]
    fn line_height_huge_value_is_clamped() {
        // 1e39（f64 有限，as f32 → inf）与 tokenizer 对 `1e999` 产出的 inf
        // 都收进上限，不得以非有限值进入 cosmic-text `Metrics`（审计 F-1 同类通路）
        assert_eq!(
            used_line_height_px(&style_with(px(1e39)), 16.0),
            MAX_LINE_HEIGHT_PX
        );
        assert_eq!(
            used_line_height_px(&style_with(px(f64::INFINITY)), 16.0),
            MAX_LINE_HEIGHT_PX
        );
    }

    #[test]
    fn text_transform_uppercase_and_lowercase() {
        // Unicode 全尺寸映射：ß → SS
        assert_eq!(apply_text_transform("straße", Some("uppercase")), "STRASSE");
        assert_eq!(
            apply_text_transform("ABC def", Some("LOWERCASE")),
            "abc def"
        );
    }

    #[test]
    fn text_transform_capitalize_words() {
        assert_eq!(
            apply_text_transform("hello  wide\tworld", Some("capitalize")),
            "Hello  Wide\tWorld"
        );
        // 非 ASCII 首字符同样大写
        assert_eq!(apply_text_transform("étoile", Some("capitalize")), "Étoile");
    }

    #[test]
    fn text_transform_none_missing_and_unknown_borrow() {
        for keyword in [None, Some("none"), Some("full-width"), Some("bogus")] {
            let out = apply_text_transform("Mixed Case", keyword);
            assert_eq!(out, "Mixed Case");
            assert!(
                matches!(out, Cow::Borrowed(_)),
                "should borrow untouched text"
            );
        }
    }

    #[test]
    fn text_transform_capitalize_borrows_when_unchanged() {
        let out = apply_text_transform("Already Capitalized", Some("capitalize"));
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn text_transform_keyword_reads_computed_style() {
        let mut cs = ComputedStyle::new();
        cs.set(
            "text-transform",
            crate::ComputedValue::from_keyword("uppercase"),
        );
        assert_eq!(text_transform_keyword(&cs), Some("uppercase"));
        assert_eq!(text_transform_keyword(&ComputedStyle::new()), None);
    }
}
