//! CC-7 端到端集成测试：parse CSS → DOM → cascade → computed value。
//!
//! 验证完整数据流：
//! ```text
//! CssStyleSheet[] + DomElement
//!     → collect_declared_values (§5 Filtering)
//!     → cascade_for_element (§6.1 Cascade 排序)
//!     → cascade_winner (取首项)
//!     → apply_defaulting (§7 Defaulting)
//!     → compute_value (§4.4 Computed Value)
//! ```

use muskitty_cascade::{
    apply_defaulting, cascade_for_element, cascade_winner, collect_declared_values, compute_value,
    ComputeContext, ComputedValue,
};
use muskitty_css::parse_stylesheet;
use muskitty_css::tokenizer::Token;
use muskitty_cssom::{from_stylesheet, Origin};
use muskitty_dom::{Attribute, Node};
use muskitty_selectors::matching::DomElement;
use std::collections::HashMap;

// —— 辅助函数 ——

fn make_element(tag: &str, attrs: &[(&str, &str)]) -> DomElement {
    let doc = Node::new_document();
    let attrs: Vec<Attribute> = attrs.iter().map(|(k, v)| Attribute::new(k, v)).collect();
    let node = Node::new_element_html(tag, attrs, &doc);
    DomElement::new(node)
}

fn make_sheet(css: &str, origin: Origin) -> muskitty_cssom::CssStyleSheet {
    let parsed = parse_stylesheet(css);
    let mut sheet = from_stylesheet(&parsed);
    sheet.origin = origin;
    sheet
}

/// 完整 pipeline：对单个属性，从 DOM + CSS 计算出 computed value。
fn compute_property(
    element: &DomElement,
    sheets: &[muskitty_cssom::CssStyleSheet],
    property: &str,
    parent_computed: Option<&ComputedValue>,
    ctx: &ComputeContext,
) -> ComputedValue {
    let declared = collect_declared_values(element, sheets);
    let groups = cascade_for_element(declared);
    let group: &[muskitty_cascade::DeclaredValue] =
        groups.get(property).map(|g| g.as_slice()).unwrap_or(&[]);
    let winner = cascade_winner(group);
    let cascaded = winner.map(|w| w.value.as_slice());
    let specified = apply_defaulting(property, cascaded, parent_computed);
    // 单态化（P2-20）：defaulting 产物与原始声明统一为 token 序列，直接
    // 计算（幂等），不再区分 Raw/Keyword 来源。
    compute_value(property, specified.tokens(), ctx)
}

/// 断言 computed value 首 token 为 `value unit` 的 Dimension（如 40px）。
fn assert_dimension(cv: &ComputedValue, value: f64, unit: &str) {
    let cvs = cv.tokens();
    match &cvs[0] {
        muskitty_css::parser::ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
            assert_eq!(n.value, value);
            assert_eq!(u, unit);
        }
        other => panic!("expected Dimension, got {:?}", other),
    }
}

fn default_ctx() -> ComputeContext<'static> {
    static EMPTY: std::sync::OnceLock<HashMap<String, Vec<muskitty_css::parser::ComponentValue>>> =
        std::sync::OnceLock::new();
    let props = EMPTY.get_or_init(HashMap::new);
    ComputeContext::new(props)
}

// —— 基础 cascade ——

#[test]
fn single_rule_single_property() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: red; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.tokens().len(), 1);
    assert_eq!(result.keyword(), Some("red"));
}

#[test]
fn higher_specificity_wins() {
    let element = make_element("div", &[("id", "main")]);
    let sheet = make_sheet("div { color: red; } #main { color: blue; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("blue")); // #main 的特异性更高
}

#[test]
fn important_beats_normal() {
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "div { color: red; } div { color: blue !important; }",
        Origin::Author,
    );
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("blue")); // !important 胜出
}

#[test]
fn later_declaration_wins_on_tie() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: red; } div { color: green; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("green")); // 后出现的胜出
}

// —— Defaulting ——

#[test]
fn no_declaration_uses_initial_value() {
    // div 无 color 声明 → 非根元素也无父 → 初始值 "black"
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font-size: 16px; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("black"));
}

#[test]
fn inherited_property_inherits_from_parent() {
    // color 是继承属性，无声明时从父继承
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font-size: 16px; }", Origin::Author);
    let ctx = default_ctx();
    let parent_color = ComputedValue::from_keyword("red");

    let result = compute_property(&element, &[sheet], "color", Some(&parent_color), &ctx);
    assert_eq!(result.keyword(), Some("red"));
}

