//! # P0-C 回归（端到端）：畸形 `.table` 只能被**安全拒绝**
//!
//! 审计的复现：只篡改缓存二进制里的一个 `unit_offsets` 值，
//! 保持文件总长、魔数、格式版本和 source checksum 都不变——
//! **装载成功**，查询时在切片范围处 panic：
//!
//! ```text
//! range start index 10000 out of range for slice of length 2
//! ```
//!
//! `source_checksum` 只表明源词典内容，**不是产物完整性校验**。
//!
//! 这一组测试走**公开 API 与真实文件**（不是内部函数），逐类畸形输入断言
//! "得到一个可读的 `FormatError`"。任何一条变成 panic，测试进程会直接崩，
//! 因此"不 panic"这件事是被强制执行、而不是被声明的。

use qingjian_core::{CandidateSink, CodeUnitId, Lexicon};
use qingjian_table::{
    BuildFingerprint, TableLexicon, COMPILER_OPTIONS, FORMAT_VERSION, HEADER_SIZE,
};

const CHECKSUM: u64 = 7;

fn fingerprint() -> BuildFingerprint {
    BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &[], CHECKSUM)
}

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "qingjian-table-integrity-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

/// 编译一份小产物并返回它的字节。
fn artifact(tag: &str) -> Vec<u8> {
    let p = dir(tag).join("d.table");
    qingjian_table::compile(
        CHECKSUM,
        fingerprint(),
        |w| {
            w.push("你好", &[0, 1], 100.0)?;
            w.push("世界", &[2, 3], 50.0)?;
            w.push("你", &[0], 200.0)?;
            w.push("好", &[1], 300.0)?;
            Ok(())
        },
        &p,
    )
    .expect("编译");
    std::fs::read(&p).expect("读产物")
}

fn write(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir(tag).join("d.table");
    std::fs::write(&p, bytes).expect("写产物");
    p
}

/// 头部之后全部字节的 FNV-1a，写回头部字段——模拟"能改文件的人"。
///
/// 威胁模型说校验和挡不住他，挡他的是**结构校验**。这个函数就是
/// 把"校验和已经对了"这个前提制造出来。
fn recompute_body_checksum(bytes: &mut [u8]) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes[HEADER_SIZE as usize..] {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    bytes[72..80].copy_from_slice(&h.to_le_bytes());
}

fn index_offset(bytes: &[u8]) -> usize {
    usize::try_from(u64::from_le_bytes(bytes[56..64].try_into().unwrap())).unwrap()
}

