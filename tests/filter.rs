//! CC-3 Filtering 测试：选择器匹配 → DeclaredValue 收集。

use muskitty_cascade::{
    collect_declared_values, collect_declared_values_prepared, prepare_sheets_with_context,
    MediaContext,
};
use muskitty_css::parse_stylesheet;
use muskitty_cssom::{from_stylesheet, Origin};
use muskitty_dom::{Attribute, Node};
use muskitty_selectors::matching::DomElement;

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

#[test]
fn simple_type_selector_match() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: red; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
    assert_eq!(declared[0].origin, Origin::Author);
    assert!(!declared[0].important);
    assert!(!declared[0].from_style_attr);
}

#[test]
fn class_selector_match() {
    let element = make_element("div", &[("class", "foo")]);
    let sheet = make_sheet(".foo { color: blue; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn id_selector_match() {
    let element = make_element("div", &[("id", "bar")]);
    let sheet = make_sheet("#bar { color: green; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
}

#[test]
fn non_matching_selector() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("span { color: red; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert!(declared.is_empty());
}

#[test]
fn multiple_declarations_in_one_rule() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: red; font-size: 16px; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 2);
    assert_eq!(declared[0].property, "color");
    assert_eq!(declared[1].property, "font-size");
}

#[test]
fn multiple_rules_matching_same_element() {
    let element = make_element("div", &[("class", "foo")]);
    let sheet = make_sheet("div { color: red; } .foo { color: blue; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 2);
    // order 应递增
    assert!(declared[0].order < declared[1].order);
}

#[test]
fn important_flag_collected() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: red !important; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert!(declared[0].important);
}

