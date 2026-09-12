//! # Cross-file references and layering
//!
//! 中文职责：`$ref` 跨文件节点引用、以及分层配置的合并。
//! English role: `$ref` cross-file node references, and layered merging.
//! 架构位置：`stele-config` 在解析之后、类型化之前的一步。
//!
//! # 为什么需要 `$ref`（G1）
//!
//! RIME 有一项我们最初漏掉的能力：**在一个配置里引用另一个文件的某个节点**。
//!
//! ```yaml
//! punctuator:
//!   __include: default:/punctuator      # RIME 的写法
//! ```
//!
//! rime-ice 大量使用它。没有它，每份方案都要把标点表、按键绑定整段重抄一遍。
//!
//! **我们的等价写法**是 `$ref`：
//!
//! ```yaml
//! punctuator:
//!   $ref: "default:/punctuator"
//! ```
//!
//! # 三条实现约束（照 RIME 的语义）
//!
//! 1. **被引用的节点永不被修改**——`$ref` 是**复制**语义，不是别名。
//! 2. **检测循环引用**并在加载期报错，不是栈溢出。
//! 3. **可选引用**：写成 `"default:/punctuator?"` 时，目标不存在不算错误
//!    （RIME 用 `?` 后缀表达同一件事）。
//!
//! RIME 的求值顺序是**引擎写死的**，不由书写顺序决定
//! （「由於 YAML map 的 key 是無序的，書寫順序並不決定編譯指令的先後」）：
//! **引用 → 合并同级字面值 → 补丁子节点**。我们照此实现。

use crate::value::{Node, Value};

/// 补丁/引用阶段的错误。**一定带上下文**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchError {
    /// 出错时正在处理的位置（文件 + 路径）。
    pub where_: String,
    /// 人话解释。
    pub message: String,
}

impl core::fmt::Display for PatchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}：{}", self.where_, self.message)
    }
}

impl std::error::Error for PatchError {}

/// 一组具名的已解析文档（例如 `default`、`stele`）。
#[derive(Clone, Debug, Default)]
pub struct Registry {
    docs: Vec<(String, Node)>,
}

