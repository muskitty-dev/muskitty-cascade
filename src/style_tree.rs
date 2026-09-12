//! §4.3/§4.4 — 整树 computed style 计算。
//!
//! 规范源: CSS Cascading Level 5 §4.3 "Computed Value" / §4.2 "Specified Value"
//!         CSS Values Level 4 §5.5（em/rem 相对长度）
//!
//! [`compute_styles`] 对整棵 DOM 树做单次自顶向下遍历，为每个元素计算
//! `ComputedStyle`。与逐个元素手写 filter→cascade→defaulting→compute 相比：
//!
//! - **单次 cascade**（PERF-2）：`collect_declared_values` + `cascade_for_element`
//!   每元素只跑一次，`--*` 自定义属性表从同一份 cascade 分组派生，不再
//!   调用会重复级联的 `collect_custom_properties`。
//! - **font-size 传播**（P0-1）：两步算法——先用父 font-size 作 em/百分比
//!   基准算本元素 font-size，再用本元素 font-size 作其余属性的 em 基准；
//!   rem 基准（根元素 font-size）自根向下传播。
//!
//! 参考实现：Servo `components/style/cascade.rs::compute_style` + 两遍
//! font-size（先字体后其余属性，em 语义 = 元素自身 font-size）。

use crate::cascade::{cascade_for_element, cascade_winner};
use crate::compute::{
    compute_value_with, needs_no_resolution, ComputeContext, CustomPropertySource,
};
use crate::custom_properties::is_css_wide_keyword;
use crate::defaulting::{apply_defaulting, is_defaulting_keyword};
use crate::filter::{
    collect_declared_values_prepared, prepare_sheets_with_context, MediaContext, PreparedSheets,
};
use crate::registry::{lookup_property, PropertyDefinition, BUILTIN_PROPERTIES};
use crate::style::{ComputedStyle, ComputedValue, DeclaredValue};
use muskitty_css::parser::ComponentValue;
use muskitty_css::tokenizer::{Numeric, Token};
use muskitty_cssom::CssStyleSheet;
use muskitty_dom::{Node, NodeKind};
use muskitty_selectors::matching::DomElement;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

/// 浏览器默认 font-size（px）。CSS 初始值 `medium` = 16px。
const DEFAULT_FONT_SIZE: f64 = 16.0;

/// 整树样式计算的视口选项（vw/vh 单位解析需要）。
#[derive(Debug, Clone, Copy)]
pub struct StyleTreeOptions {
    /// 视口宽度（px）。
    pub viewport_width: f64,
    /// 视口高度（px）。
    pub viewport_height: f64,
}

impl Default for StyleTreeOptions {
    fn default() -> Self {
        Self {
            viewport_width: 1920.0,
            viewport_height: 1080.0,
        }
    }
}

/// §4.3: 计算整棵 DOM 树的 computed style。
///
/// 返回 `HashMap<usize, ComputedStyle>`，key 为 `Rc::as_ptr(node) as usize`
/// （与现有跨 crate 约定一致；opaque 句柄化见
/// docs/security-audit-2026-08-02.md M-1，推迟到架构重构）。
/// 非 Element 节点（Document/Text/Comment）不产生样式条目。
///
/// 继承链：`--*` 自定义属性与继承属性（color/font-* 等）自父向子传递；
/// font-size 按两步算法传播（见模块文档）。
pub fn compute_styles(
    root: &Rc<RefCell<Node>>,
    sheets: &[CssStyleSheet],
    options: &StyleTreeOptions,
) -> HashMap<usize, ComputedStyle> {
    // PERF-1: 选择器解析一次，整树每个元素复用 prepared sheets。
    // media query 求值以调用方视口为准（MediaContext::default() 亦为 1920×1080，
    // 与 StyleTreeOptions::default() 一致 → 默认行为不变）。
    let media = MediaContext {
        media_type: "screen",
        viewport_w: options.viewport_width as f32,
        viewport_h: options.viewport_height as f32,
    };
    let prepared = prepare_sheets_with_context(sheets, &media);
    let mut styles = HashMap::new();
    walk(
        root,
        &prepared,
        options,
        None, // 根元素无父级自定义属性来源
        None,
        DEFAULT_FONT_SIZE,
        None,
        &mut styles,
    );
    styles
}

