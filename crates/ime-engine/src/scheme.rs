//! 输入方案：把"用户敲的一串键"翻译成"要查哪些拼音码"。
//!
//! 一个输入法方案要回答三个问题，[`Scheme`] trait 就是这三个：
//!
//! 1. 哪些键算组词（[`Scheme::accepts`]）
//! 2. 这些键对应哪些**完整的码**（查库用）
//! 3. 要不要给这些码的查询结果重新排序
//!
//! 现在有三套：
//!
//! | 方案 | 键 → 码 | 说明 |
//! | --- | --- | --- |
//! | [`FullPinyin`] | `nihao` → `ni hao` | 全拼，靠 [`crate::pinyin::Segmenter`] 切音节 |
//! | [`DoublePinyin`] | `nihc` → `ni hao` | 双拼，两键一音节，解出来还是全拼 |
//! | [`Table`] | `wqiy` → `wqiy` | 码表（五笔这类），键本身就是码 |
//!
//! 加新方案（比如郑码、仓颉、注音）就是实现这个 trait，别的都不用动 ——
//! 词库那张表本来就有 `scheme` 字段，一套方案一个名字。

use std::collections::HashMap;

use crate::dict::Dict;
use crate::pinyin::Segmenter;

/// 输入方案
pub trait Scheme {
    /// 查库用的名字（`word.scheme` 字段）
    fn name(&self) -> &str;

    /// 这个字符能进组词 buffer 吗（一般都只收小写字母；微软双拼还要收 `;`）
    fn accepts(&self, ch: char) -> bool {
        ch.is_ascii_lowercase()
    }

    /// 输入 → 候选词，按权重从高到低。
    /// 返回空表示"这不是个有效的输入"，输入法会把预编辑晾在那儿
    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<String>;
}

/// 把好几个码的查询结果并起来：按分数排序、去重。
///
/// 为什么要并：一次输入可能对应好几个码 —— `xian` 既是「先」(xian) 也是「西安」(xi an)；
/// `nih` 的尾巴 `h` 要补成 ha/hai/…/hao。每个码单独查都是一次精确命中（快），
/// 但合起来才对用户有意义，而且必须按分数重排，不能让「妮好」压过「你好」
fn merge(dict: &Dict, scheme: &str, codes: &[String], limit: usize) -> Vec<String> {
    // 每个码多取一点，合并去重之后才够 limit 个
    let per_code = limit.max(4);
    let mut scored: Vec<(String, i64)> = Vec::new();
    for code in codes {
        scored.extend(dict.exact(scheme, code, per_code));
    }
    scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));

    let mut out: Vec<String> = Vec::with_capacity(limit);
    for (text, _) in scored {
        if !out.contains(&text) {
            out.push(text);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

// ---- 全拼 -------------------------------------------------------------------

/// 全拼：`nihao` → 切词 → 查 `ni hao`
pub struct FullPinyin {
    segmenter: Segmenter,
}

impl FullPinyin {
    pub fn new(syllables: &[String]) -> Self {
        Self {
            segmenter: Segmenter::new(syllables),
        }
    }
}

impl Scheme for FullPinyin {
    fn name(&self) -> &str {
        "pinyin"
    }

    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<String> {
        merge(
            dict,
            self.name(),
            &self.segmenter.lookup_codes(input),
            limit,
        )
    }
}

// ---- 双拼 -------------------------------------------------------------------

/// 双拼键位。双拼各家的差别只在韵母摆在哪个键上，声母除了 zh/ch/sh 都还是自己
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// 韵母（按长度从长到短匹配）→ 键
    pub finals: &'static [(&'static str, char)],
    pub zh: char,
    pub ch: char,
    pub sh: char,
}

/// 自然码（也是搜狗、QQ 拼音的默认方案）
pub const NATURAL: Layout = Layout {
    finals: &[
        ("iang", 'd'),
        ("uang", 'd'),
        ("iong", 's'),
        ("ing", 'y'),
        ("uan", 'r'),
        ("van", 'r'),
        ("iao", 'c'),
        ("ian", 'm'),
        ("ang", 'h'),
        ("eng", 'g'),
        ("ong", 's'),
        ("uai", 'y'),
        ("iu", 'q'),
        ("ia", 'w'),
        ("ua", 'w'),
        ("ve", 't'),
        ("ue", 't'),
        ("uo", 'o'),
        ("un", 'p'),
        ("vn", 'p'),
        ("en", 'f'),
        ("an", 'j'),
        ("ao", 'k'),
        ("ai", 'l'),
        ("ei", 'z'),
        ("ie", 'x'),
        ("ui", 'v'),
        ("ou", 'b'),
        ("in", 'n'),
    ],
    zh: 'v',
    ch: 'i',
    sh: 'u',
};