#[test]
fn initial_keyword_resets_to_initial() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: initial; }", Origin::Author);
    let ctx = default_ctx();
    let parent_color = ComputedValue::from_keyword("red");

    let result = compute_property(&element, &[sheet], "color", Some(&parent_color), &ctx);
    assert_eq!(result.keyword(), Some("black"));
}

#[test]
fn inherit_keyword_explicitly_inherits() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: inherit; }", Origin::Author);
    let ctx = default_ctx();
    let parent_color = ComputedValue::from_keyword("blue");

    let result = compute_property(&element, &[sheet], "color", Some(&parent_color), &ctx);
    assert_eq!(result.keyword(), Some("blue"));
}

// —— 相对单位解析 ——

#[test]
fn em_resolves_in_full_pipeline() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin-top: 2em; }", Origin::Author);
    let ctx = ComputeContext {
        parent_font_size: 20.0,
        ..default_ctx()
    };

    let result = compute_property(&element, &[sheet], "margin-top", None, &ctx);
    assert_dimension(&result, 40.0, "px"); // 2em * 20px = 40px
}

#[test]
fn font_size_percentage_resolves_in_full_pipeline() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font-size: 150%; }", Origin::Author);
    let ctx = ComputeContext {
        parent_font_size: 20.0,
        ..default_ctx()
    };

    let result = compute_property(&element, &[sheet], "font-size", None, &ctx);
    assert_dimension(&result, 30.0, "px"); // 150% * 20px = 30px
}

// —— 多 origin ——

#[test]
fn author_beats_user_agent() {
    let element = make_element("div", &[]);
    let ua_sheet = make_sheet("div { color: gray; }", Origin::UserAgent);
    let author_sheet = make_sheet("div { color: red; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[ua_sheet, author_sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("red")); // Author 胜出
}

#[test]
fn important_ua_beats_important_author() {
    let element = make_element("div", &[]);
    let ua_sheet = make_sheet("div { color: gray !important; }", Origin::UserAgent);
    let author_sheet = make_sheet("div { color: red !important; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[ua_sheet, author_sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("gray")); // Important UA 胜出
}

// —— 多属性 ——

#[test]
fn multiple_properties_computed() {
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "div { color: red; font-size: 16px; display: block; }",
        Origin::Author,
    );
    let ctx = default_ctx();
    let sheets = [sheet];

    // color
    let color = compute_property(&element, &sheets, "color", None, &ctx);
    assert_eq!(color.keyword(), Some("red"));

    // font-size
    let font_size = compute_property(&element, &sheets, "font-size", None, &ctx);
    assert_dimension(&font_size, 16.0, "px");

    // display
    let display = compute_property(&element, &sheets, "display", None, &ctx);
    assert_eq!(display.keyword(), Some("block"));
}

// —— var() 全链路 ——

#[test]
fn var_in_full_pipeline() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: var(--main); }", Origin::Author);
    let mut props: HashMap<String, Vec<muskitty_css::parser::ComponentValue>> = HashMap::new();
    props.insert(
        "--main".to_string(),
        vec![muskitty_css::parser::ComponentValue::PreservedToken(
            Token::Ident("blue".to_string()),
        )],
    );
    let ctx = ComputeContext::new(&props);

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("blue"));
}

// —— Cascade Layers（P1-3）——

#[test]
fn unlayered_normal_beats_layered_normal_in_pipeline() {
    // §6.1 准则 5: 未分层 normal 声明胜过分层 normal（隐式 final 层）
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "div { color: red; } @layer a { div { color: blue; } }",
        Origin::Author,
    );
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("red"));
}

#[test]
fn earlier_layer_wins_for_important_in_pipeline() {
    // §6.1 准则 5: important 声明早层胜
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@layer a { div { color: red !important; } } @layer b { div { color: blue !important; } }",
        Origin::Author,
    );
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("red"));
}

#[test]
fn later_layer_wins_for_normal_in_pipeline() {
    // §6.1 准则 5: normal 声明晚层胜
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@layer a { div { color: red; } } @layer b { div { color: blue; } }",
        Origin::Author,
    );
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("blue"));
}

// —— 属性名大小写 + 未知属性（P2-2/P2-21）——

#[test]
fn case_insensitive_property_name_collected() {
    // P2-2: CSS 属性名大小写不敏感，COLOR 应参与 color 级联
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { COLOR: red; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("red"));
}

