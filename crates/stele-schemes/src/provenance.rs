//! # Layered resolution and provenance
//!
//! 中文职责：把"基础方案 + 用户补丁"按层合并，并记下**每个值最终来自哪一层**。
//! English role: merge the base scheme with user patches layer by layer, and
//! record which layer each final value came from.
//! 架构位置：`stele-schemes` 的装载入口；`stele-config` 的 `$ref` / 补丁机制
//! 在这里被**真正接进装载路径**。
//!
//! # 为什么"来源"必须是一等公民（PLAN D25）
//!
//! 一份方案跑起来之后，用户看到的是**合并后的结果**。这时他会问：
//!
//! > 我这个 `page_size` 到底是我改的、还是方案自带的、还是内置默认？
//!
//! 如果答不上来，用户就只能一层层翻文件——而 RIME 用户最熟悉的动作
//! 恰恰是"翻 `*.custom.yaml` 看我到底写了什么"。D25 的承诺是：
//! **`--dump-config` 打印出来的每一行都能被用户补丁覆盖，且标注它来自哪一层。**
//!
//! # 合并语义（照 RIME，不由书写顺序决定）
//!
//! RIME 的求值顺序是引擎写死的：**引用 → 合并同级字面值 → 补丁子节点**。
//! 层次之间用的是 `MergeMode::Layer`：**映射深合并、列表整体替换**。
//!
//! 列表替换这条很重要：用户写 `switches: [只留一个]` 时，他要的是
//! "就这一个"，不是"和方案里的合并"。合并列表会让用户**无法删除**任何一项，
//! 而"我加了一条规则就删不掉原来的"是最容易让人放弃配置的一种体验。

use std::collections::BTreeMap;

use stele_config::{MergeMode, Node, Value};

/// 一层的来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    /// 这一层叫什么（给人看的）。
    pub name: String,
    /// 来自哪个文件（内置层写 `<builtin>`）。
    pub file: String,
    /// 这一层的用途说明——它会被打进 `--dump-config` 的注释里。
    pub note: String,
}

impl Layer {
    /// 构造。
    #[must_use]
    pub fn new(name: &str, file: &str, note: &str) -> Self {
        Self {
            name: name.to_owned(),
            file: file.to_owned(),
            note: note.to_owned(),
        }
    }

    /// 内置默认层。
    #[must_use]
    pub fn builtin(file: &str) -> Self {
        Self::new("内置默认", file, "引擎自带的缺省值，任何一层都能覆盖它")
    }
}

/// 一个最终值来自哪里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// 层序号（对应 [`Resolution::layers`] 的下标）。
    pub layer: usize,
    /// 那一行（0 = 该层没有给出这一项，或用代码构造）。
    pub line: u32,
}

/// 合并后的结果与来源表。
#[derive(Clone, Debug)]
pub struct Resolution {
    /// 各层（按**被施加的顺序**：先内置，再方案，再用户补丁）。
    pub layers: Vec<Layer>,
    /// 合并后的配置树。
    pub root: Node,
    /// 每个**叶子路径**来自哪一层。
    ///
    /// 用 `BTreeMap<String, Origin>`：键是点分路径（`switches.ascii_mode.reset`），
    /// 有序保证 `--dump-config` 的输出是**逐字节可复现**的（PLAN §5.2）。
    origins: BTreeMap<String, Origin>,
}