/// 小鹤双拼
pub const FLYPY: Layout = Layout {
    finals: &[
        ("iang", 'l'),
        ("uang", 'l'),
        ("iong", 's'),
        ("uai", 'k'),
        ("ing", 'k'),
        ("uan", 'r'),
        ("iao", 'n'),
        ("ian", 'm'),
        ("ang", 'h'),
        ("eng", 'g'),
        ("ong", 's'),
        ("iu", 'q'),
        ("ei", 'w'),
        ("ie", 'p'),
        ("ue", 't'),
        ("ve", 't'),
        ("uo", 'o'),
        ("un", 'y'),
        ("en", 'f'),
        ("an", 'j'),
        ("ou", 'z'),
        ("ia", 'x'),
        ("ua", 'x'),
        ("ao", 'c'),
        ("ai", 'd'),
        ("ui", 'v'),
        ("in", 'b'),
    ],
    zh: 'v',
    ch: 'i',
    sh: 'u',
};

/// 微软双拼（`ing` 在分号键上，所以它比别的方案多收一个键）
pub const MSPY: Layout = Layout {
    finals: &[
        ("iang", 'd'),
        ("uang", 'd'),
        ("iong", 's'),
        ("uai", 'y'),
        ("uan", 'r'),
        ("van", 'r'),
        ("iao", 'c'),
        ("ian", 'm'),
        ("ang", 'h'),
        ("eng", 'g'),
        ("ong", 's'),
        ("er", 'r'),
        ("iu", 'q'),
        ("ia", 'w'),
        ("ua", 'w'),
        ("ve", 't'),
        ("ue", 't'),
        ("uo", 'o'),
        ("un", 'p'),
        ("vn", 'p'),
        ("en", 'f'),
        ("an", 'j'),
        ("ao", 'k'),
        ("ai", 'l'),
        ("ei", 'z'),
        ("ie", 'x'),
        ("ui", 'v'),
        ("ou", 'b'),
        ("in", 'n'),
        ("ing", ';'),
        ("v", 'y'),
    ],
    zh: 'v',
    ch: 'i',
    sh: 'u',
};

impl Layout {
    /// 按名字取预设
    pub fn preset(name: &str) -> Option<Layout> {
        match name {
            "natural" | "自然码" => Some(NATURAL),
            "flypy" | "小鹤" => Some(FLYPY),
            "mspy" | "微软" => Some(MSPY),
            _ => None,
        }
    }

    /// 一个完整音节 → 两个键。
    ///
    /// 规则是照 Rime 的双拼方案文件（`rime-double-pinyin`）推的：
    ///
    /// 1. 声母 `zh/ch/sh` 换成 `v/i/u`，别的声母就是自己
    /// 2. 韵母按**最长匹配**换成一个键（`zhang` 要匹配 `ang` 而不是 `ng`）
    /// 3. 零声母（a/o/e 开头）要把首字母抄一遍再压：`ai` → `aai` → `al`，
    ///    `ang` → `aang` → `ah`；抄完压不成两键的（`er`）就不抄
    pub fn encode(&self, syllable: &str) -> Option<String> {
        let mut code = syllable.to_string();

        if let Some(rest) = code.strip_prefix("zh") {
            code = format!("{}{rest}", self.zh);
        } else if let Some(rest) = code.strip_prefix("ch") {
            code = format!("{}{rest}", self.ch);
        } else if let Some(rest) = code.strip_prefix("sh") {
            code = format!("{}{rest}", self.sh);
        } else if code.starts_with(['a', 'o', 'e']) {
            // 零声母：抄一遍首字母，再走下面的韵母规则
            let doubled = format!("{}{}", &code[..1], code);
            let compressed = self.compress(&doubled);
            return match compressed {
                Some(code) => Some(code),
                // 抄了反而压不成两键（er 这种没有韵母规则的）→ 就用原样
                None => self.compress(&code),
            };
        }

        self.compress(&code)
    }