impl Registry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一份文档。
    pub fn insert(&mut self, name: impl Into<String>, doc: Node) {
        let name = name.into();
        if let Some(slot) = self.docs.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = doc;
        } else {
            self.docs.push((name, doc));
        }
    }

    /// 取一份文档。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Node> {
        self.docs.iter().find(|(n, _)| n == name).map(|(_, d)| d)
    }

    /// 已登记的名字（有序）。
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.docs.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// 把某份文档里的所有 `$ref` 展开。
    ///
    /// # Errors
    ///
    /// 引用目标不存在（且非可选）、循环引用、路径写错时返回 [`PatchError`]。
    pub fn expand(&self, doc_name: &str) -> Result<Node, PatchError> {
        let Some(root) = self.get(doc_name) else {
            return Err(PatchError {
                where_: doc_name.to_owned(),
                message: "文档不存在".into(),
            });
        };
        let mut stack: Vec<String> = vec![doc_name.to_owned()];
        self.expand_node(root, doc_name, "", &mut stack)
    }

    fn expand_node(
        &self,
        node: &Node,
        doc_name: &str,
        path: &str,
        stack: &mut Vec<String>,
    ) -> Result<Node, PatchError> {
        match &node.value {
            Value::Map(entries) => {
                // `$ref` 必须独占该节点（可以带一个 `$patch` 兄弟）。
                if let Some(ref_node) = entries.iter().find(|(k, _)| k == "$ref") {
                    let extra: Vec<&(String, Node)> = entries
                        .iter()
                        .filter(|(k, _)| k != "$ref" && k != "$patch")
                        .collect();
                    if !extra.is_empty() {
                        return Err(PatchError {
                            where_: loc(doc_name, path),
                            message: format!(
                                "`$ref` 只能与 `$patch` 并列，但这里还有其它键：{}。\
                                 引用的语义是「把那个节点整个搬过来」，再多写别的键含义不清。",
                                extra
                                    .iter()
                                    .map(|(k, _)| format!("`{k}`"))
                                    .collect::<Vec<_>>()
                                    .join("、")
                            ),
                        });
                    }

                    let target_spec = ref_node.1.as_str().ok_or_else(|| PatchError {
                        where_: loc(doc_name, path),
                        message: "`$ref` 的值必须是字符串，例如 `\"default:/punctuator\"`".into(),
                    })?;

                    let (target_doc, target_path, optional) = parse_ref(&target_spec);
                    let target_doc = if target_doc.is_empty() {
                        doc_name.to_owned()
                    } else {
                        target_doc
                    };
                    let key = format!("{target_doc}:{target_path}");

                    // 循环引用：报错，而不是栈溢出。
                    if stack.contains(&key) {
                        return Err(PatchError {
                            where_: loc(doc_name, path),
                            message: format!(
                                "循环引用：`{key}` 已在引用链里（{}）。\
                                 引用必须是复制语义，不能互相指向。",
                                stack.join(" → ")
                            ),
                        });
                    }

                    let resolved = match self.get(&target_doc) {
                        Some(target_root) => {
                            let sub = lookup(target_root, &target_path).ok_or_else(|| PatchError {
                                where_: loc(doc_name, path),
                                message: if optional {
                                    String::new()
                                } else {
                                    format!(
                                        "引用的节点不存在：`{target_spec}`。\
                                         若该节点确实可能缺失，请在末尾加 `?`\
                                         （可选引用），例如 `\"{target_spec}?\"`。"
                                    )
                                },
                            });
                            match sub {
                                Ok(n) => {
                                    stack.push(key);
                                    let r = self.expand_node(n, &target_doc, &target_path, stack);
                                    stack.pop();
                                    Some(r?)
                                }
                                Err(e) if optional => {
                                    let _ = e;
                                    None
                                }
                                Err(e) => return Err(e),
                            }
                        }
                        None if optional => None,
                        None => {
                            return Err(PatchError {
                                where_: loc(doc_name, path),
                                message: format!("引用的文档不存在：`{target_doc}`"),
                            });
                        }
                    };

                    let Some(mut value) = resolved else {
                        return Ok(Node::at(Value::Null, node.line));
                    };

                    // 同级 `$patch`：对引用结果做增量修改。
                    if let Some((_, p)) = entries.iter().find(|(k, _)| k == "$patch") {
                        let expanded_patch = self.expand_node(p, doc_name, path, stack)?;
                        merge_into(&mut value, &expanded_patch, MergeMode::Patch).map_err(|m| {
                            PatchError {
                                where_: loc(doc_name, path),
                                message: format!("`$patch` 应用失败：{m}"),
                            }
                        })?;
                    }

                    return Ok(value);
                }

                // 普通映射：递归展开每个值。
                let mut out = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let child_path = join(path, k);
                    out.push((
                        k.clone(),
                        self.expand_node(v, doc_name, &child_path, stack)?,
                    ));
                }
                Ok(Node::at(Value::Map(out), node.line))
            }
            Value::Seq(items) => {
                let mut out = Vec::with_capacity(items.len());
                for (i, it) in items.iter().enumerate() {
                    let child_path = format!("{path}/@{i}");
                    out.push(self.expand_node(it, doc_name, &child_path, stack)?);
                }
                Ok(Node::at(Value::Seq(out), node.line))
            }
            _ => Ok(node.clone()),
        }
    }
}

fn loc(doc: &str, path: &str) -> String {
    if path.is_empty() {
        doc.to_owned()
    } else {
        format!("{doc}:{path}")
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}/{key}")
    }
}

/// 解析 `"doc:/a/b?"` → `(doc, "a/b", optional)`。
fn parse_ref(spec: &str) -> (String, String, bool) {
    let (body, optional) = match spec.strip_suffix('?') {
        Some(b) => (b, true),
        None => (spec, false),
    };
    if let Some((doc, path)) = body.split_once(":/") {
        (doc.to_owned(), path.to_owned(), optional)
    } else {
        // 没有 `:/`：当作"同一份文档里的路径"。
        let p = body.strip_prefix(':').unwrap_or(body);
        (String::new(), p.to_owned(), optional)
    }
}

/// 按 `a/b/@0/c` 这样的路径取节点。
///
/// 支持 `@n` 索引列表；`@last` 表示最后一项。
#[must_use]
pub fn lookup<'a>(root: &'a Node, path: &str) -> Option<&'a Node> {
    if path.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for seg in path.split('/') {
        if seg.is_empty() {
            continue;
        }
        cur = if let Some(idx) = seg.strip_prefix('@') {
            let items = cur.as_seq()?;
            let i = if idx == "last" {
                items.len().checked_sub(1)?
            } else {
                idx.parse::<usize>().ok()?
            };
            items.get(i)?
        } else {
            cur.get(seg)?
        };
    }
    Some(cur)
}