#[test]
fn unknown_property_dropped() {
    // P2-21: 未注册且非 --* 的属性不进入级联
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { foo: 1; color: red; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    let props: Vec<&str> = declared.iter().map(|d| d.property.as_str()).collect();
    assert!(!props.contains(&"foo"), "unknown prop should be dropped");
    assert!(props.contains(&"color"));
}

#[test]
fn custom_property_case_preserved() {
    // P2-2: --* 自定义属性名大小写敏感，不归一化
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { --Main: red; color: var(--Main); }", Origin::Author);
    let declared = collect_declared_values(&element, &[sheet]);
    let props: Vec<&str> = declared.iter().map(|d| d.property.as_str()).collect();
    assert!(props.contains(&"--Main"));
}

// —— gap 简写展开（P2-9）——

#[test]
fn gap_shorthand_overrides_column_gap() {
    // gap 简写在收集时展开为 row-gap + column-gap，后声明覆盖 column-gap
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { column-gap: 20px; gap: 10px; }", Origin::Author);
    let ctx = default_ctx();

    let result = compute_property(&element, &[sheet], "column-gap", None, &ctx);
    assert_dimension(&result, 10.0, "px");
}

#[test]
fn gap_shorthand_two_values_split() {
    // gap: 10px 20px → row-gap=10px, column-gap=20px
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { gap: 10px 20px; }", Origin::Author);
    let ctx = default_ctx();

    let row = compute_property(
        &element,
        std::slice::from_ref(&sheet),
        "row-gap",
        None,
        &ctx,
    );
    assert_dimension(&row, 10.0, "px");

    let col = compute_property(&element, &[sheet], "column-gap", None, &ctx);
    assert_dimension(&col, 20.0, "px");
}

#[test]
fn gap_shorthand_no_longer_emits_gap_itself() {
    // P2-9: gap 不再作为独立属性进入级联
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { gap: 10px; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    let props: Vec<&str> = declared.iter().map(|d| d.property.as_str()).collect();
    assert!(!props.contains(&"gap"), "gap should be expanded away");
    assert!(props.contains(&"row-gap"));
    assert!(props.contains(&"column-gap"));
}

// —— 非匹配选择器 → defaulting ——

#[test]
fn non_matching_selector_falls_back_to_initial() {
    let element = make_element("span", &[]);
    let sheet = make_sheet("div { color: red; }", Origin::Author);
    let ctx = default_ctx();

    // span 不匹配 div 选择器 → 无声明 → 初始值 "black"
    let result = compute_property(&element, &[sheet], "color", None, &ctx);
    assert_eq!(result.keyword(), Some("black"));
}

// —— C1: 简写 → 长属性展开 ——

/// 收集 + cascade 后某属性的胜出值 token 序列（展开断言用，绕过 defaulting）。
fn winner_tokens(
    element: &DomElement,
    sheet: &muskitty_cssom::CssStyleSheet,
    property: &str,
) -> Vec<muskitty_css::parser::ComponentValue> {
    let declared = collect_declared_values(element, std::slice::from_ref(sheet));
    let groups = cascade_for_element(declared);
    groups
        .get(property)
        .map(|v| v.as_slice())
        .and_then(cascade_winner)
        .map(|d| d.value.clone())
        .unwrap_or_default()
}

/// 断言 token 序列是单个 Dimension。
fn assert_tok_dim(toks: &[muskitty_css::parser::ComponentValue], value: f64, unit: &str) {
    use muskitty_css::parser::ComponentValue;
    assert_eq!(toks.len(), 1, "expected single token, got {:?}", toks);
    match &toks[0] {
        ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
            assert_eq!(n.value, value);
            assert_eq!(u, unit);
        }
        other => panic!("expected Dimension, got {:?}", other),
    }
}

/// 断言 token 序列是单个 Ident。
fn assert_tok_ident(toks: &[muskitty_css::parser::ComponentValue], s: &str) {
    use muskitty_css::parser::ComponentValue;
    assert_eq!(toks.len(), 1, "expected single token, got {:?}", toks);
    match &toks[0] {
        ComponentValue::PreservedToken(Token::Ident(i)) => assert_eq!(i, s),
        other => panic!("expected Ident, got {:?}", other),
    }
}

/// 断言 token 序列是单个 Number。
fn assert_tok_number(toks: &[muskitty_css::parser::ComponentValue], v: f64) {
    use muskitty_css::parser::ComponentValue;
    assert_eq!(toks.len(), 1, "expected single token, got {:?}", toks);
    match &toks[0] {
        ComponentValue::PreservedToken(Token::Number(n)) => assert_eq!(n.value, v),
        other => panic!("expected Number, got {:?}", other),
    }
}

/// 断言 token 序列是单个 Percentage。
fn assert_tok_pct(toks: &[muskitty_css::parser::ComponentValue], v: f64) {
    use muskitty_css::parser::ComponentValue;
    assert_eq!(toks.len(), 1, "expected single token, got {:?}", toks);
    match &toks[0] {
        ComponentValue::PreservedToken(Token::Percentage(n)) => assert_eq!(n.value, v),
        other => panic!("expected Percentage, got {:?}", other),
    }
}

// —— margin / padding 四向拆分 ——

#[test]
fn margin_one_value_splits_to_all_four() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: 10px; }", Origin::Author);
    for (prop, expected) in [
        ("margin-top", 10.0),
        ("margin-right", 10.0),
        ("margin-bottom", 10.0),
        ("margin-left", 10.0),
    ] {
        let toks = winner_tokens(&element, &sheet, prop);
        assert_tok_dim(&toks, expected, "px");
    }
}

