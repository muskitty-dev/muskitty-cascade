//! §5 Filtering — 收集 declared values。
//!
//! 规范源: `d:\csswg\css-cascade-5\Overview.md` §5 L814-844
//!
//! 遍历 stylesheet rules，对每条匹配元素的 CssStyleRule，收集其
//! declarations 作为 DeclaredValue。条件为 false 的 @media/@supports
//! 内的 rule 在预处理时被剪枝（P2-6）。同时收集元素 inline `style`
//! 属性中的声明（§6.1 准则 4）。
//!
//! # PERF-1 选择器缓存
//!
//! [`prepare_sheets`] 在预处理阶段把每个 style rule 的选择器**一次**
//! 完成 serialize→parse→`SelectorList` 并缓存（含 specificity）；此后
//! 每个元素匹配直接复用缓存，零重复解析。`compute_styles` 走
//! prepare 一次 + [`collect_declared_values_prepared`] 逐元素复用。
//! 便捷入口 [`collect_declared_values`] 每次调用内部 prepare + collect
//! （保持旧 API，测试/单次场景使用）。

use crate::registry::lookup_property;
use crate::style::DeclaredValue;
use muskitty_css::parser::{parse_a_blocks_contents, BlockKind, ComponentValue, Rule, SimpleBlock};
use muskitty_css::tokenizer::{Numeric, Token};
use muskitty_cssom::{serialize_component_values, CssRule, CssStyleSheet, Origin};
use muskitty_selectors::matching::{matches, DomElement, Element as ElementTrait};
use muskitty_selectors::parser::parse_a_selector;
use muskitty_selectors::types::SelectorList;
use muskitty_selectors::Specificity;

/// 预处理后的 stylesheet 集合（PERF-1）。
///
/// 每个 style rule 的选择器已解析并缓存；`LayerBlock`/条件组已扁平化，
/// 每条 [`PreparedRule`] 保持文档序（嵌套子 rules 紧随父规则之后），
/// 匹配遍历与旧版递归顺序一致。
pub struct PreparedSheets {
    rules: Vec<PreparedRule>,
}

/// 一条已解析的 style rule。
struct PreparedRule {
    /// 已缓存的选择器列表（PERF-1）。
    selector_list: SelectorList,
    /// 选择器 max specificity。
    specificity: Specificity,
    /// 来源 origin。
    origin: Origin,
    /// 所属 @layer 的全局序号（P1-3）；`None` = 未分层。
    layer_order: Option<usize>,
    /// 声明块（元素无关，prepare 时克隆）。
    declarations: Vec<PreparedDecl>,
}

/// 一条声明的数据（元素无关）。
struct PreparedDecl {
    name: String,
    value: Vec<ComponentValue>,
    important: bool,
}

/// @media 条件评估上下文（P2-6）。
///
/// 预处理时用当前媒体环境判断 `@media` 内的规则是否参与匹配；不命中的
/// 整组剪枝，元素匹配阶段零开销。
#[derive(Debug, Clone, Copy)]
pub struct MediaContext {
    /// 当前媒体类型（`"screen"` / `"print"` / ...）。
    pub media_type: &'static str,
    /// 视口宽度（px）。
    pub viewport_w: f32,
    /// 视口高度（px）。
    pub viewport_h: f32,
}

impl Default for MediaContext {
    fn default() -> Self {
        // 默认屏幕视口 1920×1080。
        Self {
            media_type: "screen",
            viewport_w: 1920.0,
            viewport_h: 1080.0,
        }
    }
}

/// 预处理 stylesheet 集：每个 style rule 的选择器只解析一次。
///
/// 遍历与旧版 `collect_from_rules` 的递归一致：style rule 收集、
/// 嵌套子 rules 递归、条件组（@media/@supports/@container/@layer）穿过、
/// import/namespace 跳过。选择器解析失败的 rule 跳过但仍递归其子 rules
/// （与旧版行为一致）。
///
/// P1-3：跨全部 sheets 按文档序为每个 @layer 名分配全局序号（首次出现
/// 定序），风格规则携带其所属层的序号。
///
/// P2-6：@media 按 [`MediaContext::default`]（screen 1920×1080）评估，
/// @supports 按属性 registry 评估；不命中的条件组被剪枝。需要其他视口
/// 或媒体类型时用 [`prepare_sheets_with_context`]。
pub fn prepare_sheets(sheets: &[CssStyleSheet]) -> PreparedSheets {
    prepare_sheets_with_context(sheets, &MediaContext::default())
}

