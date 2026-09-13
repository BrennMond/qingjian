//! # Lexicon
//!
//! 中文职责：把**编码**映射到词条的内存实现。
//! English role: an in-memory `code → entries` lexicon.
//! 架构位置：`stele-core::Lexicon` 的实现。P2.5 会加一个 mmap 实现，
//! **引擎代码一行都不用改**——这正是把它做成 trait 的原因。
//!
//! # 这里只做精确匹配
//!
//! 变体拼写（简拼 / 模糊音 / 补全 / 纠错）**全部在拼写层解决**，
//! 到这里的时候已经是一条确定的编码了。
//! 好处是这张表可以简单到一次 `BTreeMap` 查找，不需要任何模糊检索结构。

use std::collections::BTreeMap;
use stele_core::{
    Candidate, CandidateSink, CodeAlphabet, CodeUnitId, Lexicon, Origin, Score, Span, SpellingAttr,
};

/// 词条：文本 + 对数域分数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// 上屏文本。
    pub text: String,
    /// 对数域分数（由权重换算，换算只发生在装载期）。
    pub score: Score,
    /// 备注（例如拼音），可选。
    pub comment: Option<String>,
}

/// 内存词库。
///
/// 内部用 `BTreeMap` 而非 `HashMap`：**凡是顺序可能影响输出的集合一律用有序容器**
/// （PLAN §5.2）。同码词条的顺序由装载时的排序确定，不依赖哈希。
#[derive(Debug)]
pub struct InMemoryLexicon {
    alphabet: CodeAlphabet,
    map: BTreeMap<Vec<CodeUnitId>, Vec<Entry>>,
}

/// 装载词库时的错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LexiconError {
    /// 词条引用了字母表里没有的编码单元。
    UnknownUnit {
        /// 出错的编码单元文本。
        unit: String,
        /// 属于哪个词条。
        word: String,
    },
}

impl core::fmt::Display for LexiconError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownUnit { unit, word } => write!(
                f,
                "词条「{word}」引用了字母表里没有的编码单元「{unit}」——\
                 方案数据不一致（字母表与词库必须来自同一个方案）"
            ),
        }
    }
}

impl std::error::Error for LexiconError {}

impl InMemoryLexicon {
    /// 由"编码单元文本序列 → 词 → 权重"的三元组构造。
    ///
    /// # Errors
    ///
    /// 某个编码单元不在字母表里时返回 [`LexiconError::UnknownUnit`]。
    /// **这是一处加载期的响亮失败**：字母表与词库不一致，说明方案数据有错，
    /// 而不是"这个词查不到"。
    pub fn from_entries<S: AsRef<str>, W: AsRef<str>>(
        alphabet: CodeAlphabet,
        entries: &[(Vec<S>, W, f64)],
    ) -> Result<Self, LexiconError> {
        let mut map: BTreeMap<Vec<CodeUnitId>, Vec<Entry>> = BTreeMap::new();

        for (code_texts, word, weight) in entries {
            let mut code = Vec::with_capacity(code_texts.len());
            for t in code_texts {
                let t = t.as_ref();
                let Some(id) = alphabet.id_of(t) else {
                    return Err(LexiconError::UnknownUnit {
                        unit: t.to_owned(),
                        word: word.as_ref().to_owned(),
                    });
                };
                code.push(id);
            }
            map.entry(code).or_default().push(Entry {
                text: word.as_ref().to_owned(),
                score: Score::from_weight(*weight),
                comment: None,
            });
        }

        // 同码词条按"分数降序、文本升序"固定下来——装载期一次排好，
        // 按键路径上不再排序（也保证同分词的顺序确定）。
        for entries in map.values_mut() {
            entries.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.text.cmp(&b.text)));
        }

        Ok(Self { alphabet, map })
    }

    /// 字母表。
    #[must_use]
    pub fn alphabet(&self) -> &CodeAlphabet {
        &self.alphabet
    }

    /// 词条总数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.values().map(Vec::len).sum()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 不同的编码数。
    #[must_use]
    pub fn code_count(&self) -> usize {
        self.map.len()
    }
}

impl Lexicon for InMemoryLexicon {
    fn lookup(&self, code: &[CodeUnitId], out: &mut CandidateSink<'_>) {
        let Some(entries) = self.map.get(code) else {
            return;
        };
        // 编码覆盖整段输入；span 由翻译器在推入前统一设置，
        // 这里先用一个占位（长度等于编码单元数，翻译器会改写）。
        let span = Span::new(0, code.len());
        for e in entries {
            out.push(Candidate {
                text: e.text.clone(),
                comment: e.comment.clone(),
                score: e.score,
                origin: Origin::SystemWord,
                attr: SpellingAttr::NORMAL,
                span,
                lane: stele_core::Lane::Input,
            });
        }
    }

    /// 内存词库**支持**前缀查询——这也是 `BTreeMap` 而非 `HashMap` 的兑现点
    /// 之一（另一个是"同码词条顺序确定"，见类型文档）。
    fn supports_prefix(&self) -> bool {
        true
    }

