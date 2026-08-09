//! §4.4 Computed Value — 相对单位解析、var() 求值、百分比解析。
//!
//! 规范源: `d:\csswg\css-cascade-5\Overview.md` §4.4 L500-555
//!
//! 将 specified value 转换为 computed value：
//! - 相对长度单位（em/rem/vh/vw/vmin/vmax）→ px
//! - `var()` 替换（递归求值 fallback）
//! - font-size 百分比 → px（其他属性的百分比推迟到 layout 阶段）

use crate::registry::{lookup_property, PercentageBasis};
use crate::style::ComputedValue;
use muskitty_css::parser::{BlockKind, ComponentValue, Function};
use muskitty_css::tokenizer::{Numeric, Token};
use std::collections::{HashMap, HashSet};

/// var() 查找自定义属性值的来源。
///
/// - [`Flat`]: 平面表 —— 单元素原语路径（`compute_value` 直接调用方），
///   继承表由调用方克隆合并。
/// - [`Chain`]: 链式继承 —— 整树路径（[`crate::style_tree::compute_styles`]），
///   `own` 只含本元素 cascade 胜出的 `--*` 声明，查表先 own 再回溯 `parent`
///   链，每层零克隆（PERF-4）。
///
/// [`Flat`]: CustomPropertySource::Flat
/// [`Chain`]: CustomPropertySource::Chain
#[derive(Debug, Clone)]
pub enum CustomPropertySource<'a> {
    /// 平面表。
    Flat(&'a HashMap<String, Vec<ComponentValue>>),
    /// 链式继承：本元素声明表 + 父链。
    Chain {
        /// 本元素 cascade 胜出的 `--*` 声明。
        own: &'a HashMap<String, Vec<ComponentValue>>,
        /// 父元素来源（继承回溯）。
        parent: Option<&'a CustomPropertySource<'a>>,
    },
}

impl<'a> CustomPropertySource<'a> {
    /// 按名称查找自定义属性值；链式来源先查 own，再回溯父链。
    pub fn get(&self, name: &str) -> Option<&'a Vec<ComponentValue>> {
        match self {
            CustomPropertySource::Flat(map) => map.get(name),
            CustomPropertySource::Chain { own, parent } => {
                own.get(name).or_else(|| parent.and_then(|p| p.get(name)))
            }
        }
    }
}

/// §4.4: Computed value 计算上下文。
///
/// 提供相对单位解析、var() 替换所需的上下文数据。
pub struct ComputeContext<'a> {
    /// 父元素 font-size（px），用于 em 解析。
    pub parent_font_size: f64,
    /// 根元素 font-size（px），用于 rem 解析。
    pub root_font_size: f64,
    /// 视口宽度（px），用于 vw/vmin/vmax 解析。
    pub viewport_width: f64,
    /// 视口高度（px），用于 vh/vmin/vmax 解析。
    pub viewport_height: f64,
    /// 自定义属性来源（name → value），用于 var() 替换。
    pub custom_properties: CustomPropertySource<'a>,
}

impl<'a> ComputeContext<'a> {
    /// 创建默认上下文（font-size 16px, viewport 1920x1080, 平面自定义属性）。
    pub fn new(custom_properties: &'a HashMap<String, Vec<ComponentValue>>) -> Self {
        Self {
            parent_font_size: 16.0,
            root_font_size: 16.0,
            viewport_width: 1920.0,
            viewport_height: 1080.0,
            custom_properties: CustomPropertySource::Flat(custom_properties),
        }
    }

    /// 用显式 font-size 与视口尺寸构造上下文（平面自定义属性来源）。
    ///
    /// `parent_font_size` 是 em/百分比基准：计算 font-size 属性时为父元素
    /// font-size；计算其余属性时为元素自身 font-size（em 语义）。`root_font_size`
    /// 是 rem 基准（根元素 font-size，px）。
    pub fn with_font_sizes(
        custom_properties: &'a HashMap<String, Vec<ComponentValue>>,
        parent_font_size: f64,
        root_font_size: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Self {
        Self {
            parent_font_size,
            root_font_size,
            viewport_width,
            viewport_height,
            custom_properties: CustomPropertySource::Flat(custom_properties),
        }
    }

    /// 用链式自定义属性来源构造上下文（整树路径，PERF-4 零克隆继承）。
    pub fn with_source(
        source: &'a CustomPropertySource<'a>,
        parent_font_size: f64,
        root_font_size: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Self {
        Self {
            parent_font_size,
            root_font_size,
            viewport_width,
            viewport_height,
            custom_properties: source.clone(),
        }
    }
}

/// §4.4: 将 specified value 转换为 computed value。
///
/// 处理：
/// - 相对长度单位（em/rem/vh/vw/vmin/vmax）→ px
/// - `var()` 替换（记忆化递归，含 fallback）
/// - font-size 百分比 → px
///
/// 其他属性的百分比保持原样（推迟到 layout 阶段解析）。
///
/// 兼容包装：值含无效 var()（首参非 `--*` 或引用 guaranteed-invalid 且无
/// fallback）时返回空 `Resolved`（旧行为）；整树路径应使用
/// [`compute_value_with`] 感知 invalid-at-computed-value（P2-5）。
pub fn compute_value(
    property: &str,
    specified: &[ComponentValue],
    ctx: &ComputeContext,
) -> ComputedValue {
    compute_value_with(property, specified, ctx)
        .unwrap_or_else(|_| ComputedValue::from_tokens(Vec::new()))
}

/// §4.4: 同 [`compute_value`]，但报告 invalid-at-computed-value。
///
/// `Err(())` = 值含无效 var()（css-variables-1 §3.1）：首参非自定义属性名，
/// 或引用了 guaranteed-invalid 的自定义属性且无 fallback。调用方（如
/// [`crate::style_tree`]）此时应将该属性按 unset 处理（继承属性取父值、
/// 非继承属性取初始值）。
///
/// `Err` 携带 `()`：无效即 sentinel，无需错误数据；调用方只需区分
/// 有效/无效（B4 计划约定的签名）。
#[allow(clippy::result_unit_err)]
pub fn compute_value_with(
    property: &str,
    specified: &[ComponentValue],
    ctx: &ComputeContext,
) -> Result<ComputedValue, ()> {
    let mut resolver = VarResolver::new(ctx);
    let resolved = resolver.resolve_tokens(specified, property)?;
    Ok(ComputedValue::from_tokens(resolved))
}

/// 单个自定义属性的解析结果。
#[derive(Debug, Clone)]
enum ResolvedVar {
    /// 完整展开后的 token 序列（相对单位已按当前 ctx 解析）。
    Tokens(Vec<ComponentValue>),
    /// guaranteed-invalid（css-variables-1 §3.1）：环、未定义、或值含
    /// 无效 var()。
    GuaranteedInvalid,
}

/// §3 var() 解析器：3 色 DFS 记忆化（P1-1 / PERF-3）。
///
/// - `resolved`（black）：已解析完成的变量，结果直接复用；
/// - `in_progress`（gray）：解析中的变量（DFS 栈），命中即环。
///
/// 每个 var 名在同一解析器内至多求值一次。对 `--v{i}: var(--v{i-1})
/// var(--v{i-1})` 这类重复引用链，输出规模 2^N 是语义使然，但每级只展开
/// 一次 —— 时间线性于输出规模，不再按 N×2^N 指数重复递归（修复前实测
/// N=22 需 30.9s）。
struct VarResolver<'a> {
    ctx: &'a ComputeContext<'a>,
    resolved: HashMap<String, ResolvedVar>,
    in_progress: HashSet<String>,
}

impl<'a> VarResolver<'a> {
    fn new(ctx: &'a ComputeContext<'a>) -> Self {
        Self {
            ctx,
            resolved: HashMap::new(),
            in_progress: HashSet::new(),
        }
    }