/// 自顶向下遍历 DOM 树。
///
/// - `prepared`：已预处理的 sheets（PERF-1，选择器已缓存、整树复用）。
/// - `parent_source`：父级 `--*` 来源（链式继承，PERF-4；根为 `None`）。
/// - `parent_style`：父元素 ComputedStyle（继承属性）。
/// - `parent_font_size`：父元素 font-size（px），根为浏览器默认 16px。
/// - `root_font_size`：根元素 font-size（px）；根元素自身计算时为 `None`。
#[allow(clippy::too_many_arguments)]
fn walk<'a>(
    node: &Rc<RefCell<Node>>,
    prepared: &PreparedSheets,
    options: &StyleTreeOptions,
    parent_source: Option<&'a CustomPropertySource<'a>>,
    parent_style: Option<&ComputedStyle>,
    parent_font_size: f64,
    root_font_size: Option<f64>,
    styles: &mut HashMap<usize, ComputedStyle>,
) {
    // 非 Element 节点（Document/Text/Comment）：不计算样式，原样向下递归。
    if !matches!(&node.borrow().kind, NodeKind::Element(_)) {
        let children: Vec<Rc<RefCell<Node>>> = node.borrow().child_nodes().to_vec();
        for child in &children {
            walk(
                child,
                prepared,
                options,
                parent_source,
                parent_style,
                parent_font_size,
                root_font_size,
                styles,
            );
        }
        return;
    }

    let addr = Rc::as_ptr(node) as usize;

    // 每元素一次 filter + cascade，派生本元素 `--*` 表 + 属性组（PERF-2）。
    let element = DomElement::new(Rc::clone(node));
    let declared = collect_declared_values_prepared(&element, prepared);
    let groups = cascade_for_element(declared);
    let own_props = derive_own_custom_props(&groups);
    // 链式来源：本元素声明优先，未声明回溯父链（PERF-4 零克隆继承）。
    let source = CustomPropertySource::Chain {
        own: &own_props,
        parent: parent_source,
    };
    let (cs, own_font_size) = compute_element_style(
        &groups,
        parent_style,
        parent_font_size,
        root_font_size,
        options,
        &source,
    );

    // 根元素自身计算 font-size 后，其 px 成为子树 rem 基准。
    let child_root_fs = if root_font_size.is_none() {
        Some(own_font_size)
    } else {
        root_font_size
    };
    // CAS-1：不再整表克隆父 ComputedStyle——先持局部 `cs` 的引用递归子树，
    // 递归返回后再入表。styles 在子树遍历期间只写不读，借用无冲突。
    let children: Vec<Rc<RefCell<Node>>> = node.borrow().child_nodes().to_vec();
    for child in &children {
        walk(
            child,
            prepared,
            options,
            Some(&source),
            Some(&cs),
            own_font_size,
            child_root_fs,
            styles,
        );
    }
    styles.insert(addr, cs);
}

/// 从 cascade 分组派生本元素自己的 `--*` 表（不含继承）。
///
/// 与 [`crate::collect_custom_properties`] 等价，但复用调用方已完成的
/// `cascade_for_element` 结果，避免第二次级联（PERF-2）；且不克隆父级表
/// —— 继承由调用方构造 [`CustomPropertySource::Chain`] 链式回溯完成
/// （PERF-4 零克隆）。
fn derive_own_custom_props(
    groups: &HashMap<String, Vec<DeclaredValue>>,
) -> HashMap<String, Vec<ComponentValue>> {
    let mut props = HashMap::new();
    for (name, group) in groups {
        if name.starts_with("--") {
            if let Some(winner) = cascade_winner(group) {
                // P2-4：CSS-wide 关键字（initial/inherit/unset/revert）不写入，
                // 避免 var() 替换出字面量关键字。
                if !is_css_wide_keyword(&winner.value) {
                    // CAS-2：DeclaredValue.value 已 Arc 化；本表仍为 Vec
                    //（每元素一次浅转，自定义属性通常少量）。
                    props.insert(name.clone(), winner.value.to_vec());
                }
            }
        }
    }
    props
}

