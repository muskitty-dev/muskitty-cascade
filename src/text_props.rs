//! 文本相关属性的**使用值**语义（line-height / text-transform）。
//!
//! 这两个属性的使用值必须被 layout（文本测量：决定换行与盒高）与 renderer
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

#[cfg(test)]
mod tests {
    use super::*;
    use muskitty_css::tokenizer::Numeric;

    fn px(v: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Dimension(
            Numeric {
                value: v,
                is_integer: false,
            },
            "px".to_string(),
        ))
    }

    fn num(v: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Number(Numeric {
            value: v,
            is_integer: false,
        }))
    }

    fn pct(v: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Percentage(Numeric {
            value: v,
            is_integer: false,
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