/// 带媒体上下文的预处理（P2-6）。
///
/// 同 [`prepare_sheets`]，但按给定 [`MediaContext`] 评估 `@media` 条件，
/// 剪枝不命中的规则组。
pub fn prepare_sheets_with_context(
    sheets: &[CssStyleSheet],
    media: &MediaContext,
) -> PreparedSheets {
    let mut rules = Vec::new();
    let mut layers = LayerTracker::new();
    for sheet in sheets {
        prepare_rules(
            &sheet.css_rules,
            sheet.origin,
            &mut layers,
            media,
            &mut rules,
        );
    }
    PreparedSheets { rules }
}

/// @layer 全局序号分配器（P1-3）。
///
/// 按文档序首次出现的层名分配递增序号；匿名层每次出现分配新序号。
/// 嵌套层采用扁平近似（`docs/audit-2026-08-08-full-scan.md` P1-3）：内层
/// 块以其自身名/匿名身份获得独立序号，不拼接父层名。
struct LayerTracker {
    /// 已见层名 → 序号。
    by_name: std::collections::HashMap<String, usize>,
    /// 下一个待分配的序号。
    next: usize,
    /// 当前嵌套层序号栈（`None` 不入栈；栈空 = 顶层）。
    stack: Vec<Option<usize>>,
}

impl LayerTracker {
    fn new() -> Self {
        Self {
            by_name: std::collections::HashMap::new(),
            next: 0,
            stack: Vec::new(),
        }
    }

    /// 声明（或再次引用）一个层，返回其全局序号。首次出现分配新序号；
    /// 匿名层（`None`）总是分配新序号。
    fn declare(&mut self, name: Option<&str>) -> usize {
        match name {
            Some(n) => *self.by_name.entry(n.to_string()).or_insert_with(|| {
                let i = self.next;
                self.next += 1;
                i
            }),
            None => {
                let i = self.next;
                self.next += 1;
                i
            }
        }
    }

    /// 进入一个层块：登记序号并压栈。
    fn enter(&mut self, name: Option<&str>) {
        let idx = self.declare(name);
        self.stack.push(Some(idx));
    }

    /// 退出当前层块。
    fn exit(&mut self) {
        self.stack.pop();
    }

    /// 当前层序号（顶层 = `None`）。
    fn current(&self) -> Option<usize> {
        self.stack.last().copied().flatten()
    }
}

/// 递归遍历 rules，构建扁平化的 [`PreparedRule`] 列表。
fn prepare_rules(
    rules: &[CssRule],
    origin: Origin,
    layers: &mut LayerTracker,
    media: &MediaContext,
    out: &mut Vec<PreparedRule>,
) {
    for rule in rules {
        match rule {
            CssRule::Style(style_rule) => {
                // §5 L829: "Its declaration's selector matches the element"
                let selector_str = serialize_component_values(&style_rule.selectors);
                if let Ok(selector_list) = parse_a_selector(&selector_str) {
                    let specificity = selector_list.specificity_max();
                    let declarations = style_rule
                        .style
                        .declarations
                        .iter()
                        .map(|d| PreparedDecl {
                            name: d.name.clone(),
                            value: d.value.clone(),
                            important: d.important,
                        })
                        .collect();
                    out.push(PreparedRule {
                        selector_list,
                        specificity,
                        origin,
                        layer_order: layers.current(),
                        declarations,
                    });
                }
                // 递归处理 CSS nesting 子 rules
                prepare_rules(&style_rule.css_rules, origin, layers, media, out);
            }
            CssRule::Media(r) => {
                // P2-6: @media 条件不命中 → 整组剪枝（元素匹配阶段零开销）。
                if eval_media_query(media, &r.condition) {
                    prepare_rules(&r.css_rules, origin, layers, media, out);
                }
            }
            CssRule::Supports(r) => {
                // P2-6: @supports 条件按属性 registry 评估，不命中 → 剪枝。
                if eval_supports_condition(&r.condition) {
                    prepare_rules(&r.css_rules, origin, layers, media, out);
                }
            }
            CssRule::Container(r) => {
                // @container 条件需容器查询（依赖布局反馈），恒 true +
                // 推迟（见 audit P2-6 范围边界）。
                prepare_rules(&r.css_rules, origin, layers, media, out);
            }
            CssRule::LayerBlock(r) => {
                // P1-3: 进入层块，层内规则携带该层序号
                layers.enter(r.name.as_deref());
                prepare_rules(&r.css_rules, origin, layers, media, out);
                layers.exit();
            }
            CssRule::LayerStatement(r) => {
                // P1-3: statement 形式声明层名（定序），无子规则
                for name in &r.names {
                    layers.declare(Some(name));
                }
            }
            CssRule::Scope(r) => {
                // @scope 是作用域容器，子规则照常参与元素匹配
                prepare_rules(&r.css_rules, origin, layers, media, out);
            }
            CssRule::Other(r) => {
                prepare_rules(&r.child_rules, origin, layers, media, out);
            }
            // 非元素匹配 rule 跳过。P2-14：@keyframes 的 Keyframe 块不再
            // 当普通 style rule 参与匹配（消除数据污染）；@font-face /
            // @page / @counter-style / @property 同样与元素匹配无关。
            CssRule::Import(_)
            | CssRule::FontFace(_)
            | CssRule::Page(_)
            | CssRule::Keyframes(_)
            | CssRule::Keyframe(_)
            | CssRule::Namespace(_)
            | CssRule::CounterStyle(_)
            | CssRule::Property(_) => {}
        }
    }
}