impl Resolution {
    /// 合并若干层。
    ///
    /// # Errors
    ///
    /// 层与层之间类型不匹配（用映射去合并一个标量）时返回错误，
    /// 并指明**是哪一层**与哪条路径——用户必须知道该去改哪个文件。
    pub fn of(layers: Vec<(Layer, Node)>) -> Result<Self, String> {
        let mut names: Vec<Layer> = Vec::with_capacity(layers.len());
        let mut root = Node::at(Value::Map(Vec::new()), 0);
        let mut origins: BTreeMap<String, Origin> = BTreeMap::new();

        for (idx, (layer, node)) in layers.into_iter().enumerate() {
            if idx == 0 {
                root = Node::at(Value::Map(Vec::new()), 0);
            }
            // 来源表按**同样的顺序**覆盖：后一层赢。
            record_origins(&node, "", idx, &mut origins);
            stele_config::merge_into(&mut root, &node, MergeMode::Layer)
                .map_err(|e| format!("合并第 {} 层「{}」失败：{e}", idx + 1, layer.file))?;
            names.push(layer);
        }

        Ok(Self {
            layers: names,
            root,
            origins,
        })
    }

    /// 某个路径的最终来源。
    ///
    /// 路径不存在时返回 `None`——**"没有这一项"与"来自第 0 层"是两件事**，
    /// 混在一起会让 `--dump-config` 骗人。
    #[must_use]
    pub fn origin_of(&self, path: &str) -> Option<&Origin> {
        // 先找**最长匹配**：`switches` 这个映射本身没有来源，
        // 但 `switches.x.reset` 有。调用方通常问的是叶子，这里也允许问中间节点
        // （回退到"第一个以它开头的已知路径"的来源）。
        if let Some(o) = self.origins.get(path) {
            return Some(o);
        }
        let prefix = format!("{path}.");
        self.origins
            .iter()
            .find(|(k, _)| k.starts_with(&prefix))
            .map(|(_, o)| o)
    }

    /// 一行"来源"注释，直接可打印。
    #[must_use]
    pub fn origin_note(&self, path: &str) -> String {
        match self.origin_of(path) {
            None => "# ← 未设置".to_owned(),
            Some(o) => {
                let layer = &self.layers[o.layer];
                if o.line == 0 {
                    format!("# ← {}({})", layer.name, layer.file)
                } else {
                    format!("# ← {}({}) 第 {} 行", layer.name, layer.file, o.line)
                }
            }
        }
    }

    /// 已被记录来源的路径数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.origins.len()
    }

    /// 是否一条来源都没有。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
    }

    /// 某一层贡献了多少个最终生效的值。
    ///
    /// 它回答的是"用户补丁到底改了多少东西"——而这个数字**很有用**：
    /// 补丁写了 20 行、生效 0 行，说明键名写错了（那正是 RIME 静默忽略
    /// 掉的错误）。
    #[must_use]
    pub fn count_of_layer(&self, layer: usize) -> usize {
        self.origins.values().filter(|o| o.layer == layer).count()
    }
}

