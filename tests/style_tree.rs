//! B3 集成测试：`compute_styles` 整树样式计算 + font-size 传播（P0-1）。
//!
//! 验证两步 font-size 算法：
//! 1. font-size 用父 font-size 作 em/百分比基准解析；
//! 2. 其余属性用元素自身 font-size 作 em 基准。
//!
//! rem 用根元素 font-size 作基准，自根向下传播。

use muskitty_cascade::{compute_styles, ComputedStyle, StyleTreeOptions};
use muskitty_css::parser::ComponentValue;
use muskitty_css::tokenizer::Token;
use muskitty_cssom::{from_stylesheet, Origin};
use muskitty_dom::{Node, NodeKind};
use muskitty_selectors::matching::{DomElement, Element as _};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

fn parse_dom(html: &str) -> Rc<RefCell<Node>> {
    muskitty_html5_parser::parse(html)
}

fn author_sheet(css: &str) -> muskitty_cssom::CssStyleSheet {
    let parsed = muskitty_css::parse_stylesheet(css);
    let mut s = from_stylesheet(&parsed);
    s.origin = Origin::Author;
    s
}

fn find_element(
    node: &Rc<RefCell<Node>>,
    predicate: &dyn Fn(&DomElement) -> bool,
) -> Option<DomElement> {
    if matches!(&node.borrow().kind, NodeKind::Element(_)) {
        let el = DomElement::new(Rc::clone(node));
        if predicate(&el) {
            return Some(el);
        }
    }
    for child in node.borrow().child_nodes() {
        if let Some(found) = find_element(child, predicate) {
            return Some(found);
        }
    }
    None
}

fn element_with_id(node: &Rc<RefCell<Node>>, id: &str) -> DomElement {
    find_element(node, &|el| el.get_attribute("id").as_deref() == Some(id))
        .unwrap_or_else(|| panic!("element #{id} not found"))
}

fn addr(el: &DomElement) -> usize {
    Rc::as_ptr(el.inner()) as usize
}

/// 从 ComputedStyle 提取某属性的第一个 px Dimension 数值。
fn style_px(cs: &ComputedStyle, prop: &str) -> f64 {
    let cv = cs
        .get(prop)
        .unwrap_or_else(|| panic!("{prop} not in style"));
    let cvs = cv.tokens();
    for v in cvs {
        if let ComponentValue::PreservedToken(Token::Dimension(n, u)) = v {
            assert_eq!(u, "px", "expected px for {prop}");
            return n.value;
        }
    }
    panic!("{prop} has no px dimension in {:?}", cvs);
}