/// §5: 收集元素的所有 declared values。
///
/// 遍历所有 stylesheet，对每条匹配 `element` 的 style rule，
/// 收集其 declarations。递归处理嵌套 rules 和条件 group rules
/// （@media/@supports/@container/@layer）。
/// 最后收集元素 inline `style` 属性中的声明。
///
/// **简化**：条件 group rules 的条件评估推迟，当前无条件收集
/// 所有嵌套 rules。
///
/// 便捷入口：每次调用内部 [`prepare_sheets`]（选择器重解析）。热路径
/// （`compute_styles`）应改为 prepare 一次 + [`collect_declared_values_prepared`]。
pub fn collect_declared_values(
    element: &DomElement,
    sheets: &[CssStyleSheet],
) -> Vec<DeclaredValue> {
    let prepared = prepare_sheets(sheets);
    collect_declared_values_prepared(element, &prepared)
}

/// §5: 用已预处理的 sheets 收集元素 declared values（PERF-1）。
///
/// 遍历 `prepared` 中按文档序排列的 style rules，匹配即收集声明；
/// 匹配过程中零分配（选择器已缓存）。最后收集元素 inline `style`
/// 属性中的声明（§6.1 准则 4）。
pub fn collect_declared_values_prepared(
    element: &DomElement,
    prepared: &PreparedSheets,
) -> Vec<DeclaredValue> {
    let mut result = Vec::new();
    let mut order = 0usize;

    for rule in &prepared.rules {
        if matches(&rule.selector_list, element) {
            for decl in &rule.declarations {
                order += 1;
                push_declared(
                    &mut result,
                    order,
                    &decl.name,
                    &decl.value,
                    decl.important,
                    rule.origin,
                    rule.specificity,
                    rule.layer_order,
                    false,
                );
            }
        }
    }

    // §6.1 准则 4: 收集 inline style 属性的声明
    collect_from_style_attr(element, &mut order, &mut result);

    result
}

/// 从元素 inline `style` 属性收集声明。
///
/// inline style 声明的 specificity 为 (1,0,0,0)（最高优先级），
/// origin 为 Author，from_style_attr = true。
fn collect_from_style_attr(
    element: &DomElement,
    order: &mut usize,
    result: &mut Vec<DeclaredValue>,
) {
    let style_str = match element.get_attribute("style") {
        Some(s) => s,
        None => return,
    };

    let block_contents = parse_a_blocks_contents(&style_str);
    // §6.1 准则 4: inline style 通过 from_style_attr 标志单独排序，
    // specificity 本身为 (0,0,0)（准则 3 不会额外加权）
    let specificity = Specificity::new(0, 0, 0);

    for rule in &block_contents.rules {
        if let Rule::Declarations(decls) = rule {
            for decl in decls {
                *order += 1;
                push_declared(
                    result,
                    *order,
                    &decl.name,
                    &decl.value,
                    decl.important,
                    Origin::Author,
                    specificity,
                    // inline style 不在任何 @layer 内
                    None,
                    true,
                );
            }
        }
    }
}

/// 归一化属性名并过滤未知属性（P2-2/P2-21）。
///
/// - 非 `--*` 属性名统一转小写（CSS 属性名大小写不敏感，§6.3.4）。
/// - 未注册且非 `--*` 的属性丢弃（不进入 cascade；`--*` 是自定义属性，
///   大小写敏感，原样保留）。
///
/// 返回 `Some(归一化名)`；`None` 表示该声明应被丢弃。
fn normalize_property_name(name: &str) -> Option<String> {
    if name.starts_with("--") {
        Some(name.to_string())
    } else if lookup_property(name).is_some() {
        Some(name.to_ascii_lowercase())
    } else {
        None
    }
}