#[test]
fn media_rule_screen_collected() {
    // P2-6：@media screen 在默认屏幕视口命中 → 收集。
    let element = make_element("div", &[]);
    let sheet = make_sheet("@media screen { div { color: black; } }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn nested_rules_collected() {
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "div { color: red; &:hover { color: blue; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    // div 匹配 → color: red
    // &:hover 不匹配（没有 :hover 状态）→ 不收集
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn specificity_recorded() {
    let element = make_element("div", &[("id", "bar"), ("class", "foo")]);
    let sheet = make_sheet(
        "#bar { color: red; } .foo { color: blue; } div { color: green; }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 3);

    // #bar 的 specificity 应该最高 (1,0,0)
    let id_decl = declared.iter().find(|d| d.specificity.a == 1).unwrap();
    assert_eq!(id_decl.property, "color");

    // .foo 的 specificity (0,1,0)
    let class_decl = declared
        .iter()
        .find(|d| d.specificity.b == 1 && d.specificity.a == 0)
        .unwrap();

    // div 的 specificity (0,0,1)
    let type_decl = declared
        .iter()
        .find(|d| d.specificity.c == 1 && d.specificity.b == 0)
        .unwrap();

    // 验证顺序
    assert!(id_decl.order < class_decl.order);
    assert!(class_decl.order < type_decl.order);
}

#[test]
fn origin_recorded() {
    let element = make_element("div", &[]);
    let ua_sheet = make_sheet("div { color: black; }", Origin::UserAgent);
    let author_sheet = make_sheet("div { color: red; }", Origin::Author);

    let declared = collect_declared_values(&element, &[ua_sheet, author_sheet]);
    assert_eq!(declared.len(), 2);

    let ua_decl = declared
        .iter()
        .find(|d| d.origin == Origin::UserAgent)
        .unwrap();
    let author_decl = declared
        .iter()
        .find(|d| d.origin == Origin::Author)
        .unwrap();

    assert!(ua_decl.order < author_decl.order);
}

#[test]
fn layer_block_rules_collected() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("@layer base { div { color: red; } }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
}

#[test]
fn keyframes_content_does_not_pollute_element_matching() {
    // P2-14: @keyframes 的 from/to 块不是 style rule，不得参与元素匹配。
    // 旧实现把 from 块转成 CssRule::Style，元素 <from> 会被
    // `from { opacity: 0 }` 匹配并收集到 opacity 声明（数据污染）。
    let element = make_element("from", &[]);
    let sheet = make_sheet(
        "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert!(
        declared.is_empty(),
        "keyframe blocks must not produce declared values, got {:?}",
        declared
    );
}

#[test]
fn font_face_and_page_do_not_pollute_element_matching() {
    // @font-face / @page 与元素匹配无关（P2-14 类型化后跳过）。
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@font-face { font-family: X; src: url(x); } @page { margin: 1cm; } div { color: red; }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn import_and_namespace_skipped() {
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@import \"style.css\"; @namespace svg \"http://www.w3.org/2000/svg\"; div { color: red; }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn multiple_stylesheets() {
    let element = make_element("div", &[]);
    let sheet1 = make_sheet("div { color: red; }", Origin::Author);
    let sheet2 = make_sheet("div { color: blue; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet1, sheet2]);
    assert_eq!(declared.len(), 2);
    // sheet1 的 order 应小于 sheet2
    assert!(declared[0].order < declared[1].order);
}

#[test]
fn style_attr_collected() {
    let element = make_element("div", &[("style", "color: red")]);
    let sheets: Vec<muskitty_cssom::CssStyleSheet> = vec![];

    let declared = collect_declared_values(&element, &sheets);

    let color_decl = declared.iter().find(|d| d.property == "color");
    assert!(
        color_decl.is_some(),
        "style attr 'color: red' should be collected"
    );
    let color_decl = color_decl.unwrap();
    assert!(color_decl.from_style_attr, "from_style_attr should be true");
    assert_eq!(color_decl.origin, Origin::Author);
    assert!(!color_decl.important);
}

#[test]
fn style_attr_multiple_declarations() {
    let element = make_element("div", &[("style", "color: red; margin-top: 10px")]);
    let sheets: Vec<muskitty_cssom::CssStyleSheet> = vec![];

    let declared = collect_declared_values(&element, &sheets);
    assert_eq!(declared.len(), 2);
    assert!(declared.iter().all(|d| d.from_style_attr));
}

#[test]
fn style_attr_with_important() {
    let element = make_element("div", &[("style", "color: red !important")]);
    let sheets: Vec<muskitty_cssom::CssStyleSheet> = vec![];

    let declared = collect_declared_values(&element, &sheets);
    assert_eq!(declared.len(), 1);
    assert!(declared[0].important);
    assert!(declared[0].from_style_attr);
}

#[test]
fn style_attr_combined_with_stylesheet() {
    // div style="color: green" + CSS div { color: red; }
    // style attr 应通过 from_style_attr 标志胜出
    let element = make_element("div", &[("style", "color: green")]);
    let sheet = make_sheet("div { color: red; }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    // 应收集到 2 条声明
    assert_eq!(declared.len(), 2);
    // 验证 style attr 的 order > stylesheet 的 order（后出现）
    let style_decl = declared.iter().find(|d| d.from_style_attr).unwrap();
    let sheet_decl = declared.iter().find(|d| !d.from_style_attr).unwrap();
    assert!(
        style_decl.order > sheet_decl.order,
        "style attr order should be greater"
    );
}

// —— P2-6: @media / @supports 条件评估 ——

#[test]
fn media_print_pruned_on_screen() {
    // @media print 在默认屏幕视口不命中 → 不产生声明。
    let element = make_element("div", &[]);
    let sheet = make_sheet("@media print { div { color: red; } }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert!(
        declared.is_empty(),
        "@media print must not produce declarations on screen, got {:?}",
        declared
    );
}

#[test]
fn media_all_matches() {
    let element = make_element("div", &[]);
    let sheet = make_sheet("@media all { div { color: red; } }", Origin::Author);

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn media_screen_min_width_matches_default_viewport() {
    // 默认视口 1920 宽 → min-width: 100px 命中。
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@media screen and (min-width: 100px) { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn media_min_width_pruned_on_narrow_viewport() {
    // 视口宽 50px → min-width: 100px 不命中 → 剪枝。
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@media screen and (min-width: 100px) { div { color: red; } }",
        Origin::Author,
    );

    let prepared = prepare_sheets_with_context(
        &[sheet],
        &MediaContext {
            media_type: "screen",
            viewport_w: 50.0,
            viewport_h: 1080.0,
        },
    );
    let declared = collect_declared_values_prepared(&element, &prepared);
    assert!(
        declared.is_empty(),
        "narrow viewport must prune min-width:100px rule, got {:?}",
        declared
    );
}

#[test]
fn media_unknown_feature_fails_closed() {
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@media (orientation: portrait) { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert!(
        declared.is_empty(),
        "unknown media feature must fail closed, got {:?}",
        declared
    );
}

#[test]
fn supports_display_flex_matches() {
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@supports (display: flex) { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].property, "color");
}

#[test]
fn supports_custom_property_matches() {
    // (--foo: red) → custom property → 支持
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@supports (--foo: red) { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
}

#[test]
fn supports_unknown_property_fails_closed() {
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@supports (bogus-prop: x) { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert!(
        declared.is_empty(),
        "unknown property in @supports must fail closed, got {:?}",
        declared
    );
}

#[test]
fn supports_not_inverts() {
    // not (bogus-prop: x) → 未知属性不成立，not 取反成立 → 收集
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@supports not (bogus-prop: x) { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
}

#[test]
fn supports_and_chain() {
    // (display: flex) and (color: red) → 两者都支持 → 收集
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@supports (display: flex) and (color: red) { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
}

#[test]
fn media_comma_list_is_or() {
    // @media print, screen → OR，任一命中即收集
    let element = make_element("div", &[]);
    let sheet = make_sheet(
        "@media print, screen { div { color: red; } }",
        Origin::Author,
    );

    let declared = collect_declared_values(&element, &[sheet]);
    assert_eq!(declared.len(), 1);
}

// ---- MQ-V：@media 求值矩阵（MQ L4 §3 三值逻辑 + §2.2 修饰符）----

/// 在给定视口下收集 `@media <condition> { div { color: red } }` 的声明数。
fn media_decl_count(condition: &str, viewport: (f32, f32)) -> usize {
    let element = make_element("div", &[]);
    let css = format!("@media {condition} {{ div {{ color: red; }} }}");
    let sheet = make_sheet(&css, Origin::Author);
    let media = MediaContext {
        media_type: "screen",
        viewport_w: viewport.0,
        viewport_h: viewport.1,
    };
    let prepared = prepare_sheets_with_context(&[sheet], &media);
    collect_declared_values_prepared(&element, &prepared).len()
}

/// 默认 screen 1920×1080 视口下的声明数。
fn media_count_screen(condition: &str) -> usize {
    media_decl_count(condition, (1920.0, 1080.0))
}

#[test]
fn media_not_negates_the_whole_query() {
    // §2.2：`not` 修饰整个 query —— `not print` 在 screen 上成立。
    assert_eq!(media_count_screen("not print"), 1);
    assert_eq!(media_count_screen("not screen"), 0);
    // `not all` 恒不成立（§2.2 的 error-handling 替换值）。
    assert_eq!(media_count_screen("not all"), 0);
}

#[test]
fn media_only_modifier_is_transparent() {
    // §2.2：`only` 对结果无影响（旧浏览器兼容修饰符）。
    assert_eq!(media_count_screen("only screen"), 1);
    assert_eq!(media_count_screen("only print"), 0);
    assert_eq!(media_count_screen("only all"), 1);
}

#[test]
fn media_unknown_feature_negated_stays_false() {
    // §3 三值逻辑的核心用例：unknown 经 not 仍是 unknown（→ false）。
    // 若把 unknown 当 false（二值近似），`not (orientation: bogus)` 会误判为
    // true —— 这正是 Kleene 逻辑要防的事。
    assert_eq!(media_count_screen("not (orientation: bogus)"), 0);
    assert_eq!(media_count_screen("not (unknown-feature: 1px)"), 0);
    assert_eq!(media_count_screen("not (min-resolution: 2dppx)"), 0);
}

#[test]
fn media_unknown_absorbs_and_short_circuits_or() {
    // §3：`false and unknown` = false；`true or unknown` = true；
    // 其余组合里的 unknown 让整体变成 unknown（→ false）。
    // 1920 视口下 (min-width: 100px) = true、(min-width: 9999px) = false。
    assert_eq!(
        media_count_screen("(min-width: 9999px) and (unknown-feature: x)"),
        0,
        "false AND unknown = false"
    );
    assert_eq!(
        media_count_screen("(min-width: 100px) or (unknown-feature: x)"),
        1,
        "true OR unknown = true"
    );
    assert_eq!(
        media_count_screen("(min-width: 100px) and (unknown-feature: x)"),
        0,
        "true AND unknown = unknown -> false"
    );
}

#[test]
fn media_malformed_query_is_not_all_but_recovers_at_comma() {
    // §3 Error Handling：语法不匹配 → 该 query 变 `not all`（false），
    // 但**不影响**下一个顶层逗号之后的 query。
    assert_eq!(
        media_count_screen("screen (min-width: 100px)"),
        0,
        "missing operator must be malformed"
    );
    assert_eq!(
        media_count_screen("screen (min-width: 100px), screen"),
        1,
        "recovery at the next comma"
    );
    assert_eq!(
        media_count_screen("screen and"),
        0,
        "dangling `and` is malformed"
    );
    assert_eq!(
        media_count_screen("and screen"),
        0,
        "leading `and` is malformed"
    );
}

#[test]
fn media_em_and_rem_units_resolve_against_initial_font_size() {
    // MQ L4 §4.1：媒体查询里的 em 以初始字号（16px）为基准。
    // 1920 视口：60em = 960px → true；200em = 3200px → false。
    assert_eq!(media_count_screen("(min-width: 60em)"), 1);
    assert_eq!(media_count_screen("(min-width: 200em)"), 0);
    assert_eq!(media_count_screen("(min-width: 60rem)"), 1);
    // 边界：120em = 1920px，min/max 都用 <= / >=（闭区间）。
    assert_eq!(media_count_screen("(min-width: 120em)"), 1);
    assert_eq!(media_count_screen("(max-width: 120em)"), 1);
}

#[test]
fn media_orientation_landscape_and_portrait() {
    // §4：orientation = landscape 当且仅当宽 > 高。
    assert_eq!(media_count_screen("(orientation: landscape)"), 1);
    assert_eq!(media_count_screen("(orientation: portrait)"), 0);
    assert_eq!(
        media_decl_count("(orientation: portrait)", (768.0, 1024.0)),
        1
    );
    assert_eq!(
        media_decl_count("(orientation: landscape)", (768.0, 1024.0)),
        0
    );
}

#[test]
fn media_height_features_and_units() {
    // 1080 视口高度。
    assert_eq!(media_count_screen("(min-height: 1000px)"), 1);
    assert_eq!(media_count_screen("(max-height: 1000px)"), 0);
    // 70em = 1120px ≥ 1080 → max-height 成立；60em = 960px < 1080 → 不成立。
    assert_eq!(media_count_screen("(max-height: 70em)"), 1);
    assert_eq!(media_count_screen("(min-height: 70em)"), 0);
    assert_eq!(media_count_screen("(min-height: 60em)"), 1);
}

#[test]
fn media_or_chain_of_features() {
    // §3：无类型时是 <media-condition>，允许 `or` 链。
    assert_eq!(
        media_count_screen("(min-width: 9999px) or (min-width: 100px)"),
        1,
        "false OR true = true"
    );
    assert_eq!(
        media_count_screen("(min-width: 9999px) or (min-width: 8888px)"),
        0,
        "false OR false = false"
    );
    // and 与 or 不得混用（不同产生式）→ malformed → not all。
    assert_eq!(
        media_count_screen("(min-width: 100px) and (max-width: 1px) or (min-width: 100px)"),
        0,
        "mixing and/or at one level is malformed"
    );
}

#[test]
fn media_nested_parenthesized_condition() {
    // §3：括号内可嵌套条件（`media-in-parens` 的第二种分支）。
    assert_eq!(
        media_count_screen("((min-width: 100px) and (max-width: 9999px))"),
        1
    );
    assert_eq!(media_count_screen("((min-width: 9999px))"), 0);
    assert_eq!(media_count_screen("(not (min-width: 9999px))"), 1);
}

#[test]
fn media_type_with_and_condition() {
    // `screen and (min-width: 100px)`：类型与条件以 and 连接。
    assert_eq!(media_count_screen("screen and (min-width: 100px)"), 1);
    assert_eq!(media_count_screen("screen and (min-width: 9999px)"), 0);
    assert_eq!(media_count_screen("print and (min-width: 100px)"), 0);
}

#[test]
fn media_zero_length_without_unit() {
    // 裸 0 是合法长度（`min-width: 0` 恒真）。
    assert_eq!(media_count_screen("(min-width: 0)"), 1);
}

// ---- CS-1：sheet 级字段（disabled / alternate / media 属性）----

/// 把 `media` 属性式的 media query 列表（ident 序列）挂到表上。
fn with_media_attr(
    mut sheet: muskitty_cssom::CssStyleSheet,
    idents: &[&str],
) -> muskitty_cssom::CssStyleSheet {
    use muskitty_css::tokenizer::Token;
    use muskitty_cssom::ComponentValue;
    sheet.media = idents
        .iter()
        .map(|i| ComponentValue::PreservedToken(Token::Ident((*i).to_string())))
        .collect();
    sheet
}

/// 声明的值里是否含某个 ident（用于断言胜者来源）。
fn has_ident(value: &[muskitty_cssom::ComponentValue], want: &str) -> bool {
    use muskitty_css::tokenizer::Token;
    use muskitty_cssom::ComponentValue;
    value
        .iter()
        .any(|cv| matches!(cv, ComponentValue::PreservedToken(Token::Ident(n)) if n == want))
}

#[test]
fn disabled_sheet_is_skipped() {
    // <link disabled>（HTML §4.2.4 L754-760）→ 整表不生效。
    let element = make_element("div", &[]);
    let mut sheet = make_sheet("div { color: red; }", Origin::Author);
    sheet.disabled = true;
    assert!(collect_declared_values(&element, &[sheet]).is_empty());
}

#[test]
fn alternate_sheet_is_skipped() {
    // <link rel="alternate stylesheet">：未被显式启用 → 不生效（本轮无切换 UI）。
    let element = make_element("div", &[]);
    let mut sheet = make_sheet("div { color: red; }", Origin::Author);
    sheet.alternate = true;
    assert!(collect_declared_values(&element, &[sheet]).is_empty());
}

#[test]
fn sheet_media_attribute_gates_whole_sheet() {
    // HTML §4.2.4 L841：外链的 media 属性是规定性的——不匹配整表不应用。
    let element = make_element("div", &[]);
    let print_sheet = with_media_attr(
        make_sheet("div { color: red; }", Origin::Author),
        &["print"],
    );
    let screen_sheet = with_media_attr(
        make_sheet("div { color: blue; }", Origin::Author),
        &["screen"],
    );

    assert!(
        collect_declared_values(&element, &[print_sheet]).is_empty(),
        "media=print 在 screen 环境不生效"
    );
    assert_eq!(
        collect_declared_values(&element, &[screen_sheet]).len(),
        1,
        "media=screen 生效"
    );
}

#[test]
fn sheet_media_attribute_comma_list_is_or() {
    use muskitty_css::tokenizer::Token;
    use muskitty_cssom::ComponentValue;
    let element = make_element("div", &[]);
    let mut sheet = make_sheet("div { color: red; }", Origin::Author);
    sheet.media = vec![
        ComponentValue::PreservedToken(Token::Ident("print".to_string())),
        ComponentValue::PreservedToken(Token::Comma),
        ComponentValue::PreservedToken(Token::Ident("screen".to_string())),
    ];
    assert_eq!(collect_declared_values(&element, &[sheet]).len(), 1);
}

#[test]
fn empty_sheet_media_applies() {
    // 空 media 列表 = 无媒体条件（内嵌 <style> 与无 media 属性的 link）。
    // MQ L4 §2.1："An empty media query list evaluates to true"。
    let element = make_element("div", &[]);
    let sheet = make_sheet("div { color: red; }", Origin::Author);
    assert!(sheet.media.is_empty());
    assert_eq!(collect_declared_values(&element, &[sheet]).len(), 1);
}

/// 把 media query 文本编译为 sheet.media（component value 列表）。
fn with_media_query(
    mut sheet: muskitty_cssom::CssStyleSheet,
    query: &str,
) -> muskitty_cssom::CssStyleSheet {
    use muskitty_css::parser::{parse_a_comma_separated_list_of_component_values, ComponentValue};
    use muskitty_css::tokenizer::Token;
    let groups = parse_a_comma_separated_list_of_component_values(query);
    let mut out: Vec<ComponentValue> = Vec::new();
    for (i, mut group) in groups.into_iter().enumerate() {
        if i > 0 {
            out.push(ComponentValue::PreservedToken(Token::Comma));
        }
        out.append(&mut group);
    }
    sheet.media = out;
    sheet
}

/// 在给定视口下该表的生效声明数（走 sheet 级 media 门控）。
fn sheet_media_count(query: &str, viewport: (f32, f32)) -> usize {
    let element = make_element("div", &[]);
    let sheet = with_media_query(make_sheet("div { color: red; }", Origin::Author), query);
    let media = MediaContext {
        media_type: "screen",
        viewport_w: viewport.0,
        viewport_h: viewport.1,
    };
    let prepared = prepare_sheets_with_context(&[sheet], &media);
    collect_declared_values_prepared(&element, &prepared).len()
}

#[test]
fn sheet_media_attribute_with_feature_query() {
    // CS-1d 只验证了 ident 型 media；这里补齐**特性查询**（CS-1 规划里
    // media 属性与 @media 共用同一求值器的正确性锚点）。
    assert_eq!(
        sheet_media_count("(min-width: 1000px)", (1920.0, 1080.0)),
        1
    );
    assert_eq!(
        sheet_media_count("(min-width: 3000px)", (1920.0, 1080.0)),
        0
    );
    assert_eq!(sheet_media_count("(max-width: 800px)", (640.0, 480.0)), 1);
    // 单位换算与 orientation 同样适用于 sheet 级 media。
    assert_eq!(sheet_media_count("(min-width: 60em)", (1920.0, 1080.0)), 1);
    assert_eq!(
        sheet_media_count("(orientation: landscape)", (1920.0, 1080.0)),
        1
    );
    assert_eq!(
        sheet_media_count("(orientation: portrait)", (1920.0, 1080.0)),
        0
    );
}

#[test]
fn sheet_media_attribute_negation_and_unknown() {
    // `not screen` 在 screen 环境不生效；未知特性经 not 仍不生效（三值）。
    assert_eq!(sheet_media_count("not screen", (1920.0, 1080.0)), 0);
    assert_eq!(sheet_media_count("not print", (1920.0, 1080.0)), 1);
    assert_eq!(
        sheet_media_count("not (unknown-feature: 1px)", (1920.0, 1080.0)),
        0,
        "unknown negated stays unknown -> false"
    );
}

#[test]
fn sheet_media_attribute_malformed_is_pruned() {
    // malformed media 属性 → 该表不生效（§3 error handling；不是"忽略属性"）。
    assert_eq!(
        sheet_media_count("screen (min-width: 1px)", (1920.0, 1080.0)),
        0
    );
    assert_eq!(sheet_media_count("and screen", (1920.0, 1080.0)), 0);
    // 同一列表内的合法 query 不受 malformed 邻居影响。
    assert_eq!(
        sheet_media_count("screen (min-width: 1px), screen", (1920.0, 1080.0)),
        1
    );
}

#[test]
fn disabled_first_sheet_lets_later_one_win() {
    // 被禁用的表不参与层叠，也不占 order：后一张表仍按文档序胜出。
    let element = make_element("div", &[]);
    let mut first = make_sheet("div { color: red; }", Origin::Author);
    first.disabled = true;
    let second = make_sheet("div { color: green; }", Origin::Author);

    let declared = collect_declared_values(&element, &[first, second]);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].origin, Origin::Author);
    assert!(
        has_ident(&declared[0].value, "green"),
        "胜者应来自未被禁用的第二张表"
    );
}