    /// 解析一组 token。任一 token 无效 → `Err`（整条属性 invalid at
    /// computed-value time）。
    ///
    /// 输入 token 的生命周期独立于 `'a`：调用方可能是属性 specified 值
    /// （短借用），也可能是 `ctx` 内自定义属性值（长借用）。
    fn resolve_tokens(
        &mut self,
        tokens: &[ComponentValue],
        property: &str,
    ) -> Result<Vec<ComponentValue>, ()> {
        let mut out = Vec::with_capacity(tokens.len());
        for cv in tokens {
            out.extend(self.resolve_component(cv, property)?);
        }
        Ok(out)
    }

    /// 递归解析单个 component value。
    ///
    /// 返回 `Vec` 是因为 `var()` 替换可能展开为多个值；`Err` 表示无效
    /// var()（见 [`compute_value_with`]）。
    fn resolve_component(
        &mut self,
        cv: &ComponentValue,
        property: &str,
    ) -> Result<Vec<ComponentValue>, ()> {
        match cv {
            // 相对长度单位解析
            ComponentValue::PreservedToken(Token::Dimension(numeric, unit)) => {
                Ok(resolve_dimension(numeric, unit, property, self.ctx))
            }
            // 百分比解析（仅 font-size 等需要在此阶段解析）
            ComponentValue::PreservedToken(Token::Percentage(numeric)) => {
                Ok(resolve_percentage(numeric, property, self.ctx))
            }
            // var() 替换：首参非 `--*` 或语法无效 → invalid at computed-value time（P2-5）
            ComponentValue::Function(func) if func.name.eq_ignore_ascii_case("var") => {
                let (name, fallback) = parse_var_args(&func.value).ok_or(())?;
                self.resolve_var_ref(&name, fallback, property)
            }
            // 其他函数（如 calc()）— 递归解析参数
            ComponentValue::Function(func) => {
                let resolved_args = self.resolve_tokens(&func.value, property)?;
                // calc() 数值求值（P3-2）：可求值 → 折叠为单值；否则保留函数
                // （layout 侧对不可求值 calc 仍走 extract_px/extract_percent 兜底）。
                if func.name.eq_ignore_ascii_case("calc") {
                    if let Some(folded) = evaluate_calc(&resolved_args) {
                        return Ok(vec![folded]);
                    }
                }
                Ok(vec![ComponentValue::Function(Function {
                    name: func.name.clone(),
                    value: resolved_args,
                })])
            }
            // 其他 token 原样保留
            other => Ok(vec![other.clone()]),
        }
    }

    /// 解析单个 var() 引用。
    ///
    /// 被引用的自定义属性 guaranteed-invalid（环、未定义、值含无效 var()）时：
    /// 有 fallback 则递归解析 fallback（P1-2），无 fallback 则 `Err`。
    fn resolve_var_ref(
        &mut self,
        name: &str,
        fallback: &[ComponentValue],
        property: &str,
    ) -> Result<Vec<ComponentValue>, ()> {
        let ctx = self.ctx; // Copy，避免字段访问与 &mut self 冲突

        // black：已解析 → 直接复用展开结果。
        let cached = self.resolved.get(name).cloned();
        if let Some(rv) = cached {
            return self.materialize(rv, fallback, property);
        }
        // gray：DFS 栈命中 → 环。该 var() 视为 guaranteed-invalid（P1-2）。
        if !self.in_progress.insert(name.to_string()) {
            return self.resolve_fallback(fallback, property);
        }

        // 首次解析：求值该变量的值（可能含嵌套 var()）。
        let result = match ctx.custom_properties.get(name) {
            Some(value) => self.resolve_tokens(value, property),
            None => Err(()), // 未定义 → guaranteed-invalid
        };
        self.in_progress.remove(name);

        let rv = match result {
            Ok(tokens) => ResolvedVar::Tokens(tokens),
            Err(()) => ResolvedVar::GuaranteedInvalid,
        };
        self.resolved.insert(name.to_string(), rv.clone());
        self.materialize(rv, fallback, property)
    }

    /// 将缓存结果物化为输出：Tokens 直接返回；GuaranteedInvalid 走 fallback。
    fn materialize(
        &mut self,
        rv: ResolvedVar,
        fallback: &[ComponentValue],
        property: &str,
    ) -> Result<Vec<ComponentValue>, ()> {
        match rv {
            ResolvedVar::Tokens(tokens) => Ok(tokens),
            ResolvedVar::GuaranteedInvalid => self.resolve_fallback(fallback, property),
        }
    }