/// 收集一条声明的 DeclaredValue（sheets 与 inline style 共用）。
///
/// 统一处理：
/// - C1：简写展开（margin/padding/flex/background/font/gap）→ 多条长属性
///   声明（共享同一 `order`/specificity/importance）。简写名
///   （margin/padding/background/font）未注册，必须在
///   [`normalize_property_name`] 过滤前展开。展开为空 = 简写值无效 → 丢弃。
/// - P2-2/P2-21：其余属性名归一化 + 未知属性过滤（[`normalize_property_name`]）。
///
/// `order` 由调用方递增后传入。
#[allow(clippy::too_many_arguments)]
fn push_declared(
    result: &mut Vec<DeclaredValue>,
    order: usize,
    name: &str,
    value: &[ComponentValue],
    important: bool,
    origin: Origin,
    specificity: Specificity,
    layer_order: Option<usize>,
    from_style_attr: bool,
) {
    if let Some(expanded) = expand_shorthand(name, value) {
        for (p, v) in expanded {
            result.push(DeclaredValue {
                property: p.to_string(),
                value: v,
                important,
                origin,
                specificity,
                order,
                layer_order,
                from_style_attr,
            });
        }
        return;
    }

    let property = match normalize_property_name(name) {
        Some(p) => p,
        None => return,
    };
    result.push(DeclaredValue {
        property,
        value: value.to_vec(),
        important,
        origin,
        specificity,
        order,
        layer_order,
        from_style_attr,
    });
}

/// 将 `gap` 简写值拆分为 `(row-gap, column-gap)` 分量（P2-9）。
///
/// CSS Box Alignment §6.2: `gap: <row-gap> <column-gap>?`。单值时双轴复用；
/// 跳过空白 token。
fn split_gap_value(value: &[ComponentValue]) -> (Vec<ComponentValue>, Vec<ComponentValue>) {
    let parts: Vec<&ComponentValue> = value
        .iter()
        .filter(|cv| !matches!(cv, ComponentValue::PreservedToken(Token::Whitespace)))
        .collect();
    match parts.as_slice() {
        [first] => (vec![(*first).clone()], vec![(*first).clone()]),
        [first, second, ..] => (vec![(*first).clone()], vec![(*second).clone()]),
        [] => (vec![], vec![]),
    }
}

// ── C1: 简写属性 → 长属性展开 ──────────────────────────────────

/// 简写属性展开分派。非简写返回 `None`；简写返回展开后的长属性声明列表
/// （可能为空 = 简写值无效，调用方丢弃整条声明）。
///
/// 属性名大小写不敏感（CSS §6.3.4）；`--*` 自定义属性不含简写名，天然不命中。
fn expand_shorthand(
    name: &str,
    value: &[ComponentValue],
) -> Option<Vec<(&'static str, Vec<ComponentValue>)>> {
    if name.eq_ignore_ascii_case("margin") {
        Some(expand_box_4(value, BOX_MARGIN))
    } else if name.eq_ignore_ascii_case("padding") {
        Some(expand_box_4(value, BOX_PADDING))
    } else if name.eq_ignore_ascii_case("flex") {
        Some(expand_flex(value))
    } else if name.eq_ignore_ascii_case("background") {
        Some(expand_background(value))
    } else if name.eq_ignore_ascii_case("font") {
        Some(expand_font(value))
    } else if name.eq_ignore_ascii_case("gap") {
        Some(expand_gap(value))
    } else {
        None
    }
}

/// margin 四向长属性名。
const BOX_MARGIN: [&str; 4] = ["margin-top", "margin-right", "margin-bottom", "margin-left"];
/// padding 四向长属性名。
const BOX_PADDING: [&str; 4] = [
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
];