#[test]
fn font_size_inherits_to_child() {
    // 父 div font-size:28px，子 span 未声明 → 继承 28px
    let dom = parse_dom(
        r#"<html><body><div id="a" style="font-size: 28px"><span id="b"></span></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let b = element_with_id(&dom, "b");
    assert_eq!(style_px(styles.get(&addr(&a)).unwrap(), "font-size"), 28.0);
    assert_eq!(style_px(styles.get(&addr(&b)).unwrap(), "font-size"), 28.0);
}

#[test]
fn font_size_percentage_of_parent() {
    // 父 font-size:32px，子 font-size:200% → 64px
    let dom = parse_dom(
        r#"<html><body><div id="a" style="font-size: 32px"><span id="b" style="font-size: 200%"></span></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let b = element_with_id(&dom, "b");
    assert_eq!(style_px(styles.get(&addr(&b)).unwrap(), "font-size"), 64.0);
}

#[test]
fn em_in_margin_uses_own_font_size() {
    // P0-1 核心回归：div 自身 font-size:32px，margin-top:2em → 64px
    // （em 语义 = 元素自身 font-size，而非父 font-size 16px）。
    let dom = parse_dom(
        r#"<html><body><div id="a" style="font-size: 32px; margin-top: 2em"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    assert_eq!(style_px(styles.get(&addr(&a)).unwrap(), "margin-top"), 64.0);
}

#[test]
fn em_in_child_margin_uses_inherited_font_size() {
    // 父 font-size:32px，子 span margin-left:2em → 64px
    // （span 继承 32px，em 按自身 32px 计算）。
    let dom = parse_dom(
        r#"<html><body><div id="a" style="font-size: 32px"><span id="b" style="margin-left: 2em"></span></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let b = element_with_id(&dom, "b");
    assert_eq!(
        style_px(styles.get(&addr(&b)).unwrap(), "margin-left"),
        64.0
    );
}

#[test]
fn rem_uses_root_font_size() {
    // 根元素（html）font-size:20px，后代 margin-left:2rem → 40px
    let dom = parse_dom(
        r#"<html style="font-size: 20px"><body><div id="a" style="margin-left: 2rem"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    assert_eq!(
        style_px(styles.get(&addr(&a)).unwrap(), "margin-left"),
        40.0
    );
}

#[test]
fn root_font_size_defaults_to_16() {
    // 未声明 font-size → 根默认 16px；子元素继承 16px
    let dom = parse_dom(r#"<html><body><div id="a" style="margin-top: 1em"></div></body></html>"#);
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    assert_eq!(style_px(styles.get(&addr(&a)).unwrap(), "font-size"), 16.0);
    // 1em = 自身 font-size 16px
    assert_eq!(style_px(styles.get(&addr(&a)).unwrap(), "margin-top"), 16.0);
}

/// 从 ComputedStyle 提取某属性的第一个 Ident（用于 color 等关键字断言）。
fn style_ident(cs: &ComputedStyle, prop: &str) -> String {
    let cv = cs
        .get(prop)
        .unwrap_or_else(|| panic!("{prop} not in style"));
    let cvs = cv.tokens();
    for v in cvs {
        if let ComponentValue::PreservedToken(Token::Ident(s)) = v {
            return s.clone();
        }
    }
    panic!("{prop} has no Ident in {:?}", cvs);
}

// —— P2-4: CSS-wide 关键字不写入 `--*` 表 ——

#[test]
fn var_references_initial_css_wide_keyword_as_undefined() {
    // P2-4: :root { --x: initial } 不写入 props → var(--x, orange) 命中 fallback
    let dom = parse_dom(
        r#"<html><body><div id="a" style="color: var(--x, orange)"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, ":root { --x: initial; }");
    let a = element_with_id(&dom, "a");
    assert_eq!(
        style_ident(styles.get(&addr(&a)).unwrap(), "color"),
        "orange",
        "initial 不写入 --x，var() 应回退到 fallback"
    );
}

#[test]
fn var_references_inherit_keyword_uses_parent_chain() {
    // P2-4: .child { --x: inherit } 不覆盖 → var(--x) 回溯到根级 red
    let dom = parse_dom(
        r#"<html><body>
            <div class="child" id="c" style="--x: inherit">
                <span id="g" style="color: var(--x)"></span>
            </div>
        </body></html>"#,
    );
    let styles = run_from_dom(&dom, ":root { --x: red; }");
    let g = element_with_id(&dom, "g");
    assert_eq!(
        style_ident(styles.get(&addr(&g)).unwrap(), "color"),
        "red",
        "inherit 关键字应沿用父链值"
    );
}

// —— P2-5: invalid-at-computed-value（var() 首参非 --*）→ 属性按 unset ——

#[test]
fn invalid_var_treats_property_as_unset() {
    // div { color: var(color) } 首参非 --* → invalid at computed-value time
    // → 属性按 unset：继承属性取父值（html color: red）
    let dom = parse_dom(
        r#"<html style="color: red"><body><div id="a" style="color: var(color)"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    assert_eq!(
        style_ident(styles.get(&addr(&a)).unwrap(), "color"),
        "red",
        "var(color) 无效 → color 回退到继承的父值"
    );
}

/// 复用已解析的 DOM（避免重复解析），用默认视口（1920×1080）。
fn run_from_dom(dom: &Rc<RefCell<Node>>, css: &str) -> HashMap<usize, ComputedStyle> {
    run_from_dom_opts(dom, css, &StyleTreeOptions::default())
}

/// 同 [`run_from_dom`]，但指定视口选项（media query 求值用）。
fn run_from_dom_opts(
    dom: &Rc<RefCell<Node>>,
    css: &str,
    options: &StyleTreeOptions,
) -> HashMap<usize, ComputedStyle> {
    let sheet = author_sheet(css);
    compute_styles(dom, &[sheet], options)
}

// —— M-3: media 视口接线 ——

#[test]
fn media_min_width_applies_at_wide_viewport() {
    // StyleTreeOptions::default() 视口 1920×1080 → (min-width:800px) 命中
    let dom = parse_dom(r#"<html><body><div id="a"></div></body></html>"#);
    let styles = run_from_dom(&dom, "@media (min-width: 800px) { div { color: red } }");
    let a = element_with_id(&dom, "a");
    assert_eq!(
        style_ident(styles.get(&addr(&a)).unwrap(), "color"),
        "red",
        "默认 1920 视口应命中 (min-width:800px)"
    );
}

#[test]
fn media_min_width_pruned_at_narrow_viewport() {
    // 640 视口 → (min-width:800px) 剪枝，color 回退初始值 black
    let dom = parse_dom(r#"<html><body><div id="a"></div></body></html>"#);
    let opts = StyleTreeOptions {
        viewport_width: 640.0,
        viewport_height: 480.0,
    };
    let styles = run_from_dom_opts(
        &dom,
        "@media (min-width: 800px) { div { color: red } }",
        &opts,
    );
    let a = element_with_id(&dom, "a");
    assert_eq!(
        style_ident(styles.get(&addr(&a)).unwrap(), "color"),
        "black",
        "640 视口应剪枝 (min-width:800px)，color 取初始值"
    );
}

// —— CAS-2/3: compute_one 快速路径回归 ——

/// CSS-wide 关键字（含 revert-layer）不得被"免解析直享"快速路径短路成
/// 字面量——必须走 defaulting 改写：继承属性当 inherit（取父值）、
/// 非继承属性当 initial。快速路径判定若漏掉任一关键字（如 revert-layer），
/// 本测试即失败（computed 值变成字面量 ident）。
#[test]
fn css_wide_keywords_default_not_shared() {
    let dom = parse_dom(
        r#"<html style="color: red"><body><div id="a" style="color: revert-layer; display: revert"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    // color（继承）: revert-layer → unset → 继承父值 red，非字面量。
    assert_eq!(
        style_ident(cs, "color"),
        "red",
        "revert-layer on inherited property must default to parent value"
    );
    // display（非继承）: revert → initial → inline，非字面量。
    assert_eq!(
        style_ident(cs, "display"),
        "inline",
        "revert on non-inherited property must default to initial value"
    );
}

/// 未声明属性经快速路径直填后的语义不变：非继承属性取 initial 常量，
/// 继承属性取父 computed 值（此前由"全属性盲算"逐 token 物化）。
#[test]
fn undeclared_properties_fill_initial_and_parent() {
    let dom = parse_dom(r#"<html style="white-space: pre"><body><div id="a"></div></body></html>"#);
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    // 非继承 + 未声明 → initial（overflow: visible）。
    assert_eq!(
        style_ident(cs, "overflow"),
        "visible",
        "undeclared non-inherited property must fill initial constant"
    );
    // 继承 + 未声明 → 父 computed 值（white-space: pre）。
    assert_eq!(
        style_ident(cs, "white-space"),
        "pre",
        "undeclared inherited property must take parent computed value"
    );
}

/// 免解析直享（CAS-2 快速路径 2）对普通声明值的结果与完整路径一致：
/// token 序列原样进入 computed 值。
#[test]
fn plain_declared_value_computes_identically() {
    let dom = parse_dom(
        r#"<html><body><div id="a" style="color: rgb(0, 0, 255); margin-left: 10px"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    // 函数值（rgb）走完整解析路径，token 保持函数形态。
    let color = cs.get("color").unwrap();
    assert!(
        color.tokens().iter().any(|cv| matches!(
            cv,
            ComponentValue::Function(f) if f.name.eq_ignore_ascii_case("rgb")
        )),
        "rgb() must stay a function token, got {:?}",
        color.tokens()
    );
    // 绝对单位 px 值原样保留。
    assert_eq!(style_px(cs, "margin-left"), 10.0);
}

// —— M-3 batch 2: border/outline 宽度关键字归一化（CSS Backgrounds L3 §4.3）——

#[test]
fn border_width_keywords_normalize_to_px() {
    let dom = parse_dom(
        r#"<html><body><div id="a" style="border: thin solid red; border-left-width: thick; outline: medium solid"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    // thin=1px / medium=3px / thick=5px（Chrome/Firefox 取值）
    assert_eq!(style_px(cs, "border-top-width"), 1.0);
    assert_eq!(style_px(cs, "border-right-width"), 1.0);
    assert_eq!(
        style_px(cs, "border-left-width"),
        5.0,
        "later declaration wins"
    );
    assert_eq!(style_px(cs, "outline-width"), 3.0);
}

#[test]
fn border_width_default_medium_normalizes_without_declaration() {
    // 未声明任何 border → 初始值 medium 同样归一化为 3px（used 值由
    // style=none 在下游定为 0）
    let dom = parse_dom(r#"<html><body><div id="a"></div></body></html>"#);
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    assert_eq!(style_px(cs, "border-top-width"), 3.0);
    assert_eq!(style_px(cs, "outline-width"), 3.0);

    // 显式 px 宽度原样保留
    let dom = parse_dom(
        r#"<html><body><div id="a" style="border-width: 7px; border-style: solid"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    assert_eq!(style_px(cs, "border-top-width"), 7.0);
}

// —— M-3 batch 3: line-height 计算值（CSS Inline L3 §4.2）——

#[test]
fn line_height_percentage_normalizes_against_own_font_size() {
    // 150% × 24px = 36px（百分比按自身 font-size 在计算值阶段解析）
    let dom = parse_dom(
        r#"<html><body><div id="a" style="font-size: 24px; line-height: 150%"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    assert_eq!(style_px(cs, "line-height"), 36.0);
}

#[test]
fn line_height_number_and_normal_stay_unresolved() {
    // 数保持数字形态（倍数随值继承，由各元素自身 font-size 折算）；
    // normal 保持关键字（使用值阶段取 UA 默认 1.2）
    let dom = parse_dom(
        r#"<html><body><div id="a" style="font-size: 20px; line-height: 1.5"></div><div id="b" style="line-height: normal"></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let cs = styles.get(&addr(&a)).unwrap();
    let number = cs.get("line-height").unwrap();
    assert!(
        matches!(
            number.tokens().first(),
            Some(ComponentValue::PreservedToken(Token::Number(_)))
        ),
        "line-height: 1.5 must stay a number (it inherits as a multiplier), got {:?}",
        number.tokens()
    );
    assert_eq!(
        muskitty_cascade::used_line_height_px(cs, 20.0),
        30.0,
        "1.5 × 20px"
    );

    let b = element_with_id(&dom, "b");
    let cs_b = styles.get(&addr(&b)).unwrap();
    assert_eq!(style_ident(cs_b, "line-height"), "normal");
    assert_eq!(muskitty_cascade::used_line_height_px(cs_b, 16.0), 19.2);
}

#[test]
fn line_height_number_inherits_as_multiplier() {
    // 父 1.5 / 16px → 父行高 24px；子 font-size 32px 继承同一个 1.5 → 48px
    let dom = parse_dom(
        r#"<html><body><div id="a" style="font-size: 16px; line-height: 1.5"><span id="b" style="font-size: 32px"></span></div></body></html>"#,
    );
    let styles = run_from_dom(&dom, "");
    let a = element_with_id(&dom, "a");
    let b = element_with_id(&dom, "b");
    let cs_a = styles.get(&addr(&a)).unwrap();
    let cs_b = styles.get(&addr(&b)).unwrap();
    assert_eq!(muskitty_cascade::used_line_height_px(cs_a, 16.0), 24.0);
    assert_eq!(
        muskitty_cascade::used_line_height_px(cs_b, 32.0),
        48.0,
        "inherited multiplier applies to the child's own font-size"
    );
}