/// 计算单个元素的 ComputedStyle，返回 `(style, own_font_size_px)`。
///
/// 两步 font-size 算法：
/// 1. 用父 font-size 作 em/百分比基准算本元素 font-size → px；
/// 2. 用本元素 font-size 作 em 基准算其余属性（em 语义 = 自身 font-size）。
fn compute_element_style<'a>(
    groups: &HashMap<String, Vec<DeclaredValue>>,
    parent_style: Option<&ComputedStyle>,
    parent_font_size: f64,
    root_font_size: Option<f64>,
    options: &StyleTreeOptions,
    source: &'a CustomPropertySource<'a>,
) -> (ComputedStyle, f64) {
    let root_fs = root_font_size.unwrap_or(DEFAULT_FONT_SIZE);
    let mut cs = ComputedStyle::new();

    // 步骤 1：font-size（em/百分比基准 = 父 font-size）。
    let fs_ctx = ComputeContext::with_source(
        source,
        parent_font_size,
        root_fs,
        options.viewport_width,
        options.viewport_height,
    );
    let font_size = compute_one("font-size", groups, parent_style, &fs_ctx);
    let own_font_size = extract_font_size_px(&font_size).unwrap_or(parent_font_size);
    // CSS 语义下 font-size 的 computed value 是解析后的长度：关键字（如
    // "medium"=16px）归一化为 px Dimension，供 layout 等下游直接读取。
    cs.set("font-size", normalize_font_size(font_size, own_font_size));

    // 步骤 2：其余属性（em 基准 = 自身 font-size）。
    let ctx = ComputeContext::with_source(
        source,
        own_font_size,
        root_fs,
        options.viewport_width,
        options.viewport_height,
    );
    for property in groups.keys() {
        if property == "font-size" {
            continue;
        }
        let computed = compute_one(property, groups, parent_style, &ctx);
        cs.set(property.clone(), computed);
    }
    for prop_def in BUILTIN_PROPERTIES.iter() {
        if prop_def.name == "font-size" || cs.properties.contains_key(prop_def.name) {
            continue;
        }
        let computed = compute_one(prop_def.name, groups, parent_style, &ctx);
        cs.set(prop_def.name.to_string(), computed);
    }

    // 边框/轮廓宽度关键字归一化（见 [`normalize_line_width`]）：`medium`
    // 等关键字在此转成 px，layout 与 renderer 共用同一份映射。
    for name in LINE_WIDTH_PROPERTIES {
        normalize_line_width(&mut cs, name);
    }

    // line-height 百分比归一化（CSS Inline L3 §4.2）：百分比按**自身**
    // font-size 在计算值阶段解析为长度（Chrome `getComputedStyle` 亦返回 px）。
    // `<number>` 保持数字形态——它是倍数且随值继承，必须由各元素自身
    // font-size 折算（见 cascade `text_props::used_line_height_px`）。
    normalize_line_height_percentage(&mut cs, own_font_size);

    (cs, own_font_size)
}

/// 把 `line-height: <percentage>` 归一化为 px Dimension。
///
/// 计算值语义：百分比相对**自身** font-size（`own_font_size`）解析；数/长度/
/// `normal` 原样保留。负百分比与非法值不在此处理（使用值阶段回退 `normal`）。
fn normalize_line_height_percentage(cs: &mut ComputedStyle, own_font_size: f64) {
    let Some(cv) = cs.get("line-height") else {
        return;
    };
    let pct = cv.tokens().iter().find_map(|v| match v {
        ComponentValue::PreservedToken(Token::Percentage(numeric))
            if numeric.value.is_finite() && numeric.value >= 0.0 =>
        {
            Some(numeric.value)
        }
        _ => None,
    });
    let Some(pct) = pct else {
        return;
    };
    cs.set(
        "line-height",
        ComputedValue::from_tokens(vec![ComponentValue::PreservedToken(Token::Dimension(
            Numeric {
                value: pct / 100.0 * own_font_size,
                is_integer: false,
            },
            "px".to_string(),
        ))]),
    );
}