    /// 把韵母换成键，要求结果正好两键
    fn compress(&self, code: &str) -> Option<String> {
        // 最长的韵母优先：`zhang` 要匹配 `ang`，不能匹配成 `ng`
        let mut best: Option<&(&str, char)> = None;
        for entry in self.finals {
            let (finals, _) = entry;
            if code.len() > finals.len()
                && code.ends_with(finals)
                && best.is_none_or(|(best_finals, _)| finals.len() > best_finals.len())
            {
                best = Some(entry);
            }
        }
        let code = match best {
            Some((finals, key)) => format!("{}{}", &code[..code.len() - finals.len()], key),
            None => code.to_string(),
        };
        (code.chars().count() == 2).then_some(code)
    }
}

/// 双拼：输入是一串两键一组的东西，每组解成一个音节，解出来就变成全拼
pub struct DoublePinyin {
    /// 两键码 → 音节
    by_code: HashMap<String, String>,
    /// 首键 → 以它开头的所有音节（最后一组还没打完的时候补全用）
    by_first: HashMap<char, Vec<String>>,
    /// 这套键位用到分号吗（微软双拼的 ing 在分号上）
    semicolon: bool,
}

impl DoublePinyin {
    pub fn new(layout: Layout, syllables: &[String]) -> Self {
        let mut by_code = HashMap::with_capacity(syllables.len() * 2);
        let mut by_first: HashMap<char, Vec<String>> = HashMap::new();
        let mut semicolon = false;
        for syllable in syllables {
            let Some(code) = layout.encode(syllable) else {
                continue; // 这套键位没给这个音节安排位置
            };
            semicolon |= code.contains(';');
            by_first
                .entry(code.chars().next().unwrap())
                .or_default()
                .push(syllable.clone());
            by_code.insert(code, syllable.clone());
        }
        for list in by_first.values_mut() {
            list.sort();
            list.dedup();
        }
        Self {
            by_code,
            by_first,
            semicolon,
        }
    }

    /// 两键码 → 音节（`ni` → `ni`，`hc` → `hao`）
    pub fn decode_syllable(&self, code: &str) -> Option<&str> {
        self.by_code.get(code).map(String::as_str)
    }

    /// 输入 → 要查的拼音码
    fn lookup_codes(&self, input: &str) -> Vec<String> {
        let chars: Vec<char> = input.chars().collect();
        let mut complete: Vec<String> = Vec::new();

        // 两个一组往下解
        let mut index = 0;
        while index + 1 < chars.len() {
            let pair: String = chars[index..index + 2].iter().collect();
            match self.decode_syllable(&pair) {
                Some(syllable) => {
                    complete.push(syllable.to_string());
                    index += 2;
                }
                None => return Vec::new(), // 这组键解不出音节，整个输入作废
            }
        }

        if index == chars.len() {
            // 正好解完，不再往后联想（跟全拼一个规矩：打 ni 就只查 ni）
            return vec![complete.join(" ")];
        }

        // 剩一个键：把它当作某个音节的第一个键，枚举可能的音节
        let first = chars[index];
        let mut codes = Vec::new();
        for syllable in self.by_first.get(&first).into_iter().flatten() {
            let mut code = complete.join(" ");
            if !code.is_empty() {
                code.push(' ');
            }
            code.push_str(syllable);
            codes.push(code);
            if codes.len() >= crate::pinyin::MAX_CODES {
                break;
            }
        }
        codes
    }
}

impl Scheme for DoublePinyin {
    fn name(&self) -> &str {
        "pinyin"
    }

    fn accepts(&self, ch: char) -> bool {
        ch.is_ascii_lowercase() || (self.semicolon && ch == ';')
    }

    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<String> {
        merge(dict, self.name(), &self.lookup_codes(input), limit)
    }
}

// ---- 码表方案（五笔这类）-----------------------------------------------------