    /// 解析 fallback（被引用变量 guaranteed-invalid 时）。无 fallback → Err。
    fn resolve_fallback(
        &mut self,
        fallback: &[ComponentValue],
        property: &str,
    ) -> Result<Vec<ComponentValue>, ()> {
        if fallback.is_empty() {
            Err(())
        } else {
            self.resolve_tokens(fallback, property)
        }
    }
}

/// 解析 `var()` 参数：`var(--name, fallback...)`。
///
/// 返回 `(name, fallback_tokens)`。css-variables-1 §3.1 要求首参是自定义
/// 属性名（`--*`）；否则该 var() 在 computed-value time 无效（P2-5）。
fn parse_var_args(value: &[ComponentValue]) -> Option<(String, &[ComponentValue])> {
    // 跳过前导空白，取第一个非空白 token 作为首参。
    let mut name = None;
    let mut i = 0;
    while i < value.len() {
        match &value[i] {
            ComponentValue::PreservedToken(Token::Whitespace) => i += 1,
            ComponentValue::PreservedToken(Token::Ident(s)) => {
                name = Some(s.clone());
                i += 1;
                break;
            }
            _ => return None, // 首参不是 ident → 无效
        }
    }
    let name = name?;
    if !name.starts_with("--") {
        return None; // 首参非自定义属性名 → invalid at computed-value time
    }
    // 找逗号；逗号前不得再有 token。
    let mut j = i;
    while j < value.len() {
        match &value[j] {
            ComponentValue::PreservedToken(Token::Whitespace) => j += 1,
            ComponentValue::PreservedToken(Token::Comma) => break,
            _ => return None,
        }
    }
    // 逗号后的全部是 fallback。跳过后导前导空白：前导空白替换后无语义，
    // 且旧实现（resolve_var 的 Whitespace => continue）本就丢弃，保持兼容
    // （避免 cvs[0] 为 Whitespace 破坏下游按首 token 的断言）。
    let mut fallback_start = j + 1;
    while fallback_start < value.len() {
        if let ComponentValue::PreservedToken(Token::Whitespace) = &value[fallback_start] {
            fallback_start += 1;
        } else {
            break;
        }
    }
    let fallback = if j < value.len() {
        &value[fallback_start..]
    } else {
        &[]
    };
    Some((name, fallback))
}

/// calc() 求值的中间值（CSS Values L4 §9）。
///
/// 一个可相加/相乘的数量：纯数字、带单位长度（px 等）、或百分比。
/// 类型不兼容的运算（length + number、px × px 等）返回 `None`（不可求值，
/// 保留 calc 函数由下游兜底）。
#[derive(Debug, Clone)]
enum CalcValue {
    Number(f64),
    Dimension(f64, String),
    Percentage(f64),
}

impl CalcValue {
    /// 折叠为单个 component value。
    fn into_component_value(self) -> ComponentValue {
        match self {
            CalcValue::Number(v) => ComponentValue::PreservedToken(Token::Number(Numeric {
                value: v,
                is_integer: false,
            })),
            CalcValue::Dimension(v, unit) => ComponentValue::PreservedToken(Token::Dimension(
                Numeric {
                    value: v,
                    is_integer: false,
                },
                unit,
            )),
            CalcValue::Percentage(v) => {
                ComponentValue::PreservedToken(Token::Percentage(Numeric {
                    value: v,
                    is_integer: false,
                }))
            }
        }
    }
}

/// §9 解析并求值 `calc(...)` 表达式（calc-sum → calc-product → calc-value）。
///
/// 求值入口：`resolve_component` 遇到 `calc()` 时调用。调用方保证参数已完成
/// var() 替换与相对单位解析（因此 em/pt 等均已归一到 px）。可求值（全部类型
/// 兼容、无除零）时返回折叠后的单值；否则返回 `None`，调用方保留 calc 函数，
/// 由 layout 侧 extract_px/extract_percent 兜底。
fn evaluate_calc(args: &[ComponentValue]) -> Option<ComponentValue> {
    let mut pos = 0;
    let sum = parse_calc_sum(args, &mut pos)?;
    skip_ws(args, &mut pos);
    if pos != args.len() {
        return None; // 尾随内容（如 `10px 5px`）→ 语法无效
    }
    Some(sum.into_component_value())
}

/// calc-sum: `<calc-product> [ [ '+' | '-' ] <calc-product> ]*`（左结合）。
fn parse_calc_sum(cvs: &[ComponentValue], pos: &mut usize) -> Option<CalcValue> {
    let mut left = parse_calc_product(cvs, pos)?;
    loop {
        skip_ws(cvs, pos);
        let Some(op) = peek_delim(cvs, *pos) else {
            break; // 输入结束或下一项不是操作符 → sum 解析完成
        };
        if op != '+' && op != '-' {
            // 非加减操作符（`*`/`/`）属于 product 层级，回退。
            break;
        }
        *pos += 1; // 消费操作符
        let right = parse_calc_product(cvs, pos)?;
        left = apply_add_sub(op, left, right)?;
    }
    Some(left)
}

/// calc-product: `<calc-value> [ [ '*' | '/' ] <calc-value> ]*`（左结合）。
fn parse_calc_product(cvs: &[ComponentValue], pos: &mut usize) -> Option<CalcValue> {
    let mut left = parse_calc_value(cvs, pos)?;
    loop {
        skip_ws(cvs, pos);
        let Some(op) = peek_delim(cvs, *pos) else {
            break; // 输入结束或下一项不是操作符 → product 解析完成
        };
        if op != '*' && op != '/' {
            // 加减操作符属于 sum 层级，回退。
            break;
        }
        *pos += 1; // 消费操作符
        let right = parse_calc_value(cvs, pos)?;
        left = apply_mul_div(op, left, right)?;
    }
    Some(left)
}

/// calc-value: `<number> | <dimension> | <percentage> | ( <calc-sum> )`。
/// 也接受嵌套 `calc(...)`（内层同样求值）。
fn parse_calc_value(cvs: &[ComponentValue], pos: &mut usize) -> Option<CalcValue> {
    skip_ws(cvs, pos);
    match cvs.get(*pos)? {
        ComponentValue::PreservedToken(Token::Number(n)) => {
            *pos += 1;
            Some(CalcValue::Number(n.value))
        }
        ComponentValue::PreservedToken(Token::Dimension(n, unit)) => {
            *pos += 1;
            Some(CalcValue::Dimension(n.value, unit.clone()))
        }
        ComponentValue::PreservedToken(Token::Percentage(n)) => {
            *pos += 1;
            Some(CalcValue::Percentage(n.value))
        }
        ComponentValue::SimpleBlock(sb) if sb.kind == BlockKind::Paren => {
            *pos += 1;
            let mut inner = 0;
            let v = parse_calc_sum(&sb.value, &mut inner)?;
            skip_ws(&sb.value, &mut inner);
            (inner == sb.value.len()).then_some(v)
        }
        ComponentValue::Function(f) if f.name.eq_ignore_ascii_case("calc") => {
            *pos += 1;
            let mut inner = 0;
            let v = parse_calc_sum(&f.value, &mut inner)?;
            skip_ws(&f.value, &mut inner);
            (inner == f.value.len()).then_some(v)
        }
        _ => None,
    }
}

/// 加减：两侧类型必须一致（number/number、同 unit dimension、percentage）。
fn apply_add_sub(op: char, left: CalcValue, right: CalcValue) -> Option<CalcValue> {
    let combine = |a: f64, b: f64| if op == '+' { a + b } else { a - b };
    match (left, right) {
        (CalcValue::Number(a), CalcValue::Number(b)) => Some(CalcValue::Number(combine(a, b))),
        (CalcValue::Dimension(a, ua), CalcValue::Dimension(b, ub))
            if ua.eq_ignore_ascii_case(&ub) =>
        {
            Some(CalcValue::Dimension(combine(a, b), ua))
        }
        (CalcValue::Percentage(a), CalcValue::Percentage(b)) => {
            Some(CalcValue::Percentage(combine(a, b)))
        }
        // 类型不兼容（length + number、px + % 等）→ 不可求值
        _ => None,
    }
}

/// 乘除：一侧必须为 number（`2 * 3px` / `3px * 2` / `10px / 2`）。除零 → 不可求值。
fn apply_mul_div(op: char, left: CalcValue, right: CalcValue) -> Option<CalcValue> {
    let mul = |a: f64, b: f64| a * b;
    let div = |a: f64, b: f64| a / b;
    let bin = if op == '*' { mul } else { div };
    match (left, right) {
        (CalcValue::Number(a), CalcValue::Number(b)) => {
            (b != 0.0 || op == '*').then(|| CalcValue::Number(bin(a, b)))
        }
        (CalcValue::Dimension(a, u), CalcValue::Number(b)) => {
            (b != 0.0 || op == '*').then(|| CalcValue::Dimension(bin(a, b), u))
        }
        (CalcValue::Percentage(a), CalcValue::Number(b)) => {
            (b != 0.0 || op == '*').then(|| CalcValue::Percentage(bin(a, b)))
        }
        // 乘法交换：number × dimension / percentage
        (CalcValue::Number(a), CalcValue::Dimension(b, u)) if op == '*' => {
            Some(CalcValue::Dimension(a * b, u))
        }
        (CalcValue::Number(a), CalcValue::Percentage(b)) if op == '*' => {
            Some(CalcValue::Percentage(a * b))
        }
        // 其余组合（dimension × dimension、number / dimension、dimension × % 等）→ 不可求值
        _ => None,
    }
}

/// 跳过连续 whitespace token。
fn skip_ws(cvs: &[ComponentValue], pos: &mut usize) {
    while *pos < cvs.len() {
        if let ComponentValue::PreservedToken(Token::Whitespace) = &cvs[*pos] {
            *pos += 1;
        } else {
            break;
        }
    }
}

/// 窥视当前位置的 delim token（调用前须已 skip_ws），不消费。
/// 由调用方在确认操作符属于当前层级后自行 `*pos += 1`。
fn peek_delim(cvs: &[ComponentValue], pos: usize) -> Option<char> {
    match cvs.get(pos) {
        Some(ComponentValue::PreservedToken(Token::Delim(c))) => Some(*c),
        _ => None,
    }
}

/// 解析长度维度（相对单位 + 绝对单位 → px）。
///
/// - 相对单位：em/rem/vh/vw/vmin/vmax → px（依赖 ctx 基准）。
/// - 绝对单位：px 原样保留；pt/pc/in/cm/mm/q 按 CSS Values L4 §5.1 锚点
///   换算为 px（1in = 96px = 2.54cm = 25.4mm = 101.6q = 72pt = 6pc）。
/// - 未知单位：原样保留（留给下游处理）。
fn resolve_dimension(
    numeric: &Numeric,
    unit: &str,
    _property: &str,
    ctx: &ComputeContext,
) -> Vec<ComponentValue> {
    let value = numeric.value;
    // PERF-5：逐 eq_ignore_ascii_case 分支匹配，避免 to_ascii_lowercase()
    // 堆分配（px 等常用单位也经过该分支）。
    let resolved = if unit.eq_ignore_ascii_case("em") {
        Some(value * ctx.parent_font_size)
    } else if unit.eq_ignore_ascii_case("rem") {
        Some(value * ctx.root_font_size)
    } else if unit.eq_ignore_ascii_case("vh") {
        Some(value * ctx.viewport_height / 100.0)
    } else if unit.eq_ignore_ascii_case("vw") {
        Some(value * ctx.viewport_width / 100.0)
    } else if unit.eq_ignore_ascii_case("vmin") {
        Some(value * ctx.viewport_width.min(ctx.viewport_height) / 100.0)
    } else if unit.eq_ignore_ascii_case("vmax") {
        Some(value * ctx.viewport_width.max(ctx.viewport_height) / 100.0)
    } else if unit.eq_ignore_ascii_case("pt") {
        // 1pt = 1/72 in = 96/72 px
        Some(value * (96.0 / 72.0))
    } else if unit.eq_ignore_ascii_case("pc") {
        // 1pc = 1/6 in = 16px
        Some(value * (96.0 / 6.0))
    } else if unit.eq_ignore_ascii_case("in") {
        Some(value * 96.0)
    } else if unit.eq_ignore_ascii_case("cm") {
        Some(value * (96.0 / 2.54))
    } else if unit.eq_ignore_ascii_case("mm") {
        Some(value * (96.0 / 25.4))
    } else if unit.eq_ignore_ascii_case("q") {
        Some(value * (96.0 / 101.6))
    } else {
        // 未知单位 — 不转换
        None
    };

    match resolved {
        Some(px) => vec![ComponentValue::PreservedToken(Token::Dimension(
            Numeric {
                value: px,
                is_integer: false,
            },
            "px".to_string(),
        ))],
        None => vec![ComponentValue::PreservedToken(Token::Dimension(
            numeric.clone(),
            unit.to_string(),
        ))],
    }
}

/// 解析百分比。
///
/// 仅 font-size（PercentageBasis::ParentFontSize）和
/// ParentSameProperty（如果父值是绝对长度）在此阶段解析。
/// 其他百分比保持原样（推迟到 layout）。
fn resolve_percentage(
    numeric: &Numeric,
    property: &str,
    ctx: &ComputeContext,
) -> Vec<ComponentValue> {
    let basis = lookup_property(property).map(|d| d.percentages);

    match basis {
        Some(PercentageBasis::ParentFontSize) => {
            // font-size: 120% → 1.2 * parent_font_size px
            let px = numeric.value / 100.0 * ctx.parent_font_size;
            vec![ComponentValue::PreservedToken(Token::Dimension(
                Numeric {
                    value: px,
                    is_integer: false,
                },
                "px".to_string(),
            ))]
        }
        Some(PercentageBasis::RootFontSize) => {
            let px = numeric.value / 100.0 * ctx.root_font_size;
            vec![ComponentValue::PreservedToken(Token::Dimension(
                Numeric {
                    value: px,
                    is_integer: false,
                },
                "px".to_string(),
            ))]
        }
        // 其他百分比基准（ParentWidth/ParentHeight/ParentSameProperty/None）
        // 推迟到 layout 阶段解析 — 原样保留
        _ => vec![ComponentValue::PreservedToken(Token::Percentage(
            numeric.clone(),
        ))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muskitty_css::parser::SimpleBlock;

    fn dim(value: f64, unit: &str) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Dimension(
            Numeric {
                value,
                is_integer: false,
            },
            unit.to_string(),
        ))
    }

    fn pct(value: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Percentage(Numeric {
            value,
            is_integer: false,
        }))
    }