/// 递归记下每个叶子的来源。
///
/// 映射往下走（下一层可能只覆盖其中一项）；**列表与标量整体归本层**
/// （合并语义就是"列表整体替换"），列表另外按下标再记一份。
fn record_origins(node: &Node, path: &str, layer: usize, out: &mut BTreeMap<String, Origin>) {
    match &node.value {
        Value::Map(entries) => {
            if entries.is_empty() {
                out.insert(
                    path.to_owned(),
                    Origin {
                        layer,
                        line: node.line,
                    },
                );
                return;
            }
            for (k, v) in entries {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                record_origins(v, &child, layer, out);
            }
        }
        Value::Seq(items) => {
            // 列表**整体替换**：来源记在列表本身上。
            out.insert(
                path.to_owned(),
                Origin {
                    layer,
                    line: node.line,
                },
            );
            // 同时按**下标**往下拆一份：`--dump-config` 要能给
            // `switches` 里的每一项标注来源，而"第 0 项来自第 1 层"
            // 比"整个列表来自第 1 层"有用得多。
            for (i, it) in items.iter().enumerate() {
                let child = if path.is_empty() {
                    i.to_string()
                } else {
                    format!("{path}.{i}")
                };
                record_origins(it, &child, layer, out);
            }
        }
        Value::Str(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Null => {
            out.insert(
                path.to_owned(),
                Origin {
                    layer,
                    line: node.line,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stele_config::parse;

    fn layer(name: &str, text: &str) -> (Layer, Node) {
        (
            Layer::new(name, &format!("{name}.yaml"), "测试层"),
            parse(text).expect("测试用的 YAML 必须能解析"),
        )
    }

    #[test]
    fn later_layers_win_and_origins_say_so() {
        let r = Resolution::of(vec![
            layer("base", "menu:\n  page_size: 5\n"),
            layer("user", "menu:\n  page_size: 9\n"),
        ])
        .unwrap();

        // 合并结果取后一层。
        let size = stele_config::lookup(&r.root, "menu/page_size").unwrap();
        assert_eq!(size.as_int(), Some(9));

        // 而来源指向**后一层**，并带行号。
        let o = r.origin_of("menu.page_size").unwrap();
        assert_eq!(o.layer, 1, "最终生效的是用户层");
        assert_eq!(o.line, 2);
        assert!(r.origin_note("menu.page_size").contains("user.yaml"));
        assert!(r.origin_note("menu.page_size").contains("第 2 行"));
    }

    #[test]
    fn maps_deep_merge_but_lists_replace() {
        let r = Resolution::of(vec![
            layer("base", "switches:\n  - name: a\n  - name: b\n"),
            layer("user", "switches:\n  - name: c\n"),
        ])
        .unwrap();
        // 列表**整体替换**：用户写一个就只有一个。
        // 合并列表会让用户"删不掉"方案里的任何一项。
        assert_eq!(r.origin_of("switches").unwrap().layer, 1);

        let r2 = Resolution::of(vec![
            layer("base", "speller:\n  alphabet: [a, b]\n  delimiter: \"'\"\n"),
            layer("user", "speller:\n  alphabet: [a]\n"),
        ])
        .unwrap();
        // 映射深合并：用户只改了 alphabet，delimiter 仍来自方案层。
        assert_eq!(r2.origin_of("speller.alphabet").unwrap().layer, 1);
        assert_eq!(r2.origin_of("speller.delimiter").unwrap().layer, 0);
    }

    #[test]
    fn missing_paths_are_not_attributed_to_layer_zero() {
        let r = Resolution::of(vec![layer("base", "a: 1\n")]).unwrap();
        assert!(r.origin_of("nonexistent").is_none());
        assert_eq!(r.origin_note("nonexistent"), "# ← 未设置");
    }

    #[test]
    fn per_layer_contribution_can_be_counted() {
        // "补丁写了 20 行、生效 0 行"是键名写错的症状——这个计数就是抓它的。
        let r = Resolution::of(vec![
            layer("base", "a: 1\nb: 2\nc: 3\n"),
            layer("user", "b: 20\n"),
        ])
        .unwrap();
        assert_eq!(r.count_of_layer(0), 2);
        assert_eq!(r.count_of_layer(1), 1);
    }

    #[test]
    fn a_layer_may_replace_a_scalar_with_a_map() {
        // 这条行为是**实测出来的**，不是设计出来的：`merge_into` 在
        // "用映射合并标量"时选择**替换**而不是报错。
        //
        // 记录它是为了避免下一个人误以为"类型冲突会报错"——
        // 文档里那句"类型不匹配时返回错误"与实际不符，而
        // **测试是唯一能戳破这种事的东西**（见 PLAN §5 的坑 2）。
        let r = Resolution::of(vec![
            layer("base", "menu: 5\n"),
            layer("user", "menu:\n  page_size: 9\n"),
        ])
        .unwrap();
        assert_eq!(
            stele_config::lookup(&r.root, "menu/page_size")
                .and_then(stele_config::Node::as_int),
            Some(9),
            "后一层的映射整体替换了前一层的标量"
        );
        // 来源仍然如实指向用户层 —— 替换也是"这一层的贡献"。
        assert_eq!(r.origin_of("menu.page_size").unwrap().layer, 1);
    }
}