/// `margin`/`padding` 简写（CSS Box Model L3 §3.1 / §4.2）。
///
/// 1/2/3/4 值拆分：`[a]` 全同；`[a b]` 上下=a 左右=b；`[a b c]` 上=a 左右=b
/// 下=c；`[a b c d]` 顺时针 top/right/bottom/left。0 或 ≥5 分量 → 无效
/// （返回空，声明丢弃）。单全局关键字（如 `margin: inherit`）→ 每个长属性
/// 取该关键字（CSS Cascade L5 §3.2）。
fn expand_box_4(
    value: &[ComponentValue],
    props: [&'static str; 4],
) -> Vec<(&'static str, Vec<ComponentValue>)> {
    if let Some(kw) = single_global_keyword(value) {
        return props.iter().map(|p| (*p, vec![ident_token(&kw)])).collect();
    }
    let parts = non_ws_parts(value);
    let at = |i: usize| vec![(*parts[i]).clone()];
    match parts.len() {
        1 => vec![
            (props[0], at(0)),
            (props[1], at(0)),
            (props[2], at(0)),
            (props[3], at(0)),
        ],
        2 => vec![
            (props[0], at(0)),
            (props[1], at(1)),
            (props[2], at(0)),
            (props[3], at(1)),
        ],
        3 => vec![
            (props[0], at(0)),
            (props[1], at(1)),
            (props[2], at(2)),
            (props[3], at(1)),
        ],
        4 => vec![
            (props[0], at(0)),
            (props[1], at(1)),
            (props[2], at(2)),
            (props[3], at(3)),
        ],
        _ => vec![],
    }
}

/// `flex` 简写（CSS Flexbox L1 §7.1）。
///
/// 关键字：`none` → `0 0 auto`；`auto` → `1 1 auto`。分量形式
/// `<grow> <shrink>? <basis>?`：省略时 grow=1、shrink=1、basis=0%
/// （注意：与初始值 `0 1 auto` 不同——简写省略走 0%）。`<basis>` 可为长度
/// 或百分比。grow/shrink 恒为数字；非法分量（函数/关键字混入）→ 无效。
fn expand_flex(value: &[ComponentValue]) -> Vec<(&'static str, Vec<ComponentValue>)> {
    if let Some(kw) = single_global_keyword(value) {
        return ["flex-grow", "flex-shrink", "flex-basis"]
            .iter()
            .map(|p| (*p, vec![ident_token(&kw)]))
            .collect();
    }
    let parts = non_ws_parts(value);
    if parts.len() == 1 {
        if let ComponentValue::PreservedToken(Token::Ident(s)) = parts[0] {
            if s.eq_ignore_ascii_case("none") {
                return vec![
                    ("flex-grow", vec![number_token(0.0)]),
                    ("flex-shrink", vec![number_token(0.0)]),
                    ("flex-basis", vec![ident_token("auto")]),
                ];
            }
            if s.eq_ignore_ascii_case("auto") {
                return vec![
                    ("flex-grow", vec![number_token(1.0)]),
                    ("flex-shrink", vec![number_token(1.0)]),
                    ("flex-basis", vec![ident_token("auto")]),
                ];
            }
        }
    }
    let mut grow = 1.0;
    let mut shrink = 1.0;
    let mut basis: Vec<ComponentValue> = vec![percentage_token(0.0)];
    match parts.as_slice() {
        [a] => {
            if let Some(n) = number_of(a) {
                grow = n;
            } else if is_length_pct(a) {
                basis = vec![(*a).clone()];
            } else {
                return vec![];
            }
        }
        [a, b] => {
            let Some(na) = number_of(a) else {
                return vec![];
            };
            grow = na;
            if let Some(nb) = number_of(b) {
                shrink = nb;
            } else if is_length_pct(b) {
                basis = vec![(*b).clone()];
            } else {
                return vec![];
            }
        }
        [a, b, c] => {
            let (Some(na), Some(nb)) = (number_of(a), number_of(b)) else {
                return vec![];
            };
            grow = na;
            shrink = nb;
            if is_length_pct(c) {
                basis = vec![(*c).clone()];
            } else {
                return vec![];
            }
        }
        _ => return vec![],
    }
    vec![
        ("flex-grow", vec![number_token(grow)]),
        ("flex-shrink", vec![number_token(shrink)]),
        ("flex-basis", basis),
    ]
}

/// `background` 简写（CSS Backgrounds L3 §8.10）：仅展开 `background-color`。
///
/// 其余子属性（image/position/size/repeat/origin/clip/attachment）当前无消费
/// 方，推迟到有消费方时再展开。无颜色分量 → `background-color: transparent`
/// （初始值）。单全局关键字 → `background-color` 取该关键字。
fn expand_background(value: &[ComponentValue]) -> Vec<(&'static str, Vec<ComponentValue>)> {
    if let Some(kw) = single_global_keyword(value) {
        return vec![("background-color", vec![ident_token(&kw)])];
    }
    let color = non_ws_parts(value)
        .into_iter()
        .find(|cv| is_background_color(cv));
    let color_cv = match color {
        Some(c) => (*c).clone(),
        None => ident_token("transparent"),
    };
    vec![("background-color", vec![color_cv])]
}

/// `font` 简写（CSS Fonts L3 §3.8）：仅展开 `font-size`（+ 可选 `line-height`）。
///
/// style/variant/weight/stretch/family 无消费方，推迟。解析：跳过可选
/// style/variant/weight 前缀关键字，取 `<font-size>`（长度/百分比/绝对尺寸
/// 关键字），可选 `/ <line-height>`。缺 font-size → 无效（返回空）。
fn expand_font(value: &[ComponentValue]) -> Vec<(&'static str, Vec<ComponentValue>)> {
    if let Some(kw) = single_global_keyword(value) {
        return vec![("font-size", vec![ident_token(&kw)])];
    }
    let parts = non_ws_parts(value);
    let mut i = 0;
    while i < parts.len() && is_font_pre_keyword(parts[i]) {
        i += 1;
    }
    let size = match parts.get(i) {
        Some(cv) if is_font_size(cv) => cv,
        _ => return vec![], // 缺 font-size → 无效简写
    };
    i += 1;
    let mut result = vec![("font-size", vec![(*size).clone()])];
    // 可选 `/ <line-height>`
    if i < parts.len() && matches!(parts[i], ComponentValue::PreservedToken(Token::Delim('/'))) {
        if let Some(lh) = parts.get(i + 1) {
            result.push(("line-height", vec![(*lh).clone()]));
        }
    }
    result
}

/// `gap` 简写（CSS Box Alignment L3 §6.2）：复用 [`split_gap_value`]（P2-9）。
fn expand_gap(value: &[ComponentValue]) -> Vec<(&'static str, Vec<ComponentValue>)> {
    let (row, col) = split_gap_value(value);
    vec![("row-gap", row), ("column-gap", col)]
}

/// 取非空白分量（借用）。
fn non_ws_parts(value: &[ComponentValue]) -> Vec<&ComponentValue> {
    value
        .iter()
        .filter(|cv| !matches!(cv, ComponentValue::PreservedToken(Token::Whitespace)))
        .collect()
}

/// 若值是单个全局关键字（inherit/initial/unset/revert/revert-layer），返回它。
///
/// CSS Cascade L5 §3.2: 简写取全局关键字时，每个长属性均取该关键字。
fn single_global_keyword(value: &[ComponentValue]) -> Option<String> {
    let parts = non_ws_parts(value);
    if parts.len() == 1 {
        if let ComponentValue::PreservedToken(Token::Ident(s)) = parts[0] {
            if is_global_keyword(s) {
                return Some(s.clone());
            }
        }
    }
    None
}

/// 是否为全局关键字。
fn is_global_keyword(s: &str) -> bool {
    s.eq_ignore_ascii_case("inherit")
        || s.eq_ignore_ascii_case("initial")
        || s.eq_ignore_ascii_case("unset")
        || s.eq_ignore_ascii_case("revert")
        || s.eq_ignore_ascii_case("revert-layer")
}

/// background 中可作颜色的分量：hash / 颜色函数 / 非背景关键字 ident。
///
/// 反集法（C1）：背景关键字集合封闭（~20 个），不在集合内的 ident 即候选
/// 颜色（red/green/transparent/currentcolor 等），无需完整颜色名表。
/// 单全局关键字由 [`single_global_keyword`] 先行处理，不会进入此处。
fn is_background_color(cv: &ComponentValue) -> bool {
    match cv {
        ComponentValue::PreservedToken(Token::Hash(..)) => true,
        ComponentValue::PreservedToken(Token::Ident(s)) => !BACKGROUND_NON_COLOR
            .iter()
            .any(|k| s.eq_ignore_ascii_case(k)),
        ComponentValue::Function(f) => {
            f.name.eq_ignore_ascii_case("rgb")
                || f.name.eq_ignore_ascii_case("rgba")
                || f.name.eq_ignore_ascii_case("hsl")
                || f.name.eq_ignore_ascii_case("hsla")
                || f.name.eq_ignore_ascii_case("hwb")
                || f.name.eq_ignore_ascii_case("lab")
                || f.name.eq_ignore_ascii_case("lch")
                || f.name.eq_ignore_ascii_case("oklab")
                || f.name.eq_ignore_ascii_case("oklch")
                || f.name.eq_ignore_ascii_case("color")
        }
        _ => false,
    }
}

/// background 中非颜色的关键字（反集法识别颜色的排除集）。
const BACKGROUND_NON_COLOR: &[&str] = &[
    "none",
    "repeat",
    "repeat-x",
    "repeat-y",
    "no-repeat",
    "space",
    "round",
    "scroll",
    "fixed",
    "local",
    "border-box",
    "padding-box",
    "content-box",
    "cover",
    "contain",
    "center",
    "top",
    "bottom",
    "left",
    "right",
];

/// font 简写 style/variant/weight 前缀关键字（跳过）。
fn is_font_pre_keyword(cv: &ComponentValue) -> bool {
    match cv {
        ComponentValue::PreservedToken(Token::Ident(s)) => {
            s.eq_ignore_ascii_case("normal")
                || s.eq_ignore_ascii_case("italic")
                || s.eq_ignore_ascii_case("oblique")
                || s.eq_ignore_ascii_case("small-caps")
                || s.eq_ignore_ascii_case("bold")
                || s.eq_ignore_ascii_case("bolder")
                || s.eq_ignore_ascii_case("lighter")
        }
        // 数字字重（font-weight: 100-900，可含中间值）
        ComponentValue::PreservedToken(Token::Number(n)) => {
            n.value.fract() == 0.0 && (1.0..=1000.0).contains(&n.value)
        }
        _ => false,
    }
}

/// `<font-size>`：长度 / 百分比 / 绝对尺寸关键字（CSS Fonts L3 §3.5）。
fn is_font_size(cv: &ComponentValue) -> bool {
    match cv {
        ComponentValue::PreservedToken(Token::Dimension(..))
        | ComponentValue::PreservedToken(Token::Percentage(..)) => true,
        ComponentValue::PreservedToken(Token::Ident(s)) => {
            s.eq_ignore_ascii_case("xx-small")
                || s.eq_ignore_ascii_case("x-small")
                || s.eq_ignore_ascii_case("small")
                || s.eq_ignore_ascii_case("medium")
                || s.eq_ignore_ascii_case("large")
                || s.eq_ignore_ascii_case("x-large")
                || s.eq_ignore_ascii_case("xx-large")
                || s.eq_ignore_ascii_case("xxx-large")
                || s.eq_ignore_ascii_case("larger")
                || s.eq_ignore_ascii_case("smaller")
        }
        _ => false,
    }
}

/// 取 `<number>` 分量的值（非 number 返回 `None`）。
fn number_of(cv: &ComponentValue) -> Option<f64> {
    match cv {
        ComponentValue::PreservedToken(Token::Number(n)) => Some(n.value),
        _ => None,
    }
}

/// 是否为 `<length>` 或 `<percentage>` 分量。
fn is_length_pct(cv: &ComponentValue) -> bool {
    matches!(
        cv,
        ComponentValue::PreservedToken(Token::Dimension(..))
            | ComponentValue::PreservedToken(Token::Percentage(..))
    )
}

/// 构造 Ident token。
fn ident_token(s: &str) -> ComponentValue {
    ComponentValue::PreservedToken(Token::Ident(s.to_string()))
}

/// 构造 Number token。
fn number_token(v: f64) -> ComponentValue {
    ComponentValue::PreservedToken(Token::Number(Numeric {
        value: v,
        is_integer: v.fract() == 0.0,
    }))
}

/// 构造 Percentage token。
fn percentage_token(v: f64) -> ComponentValue {
    ComponentValue::PreservedToken(Token::Percentage(Numeric {
        value: v,
        is_integer: v.fract() == 0.0,
    }))
}

// ── P2-6: @media / @supports 条件评估 ─────────────────────────────

/// 评估 `@media` 条件（P2-6）。
///
/// 支持子集：媒体类型 `all` / `screen` / `print`；feature
/// `(min/max-width/height: <px>)`；逻辑 `not` / `and`（`or` 少见，
/// 主要用逗号分隔列表 = OR）。未知类型 / 未知 feature / 未知语法 →
/// `false`（fail-closed）。
fn eval_media_query(media: &MediaContext, condition: &[ComponentValue]) -> bool {
    // 顶层逗号分隔的 media query 列表：任一命中即整体 true。
    for query in split_on_commas(condition) {
        if eval_media_query_list(media, query) {
            return true;
        }
    }
    false
}

/// 求值单个 media query（无顶层逗号）。
fn eval_media_query_list(media: &MediaContext, query: &[ComponentValue]) -> bool {
    let mut result: Option<bool> = None;
    let mut pending_op: Option<&'static str> = None;
    let mut negate = false;

    for cv in query {
        match cv {
            ComponentValue::PreservedToken(Token::Whitespace) => {}
            ComponentValue::PreservedToken(Token::Ident(name)) => {
                if name.eq_ignore_ascii_case("not") {
                    negate = true;
                } else if name.eq_ignore_ascii_case("and") {
                    pending_op = Some("and");
                } else if name.eq_ignore_ascii_case("or") {
                    pending_op = Some("or");
                } else {
                    let mut cond = eval_media_type(media.media_type, name);
                    if negate {
                        cond = !cond;
                        negate = false;
                    }
                    combine_condition(&mut result, pending_op.take(), cond);
                }
            }
            ComponentValue::SimpleBlock(b) => {
                let mut cond = eval_media_feature(media, b);
                if negate {
                    cond = !cond;
                    negate = false;
                }
                combine_condition(&mut result, pending_op.take(), cond);
            }
            _ => return false, // 未知语法 → fail-closed
        }
    }

    result.unwrap_or(false)
}

/// 求值媒体类型名（大小写不敏感）；未知类型 fail-closed。
fn eval_media_type(current: &str, name: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "all" => true,
        "screen" => current == "screen",
        "print" => current == "print",
        _ => false,
    }
}

