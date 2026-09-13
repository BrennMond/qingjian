//! # Error
//!
//! 中文职责：加载期错误类型。运行期**不可失败**（见下）。
//! English role: load-time error types; the runtime is infallible by design.
//! 架构位置：qingjian-core 的错误定义，被 Engine 构造、方案加载、配置校验使用。
//!
//! # 错误策略（`docs/engine-design.md` §5.6）
//!
//! **组件 trait 一律不返回 `Result`**：按键路径（P99 < 10 ms）上不应该有错误分支。
//! 一切可预期的错误都在**加载期**暴露，且应当**一次报完**而不是遇到第一个就返回。
//!
//! **但加载期报错 ≠ 拒绝启动。** 输入法的失败是**自锁**的——一个配置笔误若导致
//! 输入法拒绝启动，用户连"打字去改配置"都做不到。因此调用方（CLI / 前端）的
//! 默认行为是**降级 + 保留上一份可用配置 + 可读诊断**（PLAN D26），不是退出。

use core::fmt;

/// 结构化诊断：说清"哪一层、哪个条目、哪个字段、期望什么"。
///
/// 可读性要求来自 PLAN D17：RIME 的静默忽略是反面教材。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// 出问题的来源层（如 `schemes/qingjian-default/rime_ice.schema.yaml`）。
    pub source: String,
    /// 条目 id（如 `translators/main`），没有则为 `None`。
    pub entry: Option<String>,
    /// 字段路径，没有则为 `None`。
    pub field: Option<String>,
    /// 人话解释：期望什么、实际是什么。
    pub message: String,
}

impl Diagnostic {
    /// 构造一条诊断。
    #[must_use]
    pub fn new(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            entry: None,
            field: None,
            message: message.into(),
        }
    }

    /// 补充条目 id。
    #[must_use]
    pub fn with_entry(mut self, entry: impl Into<String>) -> Self {
        self.entry = Some(entry.into());
        self
    }

    /// 补充字段路径。
    #[must_use]
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source)?;
        if let Some(e) = &self.entry {
            write!(f, " [{e}]")?;
        }
        if let Some(fld) = &self.field {
            write!(f, " .{fld}")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// 加载方案时的错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// 找不到该方案。
    NotFound {
        /// 方案 id。
        schema_id: String,
    },
    /// 方案配置有错。**一次报出全部问题**，不是遇到第一个就返回。
    Invalid {
        /// 方案 id。
        schema_id: String,
        /// 全部诊断。
        diagnostics: Vec<Diagnostic>,
    },
    /// 方案要求的某个服务/零件不存在。
    MissingComponent {
        /// 方案 id。
        schema_id: String,
        /// 缺失的零件名。
        component: String,
    },
    /// 方案格式版本不认识（PLAN D27）。
    UnsupportedFormatVersion {
        /// 方案 id。
        schema_id: String,
        /// 期望的格式版本。
        expected: u32,
        /// 实际读到的版本。
        found: u32,
    },
    /// 引用了不存在的节点（`$ref`，G1）。
    UnresolvedRef {
        /// 引用串。
        target: String,
    },
    /// 检测到循环引用（G1）。
    CircularRef {
        /// 引用串。
        target: String,
    },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { schema_id } => write!(f, "方案不存在：{schema_id}"),
            Self::Invalid {
                schema_id,
                diagnostics,
            } => {
                writeln!(f, "方案 {schema_id} 有 {} 处配置错误：", diagnostics.len())?;
                for d in diagnostics {
                    writeln!(f, "  - {d}")?;
                }
                Ok(())
            }
            Self::MissingComponent {
                schema_id,
                component,
            } => write!(f, "方案 {schema_id} 需要的零件不存在：{component}"),
            Self::UnsupportedFormatVersion {
                schema_id,
                expected,
                found,
            } => write!(
                f,
                "方案 {schema_id} 的格式版本是 {found}，本引擎支持到 {expected}"
            ),
            Self::UnresolvedRef { target } => write!(f, "引用不存在的节点：{target}"),
            Self::CircularRef { target } => write!(f, "循环引用：{target}"),
        }
    }
}

impl std::error::Error for SchemaError {}

/// 引擎级错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QingjianError {
    /// 方案相关错误。
    Schema(SchemaError),
    /// 编译产物校验和不匹配或格式不认识（PLAN D28）。
    ArtifactRejected {
        /// 产物路径或标识。
        artifact: String,
        /// 原因。
        reason: String,
    },
    /// I/O 错误（只在加载期出现）。
    Io {
        /// 出错路径。
        path: String,
        /// 原因。
        reason: String,
    },
}

impl From<SchemaError> for QingjianError {
    fn from(value: SchemaError) -> Self {
        Self::Schema(value)
    }
}

impl fmt::Display for QingjianError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schema(e) => write!(f, "{e}"),
            Self::ArtifactRejected { artifact, reason } => {
                write!(f, "产物被拒绝 {artifact}：{reason}")
            }
            Self::Io { path, reason } => write!(f, "读取 {path} 失败：{reason}"),
        }
    }
}

impl std::error::Error for QingjianError {}

/// 本 crate 的 `Result` 别名。
pub type Result<T> = core::result::Result<T, QingjianError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_schema_lists_every_problem() {
        let e = SchemaError::Invalid {
            schema_id: "demo".into(),
            diagnostics: vec![
                Diagnostic::new("a.yaml", "字段名写错")
                    .with_entry("translators/main")
                    .with_field("dictionary"),
                Diagnostic::new("a.yaml", "缺少必需字段").with_field("schema.schema_id"),
            ],
        };
        let text = e.to_string();
        assert!(text.contains("2 处"), "{text}");
        assert!(text.contains("translators/main"), "{text}");
        assert!(text.contains("dictionary"), "{text}");
    }
}