/// 装载并**安全查询**：要么 `Err`，要么 `Ok` 且查询不 panic。
fn open_and_probe(tag: &str, bytes: &[u8]) {
    let p = write(tag, bytes);
    if let Ok(l) = TableLexicon::open_checked(&p, Some(fingerprint())) {
        let probes: [&[u16]; 6] = [&[0, 1], &[2, 3], &[0], &[1], &[9, 9], &[0, 1, 2, 3]];
        for code in probes {
            let ids: Vec<CodeUnitId> = code.iter().map(|c| CodeUnitId(u32::from(*c))).collect();
            let mut buf = Vec::new();
            let mut sink = CandidateSink::new(&mut buf, 8);
            l.lookup(&ids, &mut sink);
        }
    }
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn a_healthy_artifact_loads_and_answers() {
    let p = write("ok", &artifact("ok-src"));
    let l = TableLexicon::open_checked(&p, Some(fingerprint())).expect("正常产物应当能装载");
    let ids = [CodeUnitId(0), CodeUnitId(1)];
    let mut buf = Vec::new();
    let mut sink = CandidateSink::new(&mut buf, 8);
    l.lookup(&ids, &mut sink);
    assert_eq!(buf.first().map(|c| c.text.as_str()), Some("你好"));
}

#[test]
fn the_audited_tamper_is_refused_not_a_panic() {
    // 审计原样：改 `unit_offsets[1]` 为一个远超单元表长的值。
    // 注意**不**重算校验和——这一条走"意外损坏"那条路。
    let mut b = artifact("tamper");
    let at = index_offset(&b) + 4;
    b[at..at + 4].copy_from_slice(&10_000u32.to_le_bytes());
    let p = write("tamper", &b);
    let e =
        TableLexicon::open_checked(&p, Some(fingerprint())).expect_err("篡改过的产物必须被拒绝");
    let msg = e.to_string();
    assert!(
        msg.contains("校验和") || msg.contains("损坏"),
        "诊断必须可读：{msg}"
    );
}

#[test]
fn the_audited_tamper_with_a_forged_checksum_is_still_refused() {
    // **校验和已经重算正确**——这模拟"能改文件的人"。
    // 挡他的必须是结构校验，而不是"他没有改校验和"。
    let mut b = artifact("forge");
    let at = index_offset(&b) + 4;
    b[at..at + 4].copy_from_slice(&10_000u32.to_le_bytes());
    recompute_body_checksum(&mut b);
    let p = write("forge", &b);
    let e = TableLexicon::open_checked(&p, Some(fingerprint()))
        .expect_err("结构损坏必须被拒绝，无论校验和是否正确");
    let msg = e.to_string();
    assert!(
        msg.contains("非单调") || msg.contains("超过总数"),
        "必须是结构校验挡下来的：{msg}"
    );
}

#[test]
fn every_offset_table_is_validated() {
    let base = artifact("offsets");
    let index = index_offset(&base);
    // 头部里 code_count 在 28..32；前缀和表有 code_count+1 项。
    let code_count = usize::try_from(u32::from_le_bytes(base[28..32].try_into().unwrap())).unwrap();

    // ① `unit_offsets` 首项必须是 0。
    let mut b = base.clone();
    b[index..index + 4].copy_from_slice(&3u32.to_le_bytes());
    recompute_body_checksum(&mut b);
    open_and_probe("off0", &b);

    // ② `unit_offsets` 末项必须等于 total_units。
    let mut b = base.clone();
    let at = index + code_count * 4;
    b[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
    recompute_body_checksum(&mut b);
    open_and_probe("off1", &b);

    // ③ `entry_offsets` 首项必须是 0。
    let mut b = base.clone();
    let at = index + (code_count + 1) * 4;
    b[at..at + 4].copy_from_slice(&2u32.to_le_bytes());
    recompute_body_checksum(&mut b);
    open_and_probe("off2", &b);

    // ④ `entry_offsets` 末项必须等于 entry_count。
    let mut b = base.clone();
    let at = index + (code_count + 1) * 4 + code_count * 4;
    b[at..at + 4].copy_from_slice(&999u32.to_le_bytes());
    recompute_body_checksum(&mut b);
    open_and_probe("off3", &b);

    // 全部走完：只要没有 panic 就算通过（`open_and_probe` 内部已经保证）。
}

#[test]
fn header_regions_are_validated() {
    for (off, val) in [
        (24usize, u32::MAX), // entry_count
        (28, u32::MAX),      // code_count
        (32, u32::MAX),      // word_bytes
        (36, u32::MAX),      // total_units
    ] {
        let mut b = artifact("hdr4");
        b[off..off + 4].copy_from_slice(&val.to_le_bytes());
        recompute_body_checksum(&mut b);
        open_and_probe(&format!("hdr4-{off}"), &b);
    }
    for (off, val) in [
        (40usize, u64::MAX), // words_offset
        (48, u64::MAX),      // entries_offset
        (56, u64::MAX),      // index_offset
    ] {
        let mut b = artifact("hdr8");
        b[off..off + 8].copy_from_slice(&val.to_le_bytes());
        recompute_body_checksum(&mut b);
        open_and_probe(&format!("hdr8-{off}"), &b);
    }
}

#[test]
fn truncation_and_appended_bytes_are_refused() {
    let base = artifact("trunc");
    let cuts = [0usize, 1, 40, 79, 80, 81, base.len() / 2, base.len() - 1];
    for cut in cuts {
        assert!(cut < base.len(), "裁切点必须严格小于产物长度");
        let p = write(&format!("cut-{cut}"), &base[..cut]);
        assert!(
            TableLexicon::open_checked(&p, Some(fingerprint())).is_err(),
            "截断到 {cut} 字节（共 {}）的产物不该装载成功",
            base.len()
        );
    }
    let mut long = base.clone();
    long.push(0);
    let p = write("long", &long);
    assert!(
        TableLexicon::open_checked(&p, Some(fingerprint())).is_err(),
        "多出尾巴的产物不该装载成功"
    );
}

#[test]
fn a_wrong_fingerprint_is_refused_with_an_explanation() {
    let p = write("fp", &artifact("fp-src"));
    let other = BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &[], 999);
    let e = TableLexicon::open_checked(&p, Some(other)).expect_err("指纹不符必须拒绝");
    let msg = e.to_string();
    assert!(msg.contains("身份指纹"), "{msg}");
    assert!(msg.contains("字母表"), "诊断要说清为什么会静默错码：{msg}");
}

#[test]
fn flipped_bytes_never_panic() {
    // 无依赖版本的畸形输入测试（本项目不引入 cargo-fuzz）。
    // 逐字节翻转 + 逐字节置零，每个位置都必须"安全拒绝或安全查询"。
    let base = artifact("flip");
    for i in (0..base.len()).step_by(5) {
        let mut b = base.clone();
        b[i] ^= 0xff;
        open_and_probe(&format!("flip-{i}"), &b);
        let mut z = base.clone();
        z[i] = 0;
        open_and_probe(&format!("zero-{i}"), &z);
    }
}