/// 求值媒体 feature（`(min-width: Npx)` 等）；未知 feature fail-closed。
fn eval_media_feature(media: &MediaContext, b: &SimpleBlock) -> bool {
    if b.kind != BlockKind::Paren {
        return false;
    }
    let name = match extract_decl_property(b) {
        Some(n) => n,
        None => return false,
    };
    let value = match extract_media_px(&b.value) {
        Some(v) => v,
        None => return false,
    };
    match name.as_str() {
        "min-width" => media.viewport_w >= value,
        "max-width" => media.viewport_w <= value,
        "min-height" => media.viewport_h >= value,
        "max-height" => media.viewport_h <= value,
        _ => false,
    }
}

/// 提取 `(prop: value)` 括号块中的属性名（首个非空白 Ident）；未知 → `None`。
fn extract_decl_property(b: &SimpleBlock) -> Option<String> {
    for cv in &b.value {
        match cv {
            ComponentValue::PreservedToken(Token::Whitespace) => {}
            ComponentValue::PreservedToken(Token::Ident(s)) => return Some(s.clone()),
            _ => return None,
        }
    }
    None
}

/// 从 component value 列表提取首个 px [`Token::Dimension`]。
fn extract_media_px(values: &[ComponentValue]) -> Option<f32> {
    for cv in values {
        if let ComponentValue::PreservedToken(Token::Dimension(n, unit)) = cv {
            if unit.eq_ignore_ascii_case("px") {
                return Some(n.value as f32);
            }
        }
    }
    None
}