    /// **补全：前缀区间扫描**。
    ///
    /// # 为什么 `BTreeMap` 恰好能做这件事
    ///
    /// `BTreeMap` 的键是**有序**的（`Vec<CodeUnitId>` 按字典序）。
    /// 而"以 `P` 为前缀"的键在这样的序里**必然是连续的一段**：
    /// 任何以 `P` 开头的键都落在 `P` 与 `P` 的下一个"更大前缀"之间。
    /// 于是 `range` 两次定位就能圈出这一段，**不需要遍历整张表**。
    ///
    /// 这正是"编码已排序 ⇒ 前缀是连续区间"这句话的可执行版本。
    ///
    /// # 复杂度
    ///
    /// 定位是 `O(log n)`；被扫的是**命中区间的大小**，与表的总大小无关。
    /// 一个两单元的前缀在 50 万词条的表里通常命中几十条。
    fn prefix_lookup(
        &self,
        prefix: &[CodeUnitId],
        exclude_exact: bool,
        out: &mut CandidateSink<'_>,
    ) {
        if prefix.is_empty() {
            return;
        }
        // 区间的右界：把前缀的最后一个单元加一 —— 于是区间恰好覆盖
        // "以 prefix 开头、且比 prefix 本身更长"的全部键。
        // 没有"加一"时（已是最大编号），直接扫到表尾。
        let mut upper: Vec<CodeUnitId> = prefix.to_vec();
        let last = upper.len() - 1;
        let bumped = upper[last].0.checked_add(1).map(CodeUnitId);
        if let Some(b) = bumped {
            upper[last] = b;
        }
        let range = match bumped {
            Some(_) => self.map.range(prefix.to_vec()..upper),
            None => self.map.range(prefix.to_vec()..),
        };
        let span = Span::new(0, prefix.len());
        for (code, entries) in range {
            if exclude_exact && code.as_slice() == prefix {
                continue;
            }
            for e in entries {
                out.push(Candidate {
                    text: e.text.clone(),
                    comment: e.comment.clone(),
                    score: e.score,
                    origin: Origin::SystemWord,
                    attr: SpellingAttr::COMPLETION,
                    span,
                    lane: stele_core::Lane::Input,
                });
            }
        }
    }
}

/// **按文本查编码**的索引（反查用）。
///
/// # 为什么它与 [`InMemoryLexicon`] 是两个类型
///
/// 方向相反：词库是"编码 → 词"，反查是"词 → 编码"。
/// 把两个方向塞进一个类型会让每个实现都要维护两份索引，
/// 而反查是**少数方案才要**的能力（见 `filter::ReverseLexicon`）。
///
/// 本类型现在还只是个空壳：它的数据要由**装载体**填（词库装载时
/// 顺手建一份倒排表）。P3 只把接口与调用点接好，
/// 真正的填充与内存计量放在反查数据真的入库时——
/// **不假装它能查到东西**：`is_empty()` 会如实回答 `true`。
#[derive(Debug, Default)]
pub struct TextIndex {
    /// 文本 → 编码序列（编码单元的**字面写法**，便于直接显示）。
    map: BTreeMap<String, Vec<Vec<String>>>,
}

impl TextIndex {
    /// 由"文本 → 编码"的条目构造。
    #[must_use]
    pub fn from_entries<S: AsRef<str>>(entries: &[(S, Vec<String>)]) -> Self {
        let mut map: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        for (text, code) in entries {
            map.entry(text.as_ref().to_owned())
                .or_default()
                .push(code.clone());
        }
        Self { map }
    }

    /// 索引里有多少个不同的文本。
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl crate::filter::ReverseLexicon for TextIndex {
    fn lookup_text(&self, text: &str, out: &mut Vec<Vec<String>>) {
        if let Some(v) = self.map.get(text) {
            out.extend(v.iter().cloned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alphabet(units: &[&str]) -> CodeAlphabet {
        CodeAlphabet::new(units.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn looks_up_an_exact_code() {
        let lex = InMemoryLexicon::from_entries(
            alphabet(&["ni", "hao"]),
            &[(vec!["ni", "hao"], "你好", 100.0)],
        )
        .unwrap();

        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        lex.lookup(&[CodeUnitId(0), CodeUnitId(1)], &mut sink);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].text, "你好");
        assert_eq!(buf[0].origin, Origin::SystemWord);
    }

    #[test]
    fn unknown_unit_is_a_load_error_not_a_miss() {
        // 字母表与词库不一致必须在**装载期**响亮报错，
        // 而不是让那个词永远查不到（那种 bug 极难排查）。
        let err =
            InMemoryLexicon::from_entries(alphabet(&["ni"]), &[(vec!["ni", "hao"], "你好", 1.0)])
                .unwrap_err();
        assert!(matches!(err, LexiconError::UnknownUnit { .. }));
    }

    #[test]
    fn same_code_entries_are_ordered_deterministically() {
        let lex = InMemoryLexicon::from_entries(
            alphabet(&["a"]),
            &[
                (vec!["a"], "低", 1.0),
                (vec!["a"], "高", 100.0),
                (vec!["a"], "中", 50.0),
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        lex.lookup(&[CodeUnitId(0)], &mut sink);
        let texts: Vec<&str> = buf.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["高", "中", "低"]);
    }

    #[test]
    fn missing_code_returns_nothing() {
        let lex =
            InMemoryLexicon::from_entries(alphabet(&["a"]), &[(vec!["a"], "甲", 1.0)]).unwrap();
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        lex.lookup(&[CodeUnitId(99)], &mut sink);
        assert!(buf.is_empty());
    }
}
