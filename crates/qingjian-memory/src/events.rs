//! # Events — 引擎事件到记忆操作的**唯一**映射点
//!
//! 中文职责：把 `Session::drain_events` 给出的 `Event::Learned` /
//! `Event::ForgetRequested` 变成 `MemoryStore::record` / `forget`。
//! English role: the single mapping from engine events to memory operations.
//! 架构位置：`qingjian-memory` 的消费者一侧；前端（CLI / TSF / Android）
//! 每个按键之后调用一次。
//!
//! # 为什么它是一条**共享函数**，而不是各前端各写一段 `match`
//!
//! HANDOFF §7.6.2 第 4 步把"忘了接 `Learned` 事件"列为这一步最容易漏的坑，
//! 而且**漏了不报错**——症状只是"学了没记住"。P3 有四个 bug 是同一种形状：
//! 接线在别处、这条路没被走到。
//!
//! 把映射收敛成一个函数之后，三个前端（CLI、将来的 TSF / Android）
//! 调的是同一段代码，而"事件 → 记忆"的语义只有一处定义。
//!
//! # 前端要做的事（三行）
//!
//! ```no_run
//! # use qingjian_core::Event;
//! # use qingjian_memory::{FileMemory, apply_events};
//! # fn demo(session_events: Vec<Event>, memory: &FileMemory) {
//! let mut events = Vec::new();
//! // ① 每个按键之后把事件取干净
//! // session.drain_events(&mut events);
//! // ② 喂给记忆
//! let handled = apply_events(memory, &events);
//! // ③ 退出时落盘（**按键路径上不许有它**）
//! let _ = handled;
//! memory.flush().ok();
//! # }
//! ```

use qingjian_core::{Commit, Event, MemoryStore, Trigger};

/// 把一批事件喂给记忆，返回处理了几条。
///
/// # 为什么它接受 `&dyn MemoryStore` 而不是 `&FileMemory`
///
/// 这样测试可以注入一个记录调用的假实现，断言"事件真的被翻译成了 record"，
/// 而不必去翻一个真实文件的字节。
///
/// # 未被消费的事件
///
/// `Event::OptionChanged` 与将来新增的变体在这里被**有意忽略**——
/// 它们与记忆无关。`Event` 是 `#[non_exhaustive]`，所以这里必须有一个
/// `_` 分支；它的存在是 Rust 逼我们写下"我知道还会有别的变体"。
pub fn apply_events(memory: &dyn MemoryStore, events: &[Event]) -> usize {
    let mut handled = 0;
    for e in events {
        match e {
            Event::Learned {
                input,
                text,
                origin,
                attr,
                lane,
                context,
                key,
            } => {
                // `Learned` 已经带齐了学习需要的一切（**规范编码键**、文本、
                // 来源，以及预测要用的**上下文**），只是形状与 `Commit` 不同。
                // 这里补上 `Commit` 里与学习无关的那个字段，把语义收敛到
                // `MemoryStore::record` 一处。
                memory.record(&Commit {
                    text: text.clone(),
                    input: input.clone(),
                    context: context.clone(),
                    origin: *origin,
                    attr: *attr,
                    lane: *lane,
                    trigger: Trigger::Space,
                    key: key.clone(),
                });
                handled += 1;
            }
            Event::ForgetRequested { input, text, key } => {
                // `forget` 要的是**与 record 同一把键**——否则会取消到
                // 另一条记录上（`nhao` 记的、被 `nihao` 取消）。
                memory.forget(key.as_deref().unwrap_or(input), text);
                handled += 1;
            }
            _ => {}
        }
    }
    handled
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingjian_core::{Context, Lane, MemoryEntry, Origin, Prediction, SpellingAttr};
    use std::sync::Mutex;

    /// 一个把调用记下来的假记忆——用它断言"事件真的被翻译成了操作"。
    #[derive(Default)]
    struct Recorder {
        /// `(键, 词, 上下文)`。
        recorded: Mutex<Vec<(String, String, Vec<String>)>>,
        forgotten: Mutex<Vec<(String, String)>>,
    }

    impl MemoryStore for Recorder {
        fn record(&self, commit: &Commit) {
            // 记的是**主键**：有编码键就用它，没有才退回拼写——
            // 与 `FileMemory::record` 的规则一致，否则这条测试测不到真东西。
            let key = commit.key.as_deref().unwrap_or(&commit.input).to_owned();
            self.recorded
                .lock()
                .unwrap()
                .push((key, commit.text.clone(), commit.context.clone()));
        }
        fn lookup(&self, _input: &str) -> Vec<MemoryEntry> {
            Vec::new()
        }
        fn forget(&self, key: &str, text: &str) {
            self.forgotten
                .lock()
                .unwrap()
                .push((key.to_owned(), text.to_owned()));
        }
        fn predict_next(&self, _context: &Context) -> Vec<Prediction> {
            Vec::new()
        }
    }

    #[test]
    fn learned_and_forget_events_become_the_right_calls() {
        let rec = Recorder::default();
        let events = vec![
            Event::Learned {
                input: "nhao".into(),
                text: "你好".into(),
                origin: Origin::SystemWord,
                attr: SpellingAttr::ABBREV,
                lane: Lane::Input,
                context: vec!["微信".into()],
                key: Some("ni'hao".into()),
            },
            Event::ForgetRequested {
                input: "nhao".into(),
                text: "你号".into(),
                key: Some("ni'hao".into()),
            },
            Event::OptionChanged {
                name: "emoji".into(),
                on: true,
            },
        ];
        assert_eq!(
            apply_events(&rec, &events),
            2,
            "只该处理两条与记忆有关的事件"
        );
        assert_eq!(
            rec.recorded.lock().unwrap().as_slice(),
            &[(
                "ni'hao".to_owned(),
                "你好".to_owned(),
                vec!["微信".to_owned()]
            )],
            "记录的必须是**规范编码键**，且上下文要一起带过去"
        );
        assert_eq!(
            rec.forgotten.lock().unwrap().as_slice(),
            &[("ni'hao".to_owned(), "你号".to_owned())],
            "取消学习要用同一把键"
        );
    }

    #[test]
    fn a_prediction_event_carries_its_context_to_the_store() {
        // P4b 的关键接线：`Lane::Predict` 的学习键是**上下文**。
        // 事件里少了它，`record` 拿到的是一份空上下文的 `Commit`，
        // 于是预测学习**静默地什么都不记**（HANDOFF §7.7.4 第 4 条）。
        let rec = Recorder::default();
        let events = vec![Event::Learned {
            input: String::new(),
            text: "朋友圈".into(),
            origin: Origin::Prediction,
            attr: SpellingAttr::NORMAL,
            lane: Lane::Predict,
            context: vec!["微信".into()],
            key: None,
        }];
        assert_eq!(apply_events(&rec, &events), 1);
        let recorded = rec.recorded.lock().unwrap();
        assert_eq!(recorded[0].1, "朋友圈");
        assert_eq!(
            recorded[0].2,
            vec!["微信".to_owned()],
            "预测的键是上下文——它必须活着穿过事件层"
        );
    }

    #[test]
    fn no_events_is_a_no_op() {
        let rec = Recorder::default();
        assert_eq!(apply_events(&rec, &[]), 0);
        assert!(rec.recorded.lock().unwrap().is_empty());
    }
}