/// 码表方案：键本身就是码，直接拿去查（五笔、郑码、仓颉……都是这个形状）。
///
/// 词库那张表有 `scheme` 字段，所以一套码表就是一批 `scheme = 'wubi'` 的行。
/// 导入用 `ime-dict --table 码表.txt --table-scheme wubi`。
///
/// 注意：码表查询是**前缀**查询（打 `w` 要能出所有以 w 开头的字），
/// 这是唯一会扫一大片的查询，码表很大时会慢。真要用起来得在导入时按前缀
/// 预先算好 top-N —— 见 README 的"已知不足"
pub struct Table {
    name: String,
}

impl Table {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl Scheme for Table {
    fn name(&self) -> &str {
        &self.name
    }

    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<String> {
        // 先精确后前缀：正好打完一个码的时候，它得排在最前面
        let mut out: Vec<String> = Vec::new();
        for (text, _) in dict.exact(&self.name, input, limit) {
            out.push(text);
        }
        for (text, _) in dict.prefix(&self.name, input, limit) {
            if !out.contains(&text) {
                out.push(text);
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 词库里的 412 个音节（测试里用一份完整的，别依赖真库）
    fn syllables() -> Vec<String> {
        crate::pinyin::SYLLABLES
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn 自然码键位() {
        let layout = NATURAL;
        // 零声母：抄首字母再压韵母
        for (syllable, want) in [
            ("ni", "ni"),
            ("hao", "hk"),
            ("zhang", "vh"),
            ("ang", "ah"),
            ("a", "aa"),
            ("ai", "al"),
            ("en", "ef"),
            ("xi", "xi"),
            ("xian", "xm"),
            ("zhuang", "vd"),
            ("juan", "jr"),
            ("jun", "jp"),
            ("lve", "lt"),
        ] {
            assert_eq!(layout.encode(syllable).as_deref(), Some(want), "{syllable}");
        }
    }

    #[test]
    fn 小鹤和微软的韵母键不一样() {
        // 小鹤：ao→c, ei→w, in→b；微软：ing 在分号上
        assert_eq!(FLYPY.encode("hao").as_deref(), Some("hc"));
        assert_eq!(FLYPY.encode("bei").as_deref(), Some("bw"));
        assert_eq!(FLYPY.encode("xin").as_deref(), Some("xb"));
        assert_eq!(MSPY.encode("xing").as_deref(), Some("x;"));
    }

    #[test]
    fn 每个音节都能编成两键() {
        // 双拼是"两键一音节"，`m`/`n`/`ng`（呣/嗯/唔）这种单键感叹词塞不进这个模型 ——
        // 词表里有，但双拼方案一般打 `en`（恩）。除了它们，别的音节都要能编出来
        const EXPECTED_MISSING: &[&str] = &["m", "n", "ng", "hm", "hng", "ê"];
        for layout in [NATURAL, FLYPY, MSPY] {
            let mut missing = Vec::new();
            for syllable in syllables() {
                match layout.encode(&syllable) {
                    Some(code) => assert_eq!(code.chars().count(), 2, "{syllable} → {code}"),
                    None => missing.push(syllable),
                }
            }
            missing.retain(|syllable| !EXPECTED_MISSING.contains(&syllable.as_str()));
            assert!(missing.is_empty(), "{layout:?} 编不出这些音节：{missing:?}");
        }
    }

    #[test]
    fn 双拼打你好() {
        let double = DoublePinyin::new(NATURAL, &syllables());
        // 自然码：ni = n+i，hao = h+k
        assert_eq!(double.lookup_codes("nihk"), ["ni hao"]);
        // 小鹤：hao = h+c
        let flypy = DoublePinyin::new(FLYPY, &syllables());
        assert_eq!(flypy.lookup_codes("nihc"), ["ni hao"]);
    }

    #[test]
    fn 双拼的半截音节也补全() {
        let double = DoublePinyin::new(NATURAL, &syllables());
        // 只打了 h：所有 h 开头的音节都该试一遍，包括 hao
        let codes = double.lookup_codes("nih");
        assert!(codes.contains(&"ni hao".to_string()), "{codes:?}");
    }

    #[test]
    fn 微软双拼收分号() {
        let mspy = DoublePinyin::new(MSPY, &syllables());
        assert!(mspy.accepts(';'));
        assert!(mspy.accepts('z'));
        let natural = DoublePinyin::new(NATURAL, &syllables());
        assert!(!natural.accepts(';'));
    }
}