/// 合并模式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MergeMode {
    /// 分层覆盖：后层覆盖前层。映射**深合并**，列表**整体替换**。
    Layer,
    /// `$patch`：键名以 `+` 结尾表示"合并/追加"而不是替换。
    Patch,
}

/// 把 `patch` 合并进 `target`（就地修改）。
///
/// # Errors
///
/// 类型不匹配（用映射去合并标量）时返回说明性错误。
pub fn merge_into(target: &mut Node, patch: &Node, mode: MergeMode) -> Result<(), String> {
    match (&mut target.value, &patch.value) {
        (Value::Map(dst), Value::Map(src)) => {
            for (k, v) in src {
                // `Patch` 模式下的 `key+`：并入而不是替换。
                let (real_key, additive) = if mode == MergeMode::Patch {
                    match k.strip_suffix('+') {
                        Some(base) => (base.to_owned(), true),
                        None => (k.clone(), false),
                    }
                } else {
                    (k.clone(), false)
                };

                match dst.iter_mut().find(|(dk, _)| *dk == real_key) {
                    Some((_, existing)) => {
                        if additive {
                            match (&mut existing.value, &v.value) {
                                (Value::Seq(d), Value::Seq(s)) => {
                                    d.extend(s.iter().cloned());
                                }
                                (Value::Map(d), Value::Map(s)) => {
                                    let mut merged = Node::at(Value::Map(d.clone()), existing.line);
                                    merge_into(
                                        &mut merged,
                                        &Node::at(Value::Map(s.clone()), v.line),
                                        MergeMode::Layer,
                                    )?;
                                    existing.value = merged.value;
                                }
                                (d, s) => {
                                    return Err(format!(
                                        "`{real_key}+` 要求两边都是列表或映射，\
                                         但左边是{}、右边是{}",
                                        kind_of(d),
                                        kind_of(s)
                                    ));
                                }
                            }
                        } else {
                            merge_into(existing, v, mode)?;
                        }
                    }
                    None => dst.push((real_key, v.clone())),
                }
            }
            Ok(())
        }
        (Value::Map(dst), _) if mode == MergeMode::Patch => {
            // `__patch` 的整体替换：允许用一个标量覆盖整个节点。
            let _ = dst;
            target.value = patch.value.clone();
            Ok(())
        }
        (Value::Seq(dst), Value::Seq(src)) if mode == MergeMode::Patch => {
            dst.extend(src.iter().cloned());
            Ok(())
        }
        _ => {
            // 分层时"后者覆盖前者"是正常语义，不是错误。
            if mode == MergeMode::Layer {
                target.value = patch.value.clone();
                return Ok(());
            }
            Err(format!(
                "类型不匹配：目标是{}，补丁是{}",
                kind_of(&target.value),
                kind_of(&patch.value)
            ))
        }
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "空",
        Value::Bool(_) => "布尔",
        Value::Int(_) => "整数",
        Value::Float(_) => "小数",
        Value::Str(_) => "字符串",
        Value::Seq(_) => "列表",
        Value::Map(_) => "映射",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn reg() -> Registry {
        let mut r = Registry::new();
        r.insert(
            "default",
            parse("punctuator:\n  half: x\n  full: y\nkey_binder:\n  bindings:\n    - a\n")
                .unwrap(),
        );
        r.insert(
            "stele",
            parse("punctuator:\n  $ref: \"default:/punctuator\"\nname: stele\n").unwrap(),
        );
        r
    }

    #[test]
    fn ref_copies_the_target_node() {
        let out = reg().expand("stele").unwrap();
        let half = out
            .get("punctuator")
            .and_then(|p| p.get("half"))
            .and_then(crate::value::Node::as_str);
        assert_eq!(half.as_deref(), Some("x"));
        assert_eq!(out.get("name").unwrap().as_str().as_deref(), Some("stele"));
    }

    #[test]
    fn referenced_node_is_not_mutated() {
        // 复制语义：展开之后原文档必须原封不动。
        let r = reg();
        let _ = r.expand("stele").unwrap();
        let d = r.get("default").unwrap();
        assert_eq!(
            d.get("punctuator")
                .unwrap()
                .get("half")
                .unwrap()
                .as_str()
                .as_deref(),
            Some("x")
        );
    }

    #[test]
    fn ref_with_patch_merges_additively() {
        let mut r = Registry::new();
        r.insert("base", parse("bindings:\n  - a\n").unwrap());
        r.insert(
            "mine",
            parse("bindings:\n  $ref: \"base:/bindings\"\n  $patch:\n    - b\n  $patch_extra: ignored\n")
                .unwrap(),
        );
        // 上面那个 `$patch_extra` 应当触发"$ref 旁还有别的键"的错误。
        assert!(r.expand("mine").is_err());

        let mut r2 = Registry::new();
        r2.insert("base", parse("bindings:\n  - a\n").unwrap());
        r2.insert(
            "mine",
            parse("bindings:\n  $ref: \"base:/bindings\"\n  $patch:\n    - b\n").unwrap(),
        );
        let out = r2.expand("mine").unwrap();
        let items: Vec<String> = out
            .get("bindings")
            .unwrap()
            .as_seq()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(items, ["a", "b"], "$patch 对列表是追加");
    }

    #[test]
    fn optional_ref_tolerates_a_missing_target() {
        let mut r = Registry::new();
        r.insert("base", parse("a: 1\n").unwrap());
        r.insert("mine", parse("x:\n  $ref: \"base:/nope?\"\n").unwrap());
        let out = r.expand("mine").unwrap();
        assert!(matches!(out.get("x").unwrap().value, Value::Null));
    }

    #[test]
    fn missing_target_without_question_mark_is_an_error_that_teaches() {
        let mut r = Registry::new();
        r.insert("base", parse("a: 1\n").unwrap());
        r.insert("mine", parse("x:\n  $ref: \"base:/nope\"\n").unwrap());
        let e = r.expand("mine").unwrap_err();
        assert!(
            e.message.contains("可选引用"),
            "应当告诉用户怎么修：{}",
            e.message
        );
    }

    #[test]
    fn cycles_are_detected_not_crashed() {
        let mut r = Registry::new();
        r.insert("a", parse("x:\n  $ref: \"b:/y\"\n").unwrap());
        r.insert("b", parse("y:\n  $ref: \"a:/x\"\n").unwrap());
        let e = r.expand("a").unwrap_err();
        assert!(e.message.contains("循环引用"), "{}", e.message);
    }

    #[test]
    fn unknown_document_is_an_error() {
        let mut r = Registry::new();
        r.insert("a", parse("x:\n  $ref: \"ghost:/y\"\n").unwrap());
        let e = r.expand("a").unwrap_err();
        assert!(e.message.contains("ghost"), "{}", e.message);
    }

    #[test]
    fn lookup_supports_list_indexes() {
        let n = parse("a:\n  - x\n  - y\n").unwrap();
        assert_eq!(lookup(&n, "a/@1").unwrap().as_str().as_deref(), Some("y"));
        assert_eq!(
            lookup(&n, "a/@last").unwrap().as_str().as_deref(),
            Some("y")
        );
        assert!(lookup(&n, "a/@9").is_none());
        assert!(lookup(&n, "zzz").is_none());
    }

    #[test]
    fn layer_mode_deep_merges_maps_and_replaces_lists() {
        let mut base = parse("engine:\n  processors:\n    - a\n    - b\n  n: 1\n").unwrap();
        let user = parse("engine:\n  processors:\n    - c\n").unwrap();
        merge_into(&mut base, &user, MergeMode::Layer).unwrap();

        let procs = base.get("engine").unwrap().get("processors").unwrap();
        assert_eq!(procs.as_seq().unwrap().len(), 1, "列表在分层时整体替换");
        // 未提及的兄弟键保留。
        assert_eq!(
            base.get("engine").unwrap().get("n").unwrap().as_int(),
            Some(1)
        );
    }

    #[test]
    fn patch_mode_additively_merges_lists() {
        let mut base = parse("bindings:\n  - a\n").unwrap();
        let user = parse("bindings+:\n  - b\n").unwrap();
        merge_into(&mut base, &user, MergeMode::Patch).unwrap();
        assert_eq!(base.get("bindings").unwrap().as_seq().unwrap().len(), 2);
    }
}