/// 需要关键字→px 归一化的宽度属性（四向 border-width + outline-width）。
const LINE_WIDTH_PROPERTIES: [&str; 5] = [
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
    "outline-width",
];

/// 对单个属性执行 defaulting + compute。
///
/// cascade 胜者 → `apply_defaulting`（CSS-wide 关键字/继承/初始值）→
/// `compute_value_with` 解析相对单位与 var()。单态化（P2-20）后值统一为
/// token 序列，defaulting 产物（关键字初始值、父计算值）再跑一次
/// compute_value_with 是幂等的（px Dimension 与 Ident 均原样返回），
/// 故不再区分 Raw/Keyword。
///
/// 值含无效 var()（`Err`）时按 unset 处理（css-variables-1 §3.1：
/// invalid at computed-value time）：继承属性取父值、非继承属性取初始值
/// —— 与未声明的 `apply_defaulting(property, None, parent)` 等价（P2-5）。
///
/// # CAS-2/3 快速路径
///
/// 完整路径对每属性做两次逐 token 物化（defaulting 的 `to_vec` +
/// `resolve_tokens` 的 `out`）。两条快速路径依上述幂等性跳过：
///
/// 1. **无胜者（未声明）**——继承属性取父 computed 值的 Arc 克隆
///    （幂等重算结果相同）；无父值（根元素）与非继承属性取
///    [`INITIAL_COMPUTED`] 预生成常量（compute 对关键字/数字 token
///    恒等）。整树路径未声明属性不再逐 token 物化（原"全属性盲算"）。
/// 2. **胜出声明免解析直享**——值非 CSS-wide 关键字
///    （[`is_defaulting_keyword`]，defaulting 需改写）且无需解析
///    （[`needs_no_resolution`]，无 var()/可换算单位/函数）时
///    compute 恒等，直接共享声明值 Arc。
fn compute_one(
    property: &str,
    groups: &HashMap<String, Vec<DeclaredValue>>,
    parent_style: Option<&ComputedStyle>,
    ctx: &ComputeContext,
) -> ComputedValue {
    let winner = groups.get(property).and_then(|g| cascade_winner(g));
    let parent_value = parent_style.and_then(|ps| ps.get(property));

    // 快速路径 1：未声明（无胜者）——defaulting 产物免 compute。
    let Some(winner) = winner else {
        return match (lookup_property(property), parent_value) {
            (Some(def), Some(pv)) if def.inherited => pv.clone(),
            (Some(def), _) => initial_computed(def),
            // 未注册属性保持原语义（initial 兜底关键字）。
            (None, _) => apply_defaulting(property, None, parent_value),
        };
    };

    // 快速路径 2：胜出声明免解析直享（Arc 共享，零深拷贝）。
    if !is_defaulting_keyword(&winner.value) && needs_no_resolution(&winner.value, property) {
        return ComputedValue::from_arc(Arc::clone(&winner.value));
    }

    // 完整路径：defaulting（CSS-wide 关键字/继承/初始值）→ compute。
    let specified = apply_defaulting(property, Some(&winner.value), parent_value);
    match compute_value_with(property, specified.tokens(), ctx) {
        Ok(computed) => computed,
        Err(()) => apply_defaulting(property, None, parent_value),
    }
}

/// CAS-3：内建属性 initial 值的预生成 [`ComputedValue`] 常量表。
///
/// 内建 initial 值均为关键字/数字 token（registry 全表无单位/百分比/
/// 函数），compute 对其恒等（[`needs_no_resolution`]），故未声明的非
/// 继承属性（及无父值的继承属性）直接共享同一 Arc 实例——零计算、
/// 零深拷贝直填，替代逐元素逐属性的关键字物化。
static INITIAL_COMPUTED: OnceLock<HashMap<&'static str, ComputedValue>> = OnceLock::new();