#[test]
fn margin_two_values_split() {
    // margin: 10px 20px → 上下=10px, 左右=20px
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: 10px 20px; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-top"), 10.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-right"), 20.0, "px");
    assert_tok_dim(
        &winner_tokens(&element, &sheet, "margin-bottom"),
        10.0,
        "px",
    );
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-left"), 20.0, "px");
}

#[test]
fn margin_three_values_split() {
    // margin: 1px 2px 3px → 上=1px, 左右=2px, 下=3px
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: 1px 2px 3px; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-top"), 1.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-right"), 2.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-bottom"), 3.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-left"), 2.0, "px");
}

#[test]
fn margin_four_values_split() {
    // margin: 1px 2px 3px 4px → top/right/bottom/left 顺时针
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: 1px 2px 3px 4px; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-top"), 1.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-right"), 2.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-bottom"), 3.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-left"), 4.0, "px");
}

#[test]
fn padding_single_value_splits() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { padding: 5px; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "padding-top"), 5.0, "px");
    assert_tok_dim(&winner_tokens(&element, &sheet, "padding-right"), 5.0, "px");
    assert_tok_dim(
        &winner_tokens(&element, &sheet, "padding-bottom"),
        5.0,
        "px",
    );
    assert_tok_dim(&winner_tokens(&element, &sheet, "padding-left"), 5.0, "px");
}

#[test]
fn margin_shorthand_does_not_emit_margin_itself() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: 10px; }", Origin::Author);
    let declared = collect_declared_values(&element, &[sheet]);
    let props: Vec<&str> = declared.iter().map(|d| d.property.as_str()).collect();
    assert!(!props.contains(&"margin"), "margin should be expanded away");
    assert!(props.contains(&"margin-top"));
}

#[test]
fn margin_shorthand_overridden_by_later_longhand() {
    // margin: 10px 之后 margin-top: 20px（order 更大）→ margin-top=20px
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: 10px; margin-top: 20px; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "margin-top"), 20.0, "px");
}

#[test]
fn margin_global_keyword_expands() {
    // margin: inherit → 每个长属性取 inherit（CSS Cascade L5 §3.2）
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: inherit; }", Origin::Author);
    assert_tok_ident(&winner_tokens(&element, &sheet, "margin-top"), "inherit");
}

#[test]
fn margin_invalid_count_dropped() {
    // 5 个分量 → 无效简写 → 整条丢弃，不产出任何 margin-* 长属性
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { margin: 1px 2px 3px 4px 5px; }", Origin::Author);
    for prop in ["margin-top", "margin-right", "margin-bottom", "margin-left"] {
        assert!(
            winner_tokens(&element, &sheet, prop).is_empty(),
            "{} should not be emitted for invalid margin shorthand",
            prop
        );
    }
}

// —— flex 简写 ——

#[test]
fn flex_none_keyword() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: none; }", Origin::Author);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-grow"), 0.0);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-shrink"), 0.0);
    assert_tok_ident(&winner_tokens(&element, &sheet, "flex-basis"), "auto");
}

#[test]
fn flex_auto_keyword() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: auto; }", Origin::Author);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-grow"), 1.0);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-shrink"), 1.0);
    assert_tok_ident(&winner_tokens(&element, &sheet, "flex-basis"), "auto");
}

#[test]
fn flex_grow_only() {
    // flex: 2 → grow=2, shrink=1, basis=0%（简写省略默认，非初始值 0 1 auto）
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: 2; }", Origin::Author);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-grow"), 2.0);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-shrink"), 1.0);
    assert_tok_pct(&winner_tokens(&element, &sheet, "flex-basis"), 0.0);
}

#[test]
fn flex_grow_shrink() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: 2 3; }", Origin::Author);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-grow"), 2.0);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-shrink"), 3.0);
    assert_tok_pct(&winner_tokens(&element, &sheet, "flex-basis"), 0.0);
}

