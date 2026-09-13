//! # Tag table
//!
//! 中文职责：把方案里**写出来的标签名**变成 [`Tag`]（`&'static str`）。
//! English role: turn tag names written in scheme data into [`Tag`] values.
//! 架构位置：`stele-engine` 的装载期工具；被方案编译与切分器使用。
//!
//! # 为什么需要它（一处类型选择带来的连锁反应）
//!
//! [`Tag`] 是 `&'static str`，理由是**分段标签来自一个封闭的小集合**
//! （`abc` / `punct` / `radical_lookup`…），因此不该为每个分段分配一个
//! `String`——分段是按键路径上的东西，那里不做堆分配。
//!
//! 但"封闭的小集合"这句话在 P3 之前是**假的**：
//!
//! ```yaml
//! affix_segmentor@radical_lookup:
//!   tag: radical_lookup      # ← 这个名字只存在于方案文件里
//! ```
//!
//! 方案可以声明**它自己的**标签名。于是装载器必须把运行期读到的字符串
//! 变成 `&'static str`。这里用**装载期一次性 intern**：
//!
//! - 每个不同的名字**只泄漏一次**（`Box::leak`），不是每次查询泄漏一次；
//! - 泄漏量有界：一个方案里的标签名是**个位数**，方案每进程装载一次；
//! - 于是按键路径上仍然零分配，[`Segment::tags`] 仍然是 `Vec<Tag>`。
//!
//! **代价是诚实的**：名字表随方案常驻。这不是"忘记释放"，
//! 而是"用一点常驻内存换按键路径零分配"——对输入法这笔交易显然划算
//! （按键 P50 的预算是 1 ms，红线上）。
//!
//! [`Segment::tags`]: stele_core::Segment::tags

use std::collections::BTreeMap;
use stele_core::Tag;

/// 标签名的装载期字典。
///
/// 用 `BTreeMap` 而非 `HashMap`：**凡是顺序可能影响输出的集合一律用有序容器**
/// （PLAN §5.2）。这里顺序本身不影响输出，但 `all()` 的遍历顺序会出现在
/// 诊断信息里——有序才有可复现的报错文本。
#[derive(Debug, Default)]
pub struct TagTable {
    names: BTreeMap<String, Tag>,
}

impl TagTable {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 取一个标签；没有就**登记**它。
    ///
    /// 同一个名字永远返回同一个 `&'static str`——这一条是
    /// `Segment::has_tag`（用 `==` 比较指针值背后指向的内容）能工作的前提，
    /// 也让标签可以当 map 的键用。
    pub fn intern(&mut self, name: &str) -> Tag {
        if let Some(t) = self.names.get(name) {
            return t;
        }
        // 泄漏一次，不再是"每次调用一次"。
        let leaked: Tag = Box::leak(name.to_owned().into_boxed_str());
        self.names.insert(name.to_owned(), leaked);
        leaked
    }

    /// 查一个标签；**不登记**（查询不该产生副作用）。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Tag> {
        self.names.get(name).copied()
    }

    /// 已登记的名字（有序）。
    pub fn all(&self) -> impl Iterator<Item = Tag> + '_ {
        self.names.values().copied()
    }

    /// 已登记的名字数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_name_yields_the_same_static_str() {
        let mut t = TagTable::new();
        let a = t.intern("radical_lookup");
        let b = t.intern("radical_lookup");
        // 同一个名字必须是**同一块内存**，否则按标签分组会莫名其妙地失败。
        assert!(std::ptr::eq(a, b));
        assert_eq!(a, "radical_lookup");
    }

    #[test]
    fn interning_is_idempotent_and_bounded() {
        let mut t = TagTable::new();
        for _ in 0..1000 {
            t.intern("abc");
            t.intern("punct");
        }
        // 泄漏量由**不同名字的个数**决定，不由调用次数决定。
        assert_eq!(t.len(), 2);
        assert_eq!(t.all().count(), 2);
    }

    #[test]
    fn get_does_not_register() {
        let mut t = TagTable::new();
        assert!(t.get("abc").is_none());
        assert!(t.is_empty());
        let _ = t.intern("abc");
        assert_eq!(t.get("abc"), Some("abc"));
    }
}
