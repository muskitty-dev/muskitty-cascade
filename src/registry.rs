//! §7.1/§7.2: Property registry — 属性元数据（初始值、继承标志）。
//!
//! 初始覆盖 ~20 个常用属性。后续可扩展为完整属性数据库。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 属性百分比参考值类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PercentageBasis {
    /// 不接受百分比。
    None,
    /// 参考父元素同属性值。
    ParentSameProperty,
    /// 参考父元素 width。
    ParentWidth,
    /// 参考父元素 height。
    ParentHeight,
    /// 参考父元素 font-size。
    ParentFontSize,
    /// 参考根元素 font-size（rem）。
    RootFontSize,
}

/// 属性元数据定义。
#[derive(Debug, Clone, Copy)]
pub struct PropertyDefinition {
    /// 属性名。
    pub name: &'static str,
    /// §7.1: 初始值。
    pub initial_value: &'static str,
    /// §7.2: 是否继承。
    pub inherited: bool,
    /// 百分比参考。
    pub percentages: PercentageBasis,
}

/// 内置属性表（~20 个常用属性）。
pub static BUILTIN_PROPERTIES: &[PropertyDefinition] = &[
    PropertyDefinition {
        name: "color",
        initial_value: "black",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "font-size",
        initial_value: "medium",
        inherited: true,
        percentages: PercentageBasis::ParentFontSize,
    },
    PropertyDefinition {
        name: "font-family",
        initial_value: "serif",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "font-weight",
        initial_value: "normal",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "line-height",
        initial_value: "normal",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "display",
        initial_value: "inline",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "margin-top",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "margin-right",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "margin-bottom",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "margin-left",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "padding-top",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "padding-right",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "padding-bottom",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "padding-left",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "width",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "height",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentHeight,
    },
    PropertyDefinition {
        name: "background-color",
        initial_value: "transparent",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "visibility",
        initial_value: "visible",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "text-align",
        initial_value: "start",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "opacity",
        initial_value: "1",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    // —— Flexbox 属性（CSS Flexbox Level 1）——
    PropertyDefinition {
        name: "flex-direction",
        initial_value: "row",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "flex-wrap",
        initial_value: "nowrap",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "justify-content",
        initial_value: "normal",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "align-items",
        initial_value: "normal",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "align-self",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "flex-grow",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "flex-shrink",
        initial_value: "1",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "flex-basis",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "gap",
        initial_value: "normal",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "row-gap",
        initial_value: "normal",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "column-gap",
        initial_value: "normal",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "box-sizing",
        initial_value: "content-box",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    // —— P2-7: 补充继承属性 ——
    PropertyDefinition {
        name: "cursor",
        initial_value: "auto",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "direction",
        initial_value: "ltr",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "letter-spacing",
        initial_value: "normal",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "white-space",
        initial_value: "normal",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "word-spacing",
        initial_value: "normal",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "font-style",
        initial_value: "normal",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "text-transform",
        initial_value: "none",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "text-indent",
        initial_value: "0",
        inherited: true,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "tab-size",
        initial_value: "8",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "orphans",
        initial_value: "2",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "widows",
        initial_value: "2",
        inherited: true,
        percentages: PercentageBasis::None,
    },
    // —— P2-7: 补充非继承属性 ——
    PropertyDefinition {
        name: "position",
        initial_value: "static",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "top",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentHeight,
    },
    PropertyDefinition {
        name: "right",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "bottom",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentHeight,
    },
    PropertyDefinition {
        name: "left",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "z-index",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "overflow",
        initial_value: "visible",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "min-width",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "min-height",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::ParentHeight,
    },
    PropertyDefinition {
        name: "max-width",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::ParentWidth,
    },
    PropertyDefinition {
        name: "max-height",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::ParentHeight,
    },
    PropertyDefinition {
        name: "border-top-width",
        initial_value: "medium",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-right-width",
        initial_value: "medium",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-bottom-width",
        initial_value: "medium",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-left-width",
        initial_value: "medium",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-top-style",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-right-style",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-bottom-style",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-left-style",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-top-color",
        initial_value: "currentcolor",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-right-color",
        initial_value: "currentcolor",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-bottom-color",
        initial_value: "currentcolor",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "border-left-color",
        initial_value: "currentcolor",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    // `border` / `border-<side>` / `border-width|style|color` 均为**简写**，
    // 在 filter.rs `expand_shorthand` 阶段展开为上面 12 条方向性长属性，
    // 不需要（也不应）注册——注册表只存长属性，与 margin/padding 一致。
    PropertyDefinition {
        name: "outline-width",
        initial_value: "medium",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "outline-style",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "outline-color",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "flex",
        initial_value: "0 1 auto",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "order",
        initial_value: "0",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "align-content",
        initial_value: "normal",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "justify-items",
        initial_value: "legacy",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "justify-self",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    // —— Grid 属性（CSS Grid Layout Level 1 §7）——
    PropertyDefinition {
        name: "grid-template-columns",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "grid-template-rows",
        initial_value: "none",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "grid-auto-flow",
        initial_value: "row",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "grid-auto-columns",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::None,
    },
    PropertyDefinition {
        name: "grid-auto-rows",
        initial_value: "auto",
        inherited: false,
        percentages: PercentageBasis::None,
    },
];

/// CAS-3：属性名 → 定义的哈希索引（O(1) 查找）。
///
/// 原实现对 [`BUILTIN_PROPERTIES`] 的线性 `eq_ignore_ascii_case` 扫描被
/// 每声明、每默认值、每百分比 token 调用（元素 × 属性量级）。内建表的
/// name 均为 ASCII 小写 `&'static str`：先按原串直查（CSS 属性名惯用
/// 小写，零分配），未命中且含大写时才 `to_ascii_lowercase` 复查一次。
///
/// **不变式**：新增属性定义的 `name` 必须全小写，否则大写查询路径失配。
static PROPERTY_INDEX: OnceLock<HashMap<&'static str, &'static PropertyDefinition>> =
    OnceLock::new();

fn property_index() -> &'static HashMap<&'static str, &'static PropertyDefinition> {
    PROPERTY_INDEX.get_or_init(|| {
        BUILTIN_PROPERTIES
            .iter()
            .map(|p| (p.name, p))
            .collect::<HashMap<_, _>>()
    })
}

/// 查找属性定义。返回 `None` 表示属性未注册。
///
/// CAS-3：经 [`PROPERTY_INDEX`] 哈希查找（O(1)），替代原全表线性扫描。
pub fn lookup_property(name: &str) -> Option<&'static PropertyDefinition> {
    let idx = property_index();
    idx.get(name).copied().or_else(|| {
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            idx.get(&*name.to_ascii_lowercase()).copied()
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_property() {
        let def = lookup_property("color").unwrap();
        assert!(def.inherited);
        assert_eq!(def.initial_value, "black");
    }

    #[test]
    fn lookup_case_insensitive() {
        assert!(lookup_property("COLOR").is_some());
        assert!(lookup_property("Font-Size").is_some());
    }

    #[test]
    fn lookup_unknown_property() {
        assert!(lookup_property("nonexistent").is_none());
    }

    #[test]
    fn inherited_properties() {
        assert!(lookup_property("color").unwrap().inherited);
        assert!(lookup_property("font-size").unwrap().inherited);
        assert!(lookup_property("visibility").unwrap().inherited);
    }

    #[test]
    fn non_inherited_properties() {
        assert!(!lookup_property("display").unwrap().inherited);
        assert!(!lookup_property("width").unwrap().inherited);
        assert!(!lookup_property("background-color").unwrap().inherited);
    }

    #[test]
    fn percentage_basis() {
        assert_eq!(
            lookup_property("width").unwrap().percentages,
            PercentageBasis::ParentWidth
        );
        assert_eq!(
            lookup_property("font-size").unwrap().percentages,
            PercentageBasis::ParentFontSize
        );
        assert_eq!(
            lookup_property("color").unwrap().percentages,
            PercentageBasis::None
        );
    }

    #[test]
    fn builtin_property_count() {
        assert!(BUILTIN_PROPERTIES.len() >= 20);
    }

    #[test]
    fn expanded_registry_inherited() {
        // P2-7: 补充的继承属性
        assert!(lookup_property("white-space").unwrap().inherited);
        assert!(lookup_property("cursor").unwrap().inherited);
        assert!(lookup_property("letter-spacing").unwrap().inherited);
        assert!(lookup_property("word-spacing").unwrap().inherited);
        assert!(lookup_property("font-style").unwrap().inherited);
        assert!(lookup_property("text-transform").unwrap().inherited);
    }

    #[test]
    fn expanded_registry_non_inherited() {
        // P2-7: 补充的非继承属性
        assert!(!lookup_property("position").unwrap().inherited);
        assert!(!lookup_property("z-index").unwrap().inherited);
        assert!(!lookup_property("overflow").unwrap().inherited);
        assert!(!lookup_property("border-top-width").unwrap().inherited);
        assert!(!lookup_property("outline-color").unwrap().inherited);
    }

    #[test]
    fn expanded_registry_initial_values() {
        assert_eq!(
            lookup_property("white-space").unwrap().initial_value,
            "normal"
        );
        assert_eq!(lookup_property("position").unwrap().initial_value, "static");
        assert_eq!(
            lookup_property("overflow").unwrap().initial_value,
            "visible"
        );
    }
}