#[test]
fn flex_grow_basis() {
    // flex: 2 10px → grow=2, shrink=1（省略）, basis=10px
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: 2 10px; }", Origin::Author);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-grow"), 2.0);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-shrink"), 1.0);
    assert_tok_dim(&winner_tokens(&element, &sheet, "flex-basis"), 10.0, "px");
}

#[test]
fn flex_full_three_components() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: 2 3 10px; }", Origin::Author);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-grow"), 2.0);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-shrink"), 3.0);
    assert_tok_dim(&winner_tokens(&element, &sheet, "flex-basis"), 10.0, "px");
}

#[test]
fn flex_length_only() {
    // flex: 10px → grow=1, shrink=1, basis=10px
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: 10px; }", Origin::Author);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-grow"), 1.0);
    assert_tok_number(&winner_tokens(&element, &sheet, "flex-shrink"), 1.0);
    assert_tok_dim(&winner_tokens(&element, &sheet, "flex-basis"), 10.0, "px");
}

#[test]
fn flex_shorthand_does_not_emit_flex() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { flex: 2; }", Origin::Author);
    let declared = collect_declared_values(&element, &[sheet]);
    let props: Vec<&str> = declared.iter().map(|d| d.property.as_str()).collect();
    assert!(!props.contains(&"flex"), "flex should be expanded away");
    assert!(props.contains(&"flex-grow"));
}

// —— background 简写 → background-color ——

#[test]
fn background_named_color_extracted() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { background: red; }", Origin::Author);
    assert_tok_ident(&winner_tokens(&element, &sheet, "background-color"), "red");
}

#[test]
fn background_hash_color_extracted() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { background: #ff0000; }", Origin::Author);
    let toks = winner_tokens(&element, &sheet, "background-color");
    assert_eq!(toks.len(), 1);
    assert!(matches!(
        &toks[0],
        muskitty_css::parser::ComponentValue::PreservedToken(Token::Hash(..))
    ));
}

#[test]
fn background_color_among_bg_layer_keywords() {
    // background: url(x.png) no-repeat red center → 反集法识别 red 为颜色
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "div { background: url(x.png) no-repeat red center; }",
        Origin::Author,
    );
    assert_tok_ident(&winner_tokens(&element, &sheet, "background-color"), "red");
}

#[test]
fn background_no_color_defaults_transparent() {
    // 只有背景图/定位关键字 → 颜色取初始值 transparent
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "div { background: url(x.png) no-repeat center; }",
        Origin::Author,
    );
    assert_tok_ident(
        &winner_tokens(&element, &sheet, "background-color"),
        "transparent",
    );
}

#[test]
fn background_shorthand_does_not_emit_background() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { background: red; }", Origin::Author);
    let declared = collect_declared_values(&element, &[sheet]);
    let props: Vec<&str> = declared.iter().map(|d| d.property.as_str()).collect();
    assert!(
        !props.contains(&"background"),
        "background should be expanded away"
    );
    assert!(props.contains(&"background-color"));
}

// —— font 简写 → font-size（+ line-height）——

#[test]
fn font_size_extracted() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font: 16px Arial; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "font-size"), 16.0, "px");
}

#[test]
fn font_with_weight_prefix() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font: bold 16px Arial; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "font-size"), 16.0, "px");
}

#[test]
fn font_with_line_height() {
    // font: 16px/1.5 Arial → font-size=16px, line-height=1.5
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font: 16px/1.5 Arial; }", Origin::Author);
    assert_tok_dim(&winner_tokens(&element, &sheet, "font-size"), 16.0, "px");
    assert_tok_number(&winner_tokens(&element, &sheet, "line-height"), 1.5);
}

#[test]
fn font_absolute_size_keyword() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font: medium Arial; }", Origin::Author);
    assert_tok_ident(&winner_tokens(&element, &sheet, "font-size"), "medium");
}

#[test]
fn font_missing_size_dropped() {
    // font: Arial（无 font-size）→ 无效简写 → 不产出 font-size
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font: Arial; }", Origin::Author);
    assert!(winner_tokens(&element, &sheet, "font-size").is_empty());
}

#[test]
fn font_shorthand_does_not_emit_font() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { font: 16px Arial; }", Origin::Author);
    let declared = collect_declared_values(&element, &[sheet]);
    let props: Vec<&str> = declared.iter().map(|d| d.property.as_str()).collect();
    assert!(!props.contains(&"font"), "font should be expanded away");
    assert!(props.contains(&"font-size"));
}