/// 取属性的预生成 initial [`ComputedValue`]（Arc 克隆，引用计数 +1）。
fn initial_computed(def: &PropertyDefinition) -> ComputedValue {
    INITIAL_COMPUTED
        .get_or_init(|| {
            BUILTIN_PROPERTIES
                .iter()
                .map(|p| (p.name, ComputedValue::from_keyword(p.initial_value)))
                .collect::<HashMap<_, _>>()
        })
        .get(def.name)
        .cloned()
        .unwrap_or_else(|| ComputedValue::from_keyword(def.initial_value))
}

/// 将 font-size 的关键字形态值归一化为 px Dimension。
///
/// 单态化（P2-20）后关键字即 `[Ident(s)]`；关键字形态（`medium`、初始值）
/// 说明该值来自 defaulting 而非显式长度声明，CSS 语义下 font-size 的计算值
/// 是长度，故转成 px Dimension。已是 Dimension（显式 px/em 等解析结果）
/// 的原样保留。
fn normalize_font_size(cv: ComputedValue, px: f64) -> ComputedValue {
    if cv.keyword().is_some() {
        ComputedValue::from_tokens(vec![ComponentValue::PreservedToken(Token::Dimension(
            Numeric {
                value: px,
                is_integer: false,
            },
            "px".to_string(),
        ))])
    } else {
        cv
    }
}

/// 将 `border-<side>-width` / `outline-width` 的 `<line-width>` 关键字
/// 归一化为 px Dimension（`thin`/`medium`/`thick` → 1/3/5）。
///
/// CSS Backgrounds & Borders L3 §4.3：三个关键字宽度 UA 相关，取值与
/// Chrome/Firefox 对齐（1/3/5px）。computed value 的语义是长度，故与
/// font-size 同样在此归一化——layout（taffy border）与 renderer（绘制）
/// 共用单一映射，避免两处各写一份。显式长度（px Dimension / `0` Number）
/// 与无法识别的值原样保留（下游按 0 处理）。
///
/// 注意：style 为 `none`/`hidden` 时 **used** width = 0（§4.1），该判定
/// 属布局/绘制阶段（`style` 与 `width` 是两个属性），不在此处做。
fn normalize_line_width(cs: &mut ComputedStyle, name: &str) {
    let px = match cs.get(name).and_then(|cv| cv.keyword()) {
        Some(kw) if kw.eq_ignore_ascii_case("thin") => 1.0,
        Some(kw) if kw.eq_ignore_ascii_case("medium") => 3.0,
        Some(kw) if kw.eq_ignore_ascii_case("thick") => 5.0,
        _ => return,
    };
    cs.set(
        name,
        ComputedValue::from_tokens(vec![ComponentValue::PreservedToken(Token::Dimension(
            Numeric {
                value: px,
                is_integer: false,
            },
            "px".to_string(),
        ))]),
    );
}

/// 从 ComputedStyle 的 font-size 值提取 px 数值。
///
/// 统一按 token 序列处理：优先取第一个 px Dimension；退化为单个 Ident
/// 关键字时按 `medium`（=16px）或数字字符串解析。其余返回 `None`
/// （调用方回退到父 font-size）。
fn extract_font_size_px(cv: &ComputedValue) -> Option<f64> {
    let cvs = cv.tokens();
    if let Some(n) = cvs.iter().find_map(|v| match v {
        ComponentValue::PreservedToken(Token::Dimension(n, u)) if u.eq_ignore_ascii_case("px") => {
            Some(n.value)
        }
        _ => None,
    }) {
        return Some(n);
    }
    match cv.keyword() {
        Some(s) if s.eq_ignore_ascii_case("medium") => Some(DEFAULT_FONT_SIZE),
        Some(s) => s.trim().parse::<f64>().ok(),
        None => None,
    }
}