    fn empty_ctx() -> ComputeContext<'static> {
        static EMPTY: std::sync::OnceLock<HashMap<String, Vec<ComponentValue>>> =
            std::sync::OnceLock::new();
        let props = EMPTY.get_or_init(HashMap::new);
        ComputeContext::new(props)
    }

    fn ctx_with_custom(props: &HashMap<String, Vec<ComponentValue>>) -> ComputeContext<'_> {
        ComputeContext::new(props)
    }

    // —— 相对单位解析 ——

    #[test]
    fn em_resolves_to_px_using_parent_font_size() {
        let ctx = ComputeContext {
            parent_font_size: 20.0,
            ..empty_ctx()
        };
        let result = compute_value("margin-top", &[dim(2.0, "em")], &ctx);
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1);
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
                assert_eq!(n.value, 40.0);
                assert_eq!(u, "px");
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    #[test]
    fn rem_resolves_to_px_using_root_font_size() {
        let ctx = ComputeContext {
            root_font_size: 18.0,
            ..empty_ctx()
        };
        let result = compute_value("margin-top", &[dim(3.0, "rem")], &ctx);
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
                assert_eq!(n.value, 54.0);
                assert_eq!(u, "px");
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    #[test]
    fn vh_resolves_to_px() {
        let ctx = ComputeContext {
            viewport_height: 1000.0,
            ..empty_ctx()
        };
        let result = compute_value("height", &[dim(50.0, "vh")], &ctx);
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, _)) => {
                assert_eq!(n.value, 500.0);
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    #[test]
    fn vw_resolves_to_px() {
        let ctx = ComputeContext {
            viewport_width: 800.0,
            ..empty_ctx()
        };
        let result = compute_value("width", &[dim(25.0, "vw")], &ctx);
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, _)) => {
                assert_eq!(n.value, 200.0);
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    #[test]
    fn vmin_and_vmax_resolve() {
        let ctx = ComputeContext {
            viewport_width: 800.0,
            viewport_height: 600.0,
            ..empty_ctx()
        };
        // vmin = min(800, 600) = 600, 10vmin = 60px
        let result = compute_value("width", &[dim(10.0, "vmin")], &ctx);
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, _)) => {
                assert_eq!(n.value, 60.0);
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
        // vmax = max(800, 600) = 800, 10vmax = 80px
        let result = compute_value("width", &[dim(10.0, "vmax")], &ctx);
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, _)) => {
                assert_eq!(n.value, 80.0);
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    #[test]
    fn px_unit_preserved() {
        let result = compute_value("width", &[dim(100.0, "px")], &empty_ctx());
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
                assert_eq!(n.value, 100.0);
                assert_eq!(u, "px");
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    // —— 百分比解析 ——

    #[test]
    fn font_size_percentage_resolves_to_px() {
        let ctx = ComputeContext {
            parent_font_size: 20.0,
            ..empty_ctx()
        };
        // font-size: 150% → 30px
        let result = compute_value("font-size", &[pct(150.0)], &ctx);
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
                assert_eq!(n.value, 30.0);
                assert_eq!(u, "px");
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    #[test]
    fn width_percentage_preserved() {
        // width 的百分比推迟到 layout — 原样保留
        let result = compute_value("width", &[pct(50.0)], &empty_ctx());
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Percentage(n)) => {
                assert_eq!(n.value, 50.0);
            }
            other => panic!("expected Percentage, got {:?}", other),
        }
    }

    // —— var() 替换 ——

    #[test]
    fn var_substitutes_custom_property() {
        let mut props = HashMap::new();
        props.insert(
            "--main-color".to_string(),
            vec![ComponentValue::PreservedToken(Token::Ident(
                "red".to_string(),
            ))],
        );
        let ctx = ctx_with_custom(&props);

        let var_fn = ComponentValue::Function(Function {
            name: "var".to_string(),
            value: vec![ComponentValue::PreservedToken(Token::Ident(
                "--main-color".to_string(),
            ))],
        });

        let result = compute_value("color", &[var_fn], &ctx);
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1);
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Ident(s)) => {
                assert_eq!(s, "red");
            }
            other => panic!("expected Ident, got {:?}", other),
        }
    }

    #[test]
    fn var_uses_fallback_when_undefined() {
        let props = HashMap::new();
        let ctx = ctx_with_custom(&props);

        let var_fn = ComponentValue::Function(Function {
            name: "var".to_string(),
            value: vec![
                ComponentValue::PreservedToken(Token::Ident("--undefined".to_string())),
                ComponentValue::PreservedToken(Token::Comma),
                ComponentValue::PreservedToken(Token::Whitespace),
                ComponentValue::PreservedToken(Token::Ident("blue".to_string())),
            ],
        });

        let result = compute_value("color", &[var_fn], &ctx);
        let cvs = result.tokens();
        // Whitespace is preserved in fallback
        let idents: Vec<_> = cvs
            .iter()
            .filter_map(|cv| match cv {
                ComponentValue::PreservedToken(Token::Ident(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(idents.contains(&"blue".to_string()));
    }

    #[test]
    fn var_resolves_relative_units_in_substitution() {
        let mut props = HashMap::new();
        props.insert("--gap".to_string(), vec![dim(2.0, "em")]);
        let ctx = ComputeContext {
            parent_font_size: 20.0,
            ..ctx_with_custom(&props)
        };

        let var_fn = ComponentValue::Function(Function {
            name: "var".to_string(),
            value: vec![ComponentValue::PreservedToken(Token::Ident(
                "--gap".to_string(),
            ))],
        });

        // var(--gap) where --gap = 2em, parent font-size = 20px → 40px
        let result = compute_value("margin-top", &[var_fn], &ctx);
        let cvs = result.tokens();
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
                assert_eq!(n.value, 40.0);
                assert_eq!(u, "px");
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    // —— §3 var() 循环检测 ——

    fn var_fn(name: &str) -> ComponentValue {
        ComponentValue::Function(Function {
            name: "var".to_string(),
            value: vec![ComponentValue::PreservedToken(Token::Ident(
                name.to_string(),
            ))],
        })
    }

    #[test]
    fn var_self_reference_returns_empty() {
        // --a: var(--a) → 自引用 → 空
        let mut props = HashMap::new();
        props.insert("--a".to_string(), vec![var_fn("--a")]);
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn("--a")], &ctx);
        assert!(
            result.tokens().is_empty(),
            "self-cycle must resolve to empty"
        );
    }

    #[test]
    fn var_two_cycle_returns_empty() {
        // --a: var(--b); --b: var(--a) → 双环 → 空
        let mut props = HashMap::new();
        props.insert("--a".to_string(), vec![var_fn("--b")]);
        props.insert("--b".to_string(), vec![var_fn("--a")]);
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn("--a")], &ctx);
        assert!(
            result.tokens().is_empty(),
            "two-cycle must resolve to empty"
        );
    }

    #[test]
    fn var_triangle_cycle_returns_empty() {
        // --a: var(--b); --b: var(--c); --c: var(--a) → 三角环 → 空
        let mut props = HashMap::new();
        props.insert("--a".to_string(), vec![var_fn("--b")]);
        props.insert("--b".to_string(), vec![var_fn("--c")]);
        props.insert("--c".to_string(), vec![var_fn("--a")]);
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn("--a")], &ctx);
        assert!(
            result.tokens().is_empty(),
            "triangle-cycle must resolve to empty"
        );
    }

    #[test]
    fn var_normal_chain_still_resolves() {
        // --a: var(--b); --b: red → 正常链 → red
        let mut props = HashMap::new();
        props.insert("--a".to_string(), vec![var_fn("--b")]);
        props.insert(
            "--b".to_string(),
            vec![ComponentValue::PreservedToken(Token::Ident(
                "red".to_string(),
            ))],
        );
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn("--a")], &ctx);
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1);
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Ident(s)) => assert_eq!(s, "red"),
            other => panic!("expected Ident, got {:?}", other),
        }
    }

    #[test]
    fn var_repeated_reference_is_not_a_cycle() {
        // --a: var(--b) var(--b); --b: red → 同一属性重复引用不是环
        let mut props = HashMap::new();
        props.insert("--a".to_string(), vec![var_fn("--b"), var_fn("--b")]);
        props.insert(
            "--b".to_string(),
            vec![ComponentValue::PreservedToken(Token::Ident(
                "red".to_string(),
            ))],
        );
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn("--a")], &ctx);
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 2);
        let reds = cvs
            .iter()
            .filter(|cv| {
                matches!(
                    cv,
                    ComponentValue::PreservedToken(Token::Ident(s)) if s == "red"
                )
            })
            .count();
        assert_eq!(reds, 2);
    }

    // —— var() 记忆化（P1-1 / PERF-3）与 invalid-at-computed-value（P2-5）——

    /// `var(--name, fallback)` 形式的函数节点。
    fn var_fn_fb(name: &str, fallback: &str) -> ComponentValue {
        ComponentValue::Function(Function {
            name: "var".to_string(),
            value: vec![
                ComponentValue::PreservedToken(Token::Ident(name.to_string())),
                ComponentValue::PreservedToken(Token::Comma),
                ComponentValue::PreservedToken(Token::Whitespace),
                ComponentValue::PreservedToken(Token::Ident(fallback.to_string())),
            ],
        })
    }

    #[test]
    fn var_doubling_chain_resolves_in_linear_time() {
        // --v0: red; --v{i}: var(--v{i-1}) var(--v{i-1})
        // 输出规模 2^N 是语义使然；记忆化保证每级只展开一次，时间线性于输出。
        // 修复前（N=22）需 30.9s 且按 N×2^N 指数重复递归。
        const N: usize = 18;
        let mut props = HashMap::new();
        props.insert(
            "--v0".to_string(),
            vec![ComponentValue::PreservedToken(Token::Ident(
                "red".to_string(),
            ))],
        );
        for i in 1..=N {
            let prev = format!("--v{}", i - 1);
            props.insert(format!("--v{}", i), vec![var_fn(&prev), var_fn(&prev)]);
        }
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn(&format!("--v{}", N))], &ctx);
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1 << N, "doubling chain expands to 2^N tokens");
        let reds = cvs
            .iter()
            .filter(|cv| {
                matches!(
                    cv,
                    ComponentValue::PreservedToken(Token::Ident(s)) if s == "red"
                )
            })
            .count();
        assert_eq!(reds, 1 << N, "all tokens must be `red`");
    }

    #[test]
    fn var_cycle_with_fallback_resolves_to_fallback() {
        // P1-2: 环中变量 guaranteed-invalid，但 var() 有 fallback 则解析 fallback。
        // --a: var(--b, red); --b: var(--a, blue) → var(--a) 得 blue
        // （--b 值含 var(--a, blue)：--a 在 gray 命中 → fallback blue → --b=blue
        //   → var(--b, red) 用 --b 计算值 blue，忽略 fallback red）。
        let mut props = HashMap::new();
        props.insert("--a".to_string(), vec![var_fn_fb("--b", "red")]);
        props.insert("--b".to_string(), vec![var_fn_fb("--a", "blue")]);
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn("--a")], &ctx);
        let cvs = result.tokens();
        let idents: Vec<_> = cvs
            .iter()
            .filter_map(|cv| match cv {
                ComponentValue::PreservedToken(Token::Ident(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(
            idents.iter().any(|s| s == "blue"),
            "expected fallback blue, got {:?}",
            idents
        );
    }

    #[test]
    fn var_undefined_with_fallback_still_resolves() {
        // 未定义变量 + fallback → 仍解析为 fallback（既有行为保持）
        let props = HashMap::new();
        let ctx = ctx_with_custom(&props);
        let result = compute_value("color", &[var_fn_fb("--nope", "green")], &ctx);
        let cvs = result.tokens();
        assert!(
            cvs.iter().any(|cv| matches!(
                cv,
                ComponentValue::PreservedToken(Token::Ident(s)) if s == "green"
            )),
            "expected fallback green, got {:?}",
            cvs
        );
    }

    #[test]
    fn compute_value_with_reports_invalid_var() {
        // P2-5: var(color) 首参非 --* → Err（invalid at computed-value time）
        let props = HashMap::new();
        let ctx = ctx_with_custom(&props);
        let bad = ComponentValue::Function(Function {
            name: "var".to_string(),
            value: vec![ComponentValue::PreservedToken(Token::Ident(
                "color".to_string(),
            ))],
        });
        assert!(
            compute_value_with("color", &[bad], &ctx).is_err(),
            "var(color) must be Err via compute_value_with"
        );
        // 兼容包装：compute_value 对该场景返回空 tokens（旧行为）
        let empty = compute_value(
            "color",
            &[ComponentValue::Function(Function {
                name: "var".to_string(),
                value: vec![ComponentValue::PreservedToken(Token::Ident(
                    "color".to_string(),
                ))],
            })],
            &ctx,
        );
        assert!(empty.tokens().is_empty());
    }

    // —— 混合值 ——

    #[test]
    fn keyword_value_preserved() {
        let id = ComponentValue::PreservedToken(Token::Ident("auto".to_string()));
        let result = compute_value("width", &[id], &empty_ctx());
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1);
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Ident(s)) => {
                assert_eq!(s, "auto");
            }
            other => panic!("expected Ident, got {:?}", other),
        }
    }

    // —— 绝对长度单位换算（P2-1，CSS Values L4 §5.1）——
    // 换算锚点：1in = 96px；1in = 2.54cm = 25.4mm = 101.6q = 72pt = 6pc。

    /// 断言单 Dimension token 的值接近 `expected` 且单位为 px。
    fn assert_px(result: &ComputedValue, expected: f64) {
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1, "expected single token, got {:?}", cvs);
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
                assert!(
                    (n.value - expected).abs() < 1e-9,
                    "expected ~{expected}px, got {}{}",
                    n.value,
                    u
                );
                assert_eq!(u, "px");
            }
            other => panic!("expected Dimension, got {:?}", other),
        }
    }

    #[test]
    fn pt_converts_to_px() {
        // 72pt = 1in = 96px → 1pt = 96/72 px
        assert_px(
            &compute_value("width", &[dim(72.0, "pt")], &empty_ctx()),
            96.0,
        );
    }

    #[test]
    fn pc_converts_to_px() {
        // 6pc = 1in = 96px → 1pc = 16px
        assert_px(
            &compute_value("width", &[dim(2.0, "pc")], &empty_ctx()),
            32.0,
        );
    }

    #[test]
    fn in_converts_to_px() {
        assert_px(
            &compute_value("width", &[dim(1.0, "in")], &empty_ctx()),
            96.0,
        );
    }

    #[test]
    fn cm_converts_to_px() {
        // 2.54cm = 1in = 96px
        assert_px(
            &compute_value("width", &[dim(2.54, "cm")], &empty_ctx()),
            96.0,
        );
    }

    #[test]
    fn mm_converts_to_px() {
        // 25.4mm = 1in = 96px
        assert_px(
            &compute_value("width", &[dim(25.4, "mm")], &empty_ctx()),
            96.0,
        );
    }

    #[test]
    fn q_converts_to_px() {
        // 101.6q = 1in = 96px
        assert_px(
            &compute_value("width", &[dim(101.6, "q")], &empty_ctx()),
            96.0,
        );
    }

    // —— calc() 数值求值（P3-2，CSS Values L4 §9）——

    fn num(value: f64) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Number(Numeric {
            value,
            is_integer: false,
        }))
    }

    fn delim(c: char) -> ComponentValue {
        ComponentValue::PreservedToken(Token::Delim(c))
    }

    fn ws() -> ComponentValue {
        ComponentValue::PreservedToken(Token::Whitespace)
    }

    fn paren(args: Vec<ComponentValue>) -> ComponentValue {
        ComponentValue::SimpleBlock(SimpleBlock {
            kind: BlockKind::Paren,
            value: args,
        })
    }

    fn calc_fn(args: Vec<ComponentValue>) -> ComponentValue {
        ComponentValue::Function(Function {
            name: "calc".to_string(),
            value: args,
        })
    }

    /// 断言结果是单个 token，值近似 expected，种类为 kind（dimension/number/percentage）。
    fn assert_single(result: &ComputedValue, expected: f64, kind: &str) {
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1, "expected single token, got {:?}", cvs);
        match &cvs[0] {
            ComponentValue::PreservedToken(Token::Dimension(n, u)) => {
                assert_eq!(kind, "dimension", "expected dimension, got {:?}", cvs[0]);
                assert!((n.value - expected).abs() < 1e-9, "got {}{}", n.value, u);
            }
            ComponentValue::PreservedToken(Token::Number(n)) => {
                assert_eq!(kind, "number", "expected number, got {:?}", cvs[0]);
                assert!((n.value - expected).abs() < 1e-9, "got {}", n.value);
            }
            ComponentValue::PreservedToken(Token::Percentage(n)) => {
                assert_eq!(kind, "percentage", "expected percentage, got {:?}", cvs[0]);
                assert!((n.value - expected).abs() < 1e-9, "got {}%", n.value);
            }
            other => panic!("expected {} token, got {:?}", kind, other),
        }
    }

    /// 断言结果是保留的 calc() 函数（不可求值场景）。
    fn assert_calc_kept(result: &ComputedValue) {
        let cvs = result.tokens();
        assert_eq!(cvs.len(), 1, "expected single calc fn, got {:?}", cvs);
        assert!(
            matches!(
                &cvs[0],
                ComponentValue::Function(f) if f.name.eq_ignore_ascii_case("calc")
            ),
            "expected calc function, got {:?}",
            cvs[0]
        );
    }

    #[test]
    fn calc_add_same_units() {
        // calc(10px + 5px) → 15px
        let expr = calc_fn(vec![
            dim(10.0, "px"),
            ws(),
            delim('+'),
            ws(),
            dim(5.0, "px"),
        ]);
        assert_single(
            &compute_value("width", &[expr], &empty_ctx()),
            15.0,
            "dimension",
        );
    }

    #[test]
    fn calc_sub_same_units() {
        // calc(10px - 5px) → 5px
        let expr = calc_fn(vec![
            dim(10.0, "px"),
            ws(),
            delim('-'),
            ws(),
            dim(5.0, "px"),
        ]);
        assert_single(
            &compute_value("width", &[expr], &empty_ctx()),
            5.0,
            "dimension",
        );
    }

    #[test]
    fn calc_mul_by_number() {
        // calc(2 * 3px) → 6px；calc(3px * 2) → 6px（乘法交换）
        let expr1 = calc_fn(vec![num(2.0), ws(), delim('*'), ws(), dim(3.0, "px")]);
        assert_single(
            &compute_value("width", &[expr1], &empty_ctx()),
            6.0,
            "dimension",
        );
        let expr2 = calc_fn(vec![dim(3.0, "px"), ws(), delim('*'), ws(), num(2.0)]);
        assert_single(
            &compute_value("width", &[expr2], &empty_ctx()),
            6.0,
            "dimension",
        );
    }

    #[test]
    fn calc_div_by_number() {
        // calc(10px / 2) → 5px
        let expr = calc_fn(vec![dim(10.0, "px"), ws(), delim('/'), ws(), num(2.0)]);
        assert_single(
            &compute_value("width", &[expr], &empty_ctx()),
            5.0,
            "dimension",
        );
    }

    #[test]
    fn calc_precedence_mul_before_add() {
        // calc(10px + 5px * 2) → 20px（乘法优先）
        let expr = calc_fn(vec![
            dim(10.0, "px"),
            ws(),
            delim('+'),
            ws(),
            dim(5.0, "px"),
            ws(),
            delim('*'),
            ws(),
            num(2.0),
        ]);
        assert_single(
            &compute_value("width", &[expr], &empty_ctx()),
            20.0,
            "dimension",
        );
    }

    #[test]
    fn calc_paren_group() {
        // calc((10px + 5px) * 2) → 30px
        let group = paren(vec![
            dim(10.0, "px"),
            ws(),
            delim('+'),
            ws(),
            dim(5.0, "px"),
        ]);
        let expr = calc_fn(vec![group, ws(), delim('*'), ws(), num(2.0)]);
        assert_single(
            &compute_value("width", &[expr], &empty_ctx()),
            30.0,
            "dimension",
        );
    }

    #[test]
    fn calc_number_only() {
        // calc(10 + 5) → 15（flex-grow 等数字属性）
        let expr = calc_fn(vec![num(10.0), ws(), delim('+'), ws(), num(5.0)]);
        assert_single(
            &compute_value("flex-grow", &[expr], &empty_ctx()),
            15.0,
            "number",
        );
    }

    #[test]
    fn calc_percentage_only() {
        // calc(50% + 25%) → 75%
        let expr = calc_fn(vec![pct(50.0), ws(), delim('+'), ws(), pct(25.0)]);
        assert_single(
            &compute_value("width", &[expr], &empty_ctx()),
            75.0,
            "percentage",
        );
    }

    #[test]
    fn calc_mixed_percentage_length_kept() {
        // calc(100% - 10px)：百分比基准未知 → 不可求值 → 保留 calc 函数
        let expr = calc_fn(vec![pct(100.0), ws(), delim('-'), ws(), dim(10.0, "px")]);
        assert_calc_kept(&compute_value("width", &[expr], &empty_ctx()));
    }

    #[test]
    fn calc_divide_by_zero_kept() {
        // calc(10px / 0)：除零 → 不可求值 → 保留 calc 函数
        let expr = calc_fn(vec![dim(10.0, "px"), ws(), delim('/'), ws(), num(0.0)]);
        assert_calc_kept(&compute_value("width", &[expr], &empty_ctx()));
    }

    #[test]
    fn calc_incompatible_types_kept() {
        // calc(10px + 5)：length + number → 不可求值 → 保留
        let expr = calc_fn(vec![dim(10.0, "px"), ws(), delim('+'), ws(), num(5.0)]);
        assert_calc_kept(&compute_value("width", &[expr], &empty_ctx()));
    }

    #[test]
    fn calc_dimension_times_dimension_kept() {
        // calc(10px * 2px)：dimension × dimension → 不可求值 → 保留
        let expr = calc_fn(vec![
            dim(10.0, "px"),
            ws(),
            delim('*'),
            ws(),
            dim(2.0, "px"),
        ]);
        assert_calc_kept(&compute_value("width", &[expr], &empty_ctx()));
    }

    #[test]
    fn calc_relative_units_resolved_before_eval() {
        // calc(2em + 3px)：em 先归一到 px（父 font-size=20px → 40px）→ 43px
        let ctx = ComputeContext {
            parent_font_size: 20.0,
            ..empty_ctx()
        };
        let expr = calc_fn(vec![dim(2.0, "em"), ws(), delim('+'), ws(), dim(3.0, "px")]);
        assert_single(&compute_value("width", &[expr], &ctx), 43.0, "dimension");
    }
}