/// 顶层逗号分隔为多个分段（逗号 = OR）。
fn split_on_commas(cvs: &[ComponentValue]) -> Vec<&[ComponentValue]> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (i, cv) in cvs.iter().enumerate() {
        if matches!(cv, ComponentValue::PreservedToken(Token::Comma)) {
            segments.push(&cvs[start..i]);
            start = i + 1;
        }
    }
    segments.push(&cvs[start..]);
    segments
}

/// 累积逻辑条件：首项直接置入；后续按 `and` / `or` 连接。
fn combine_condition(result: &mut Option<bool>, op: Option<&'static str>, cond: bool) {
    match result {
        None => *result = Some(cond),
        Some(prev) => match op {
            Some("and") => *result = Some(*prev && cond),
            Some("or") => *result = Some(*prev || cond),
            // 无逻辑连接符时以最新条件为准（简化）。
            _ => *result = Some(cond),
        },
    }
}

/// 评估 `@supports` 条件（P2-6）。
///
/// `(prop: value)` → registry 含 prop 或 `--*` 前缀 → true；
/// `not` / `and` / `or` 逻辑；未知 → `false`（fail-closed）。
fn eval_supports_condition(condition: &[ComponentValue]) -> bool {
    let mut result: Option<bool> = None;
    let mut pending_op: Option<&'static str> = None;
    let mut negate = false;

    for cv in condition {
        match cv {
            ComponentValue::PreservedToken(Token::Whitespace) => {}
            ComponentValue::PreservedToken(Token::Ident(name)) => {
                if name.eq_ignore_ascii_case("not") {
                    negate = true;
                } else if name.eq_ignore_ascii_case("and") {
                    pending_op = Some("and");
                } else if name.eq_ignore_ascii_case("or") {
                    pending_op = Some("or");
                } else {
                    return false; // 未知语法 → fail-closed
                }
            }
            ComponentValue::SimpleBlock(b) => {
                let mut cond = eval_supports_feature(b);
                if negate {
                    cond = !cond;
                    negate = false;
                }
                combine_condition(&mut result, pending_op.take(), cond);
            }
            _ => return false,
        }
    }

    result.unwrap_or(false)
}

/// 求值 `@supports` 的 `(prop: value)` 声明：属性已注册或为 `--*`。
fn eval_supports_feature(b: &SimpleBlock) -> bool {
    if b.kind != BlockKind::Paren {
        return false;
    }
    let name = match extract_decl_property(b) {
        Some(n) => n,
        None => return false,
    };
    // 自定义属性（大小写敏感）恒支持；普通属性大小写不敏感。
    if name.starts_with("--") {
        return true;
    }
    lookup_property(&name.to_ascii_lowercase()).is_some()
}
