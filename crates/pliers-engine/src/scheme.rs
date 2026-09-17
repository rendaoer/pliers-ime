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

use std::collections::{BTreeMap, HashMap};

use crate::Candidate;
use crate::dict::Dict;
use crate::pinyin::Segmenter;
use crate::sentence;

/// 输入方案
pub trait Scheme {
    /// 查库用的名字（`word.scheme` 字段）
    fn name(&self) -> &str;

    /// 这个字符能进组词 buffer 吗（一般都只收小写字母；微软双拼还要收 `;`）
    fn accepts(&self, ch: char) -> bool {
        ch.is_ascii_lowercase()
    }

    /// 输入 → 候选词，按权重从高到低。
    ///
    /// 每个候选都带 `consumed`：**整串匹配**的是全部输入，
    /// **只匹配前面一段**的（`nihaoma` → 「你好」）只吃前面几个字符，
    /// 剩下的留给下一轮 —— 这就是分段上屏
    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<Candidate>;
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

/// 整串匹配的那批候选
fn whole(input: &str, words: Vec<String>) -> Vec<Candidate> {
    words
        .into_iter()
        .map(|text| Candidate::whole(text, input))
        .collect()
}

/// 把整句候选接到精确候选后面（去重、不超 `limit`）。
///
/// 顺序是有意的：整词命中永远排在拼出来的句子前面 ——「你好」这种库里真有的词
/// 不该被「你」+「好」拼出来的同一个句子挤掉（文字一样，但前者是"查到的"）
fn push_sentences(
    out: &mut Vec<String>,
    dict: &Dict,
    scheme: &str,
    syllables: &[String],
    limit: usize,
) {
    for text in sentence::candidates(dict, scheme, syllables, limit) {
        if !out.contains(&text) {
            out.push(text);
            if out.len() >= limit {
                break;
            }
        }
    }
}

/// 分段匹配：把输入按音节边界切成"前面一段 + 剩下的一截"，
/// 前面那段照常查词（整词 + 整句），候选只吃前面那几个字符。
///
/// 从最长的前缀往短了试，**第一段有候选就收手** —— 这跟老式输入法一个思路：
/// `nihaoma` 先给「你好」相关的候选，选完 `ma` 接着来。前缀最多试 [`MAX_PREFIX_TRIES`] 个，
/// 免得长句子把查询数炸掉
fn prefix_candidates(
    dict: &Dict,
    scheme: &str,
    prefix_codes: &[(String, usize)],
    limit: usize,
) -> Vec<Candidate> {
    let per_prefix = limit.max(4);
    for (code, consumed) in prefix_codes.iter().take(MAX_PREFIX_TRIES) {
        let words = merge(dict, scheme, std::slice::from_ref(code), per_prefix);
        if !words.is_empty() {
            return words
                .into_iter()
                .map(|text| Candidate::partial(text, *consumed))
                .collect();
        }
    }
    Vec::new()
}

/// 分段匹配最多试几个前缀
const MAX_PREFIX_TRIES: usize = 3;

// ---- 全拼 -------------------------------------------------------------------

/// 全拼：`nihao` → 切词 → 查 `ni hao`
pub struct FullPinyin {
    segmenter: Segmenter,
    /// 要不要给"整句"候选（`nihaoma` → 你好吗）
    sentence: bool,
}

impl FullPinyin {
    pub fn new(syllables: &[String], sentence: bool) -> Self {
        Self {
            segmenter: Segmenter::new(syllables),
            sentence,
        }
    }
}

impl Scheme for FullPinyin {
    fn name(&self) -> &str {
        "pinyin"
    }

    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<Candidate> {
        let mut words = merge(
            dict,
            self.name(),
            &self.segmenter.lookup_codes(input),
            limit,
        );
        if self.sentence
            && let Some(syllables) = self.segmenter.complete_segmentations(input).first()
        {
            push_sentences(&mut words, dict, self.name(), syllables, limit);
        }

        let mut out = whole(input, words);
        // 整串没匹配上（或者匹配得少）时，给"只吃前面一段"的候选
        out.extend(prefix_candidates(
            dict,
            self.name(),
            &self.segmenter.prefix_codes(input),
            limit,
        ));
        out.truncate(limit);
        out
    }
}

// ---- 双拼 -------------------------------------------------------------------

/// 双拼键位。双拼各家的差别只在韵母摆在哪个键上，声母除了 zh/ch/sh 都还是自己
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// 韵母 → 键。按长度从长到短排好，匹配时取最长（`zhang` 要匹配 `ang` 不是 `ng`）
    pub finals: Vec<(String, char)>,
    pub zh: char,
    pub ch: char,
    pub sh: char,
}

/// 双拼是"两键一音节"，这几个感叹词音节没有第二个键可打（`嗯` 一般打 `en`）。
/// 校验自定义键位时要放过它们
pub const UNENCODABLE: &[&str] = &["m", "n", "ng", "hm", "hng", "ê"];

/// 零声母音节里"全拼正好两个字母"的那些：除了本方案自己的规则（安 = `aj`），
/// 也认全拼（安 = `an`）—— 见 [`DoublePinyin::new`] 里的说明
const ZERO_INITIAL_FULL: &[&str] = &["ai", "an", "ao", "ei", "en", "er", "ou"];

/// 自然码的韵母表（也是搜狗、QQ 拼音的默认方案）
const NATURAL_FINALS: &[(&str, char)] = &[
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
];

/// 小鹤双拼的韵母表
const FLYPY_FINALS: &[(&str, char)] = &[
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
];

/// 微软双拼的韵母表（`ing` 在分号键上，所以它比别的方案多收一个键）
const MSPY_FINALS: &[(&str, char)] = &[
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
];

impl Layout {
    /// 按名字取预设
    pub fn preset(name: &str) -> Option<Layout> {
        let (finals, zh, ch, sh) = match name {
            "natural" | "自然码" => (NATURAL_FINALS, 'v', 'i', 'u'),
            "flypy" | "小鹤" => (FLYPY_FINALS, 'v', 'i', 'u'),
            "mspy" | "微软" => (MSPY_FINALS, 'v', 'i', 'u'),
            _ => return None,
        };
        Some(Layout::new(finals, zh, ch, sh))
    }

    /// 空键位表：配置文件里写 `layout = "none"`，全靠 `[scheme.keys]` 自己填。
    /// 声母的默认值跟自然码一样（zh/ch/sh → v/i/u），一般也要自己写
    pub fn empty() -> Layout {
        Layout {
            finals: Vec::new(),
            zh: 'v',
            ch: 'i',
            sh: 'u',
        }
    }

    fn new(finals: &[(&str, char)], zh: char, ch: char, sh: char) -> Layout {
        let mut layout = Layout {
            finals: finals
                .iter()
                .map(|(finals, key)| ((*finals).to_string(), *key))
                .collect(),
            zh,
            ch,
            sh,
        };
        layout.sort_finals();
        layout
    }

    /// 长的韵母排前面，匹配时才能"最长优先"
    fn sort_finals(&mut self) {
        self.finals
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        self.finals.dedup_by(|a, b| a.0 == b.0);
    }

    /// 在现有键位上改几个键（配置文件里的 `[scheme.keys]`）。
    /// 键名写 `zh`/`ch`/`sh` 就是改声母，别的都当韵母
    pub fn with_keys(mut self, keys: &BTreeMap<String, String>) -> Result<Layout, String> {
        for (name, value) in keys {
            let mut chars = value.chars();
            let (Some(key), None) = (chars.next(), chars.next()) else {
                return Err(format!("键位表里 {name:?} = {value:?}：值必须正好一个字符"));
            };
            if !(key.is_ascii_lowercase() || key == ';') {
                return Err(format!(
                    "键位表里 {name:?} = {value:?}：键只能是 a-z 或分号"
                ));
            }
            match name.as_str() {
                "zh" => self.zh = key,
                "ch" => self.ch = key,
                "sh" => self.sh = key,
                _ => {
                    // 同名韵母以自定义的为准
                    self.finals.retain(|(finals, _)| finals != name);
                    self.finals.push((name.clone(), key));
                }
            }
        }
        self.sort_finals();
        Ok(self)
    }

    /// 这套键位编不出两键的音节（`m`/`n`/`ng` 那几个感叹词不算）
    pub fn unencodable(&self, syllables: &[String]) -> Vec<String> {
        syllables
            .iter()
            .filter(|syllable| {
                !UNENCODABLE.contains(&syllable.as_str()) && self.encode(syllable).is_none()
            })
            .cloned()
            .collect()
    }

    /// 编不出来的音节，缺的是哪些**韵母**。
    ///
    /// 直接把 300 个音节甩给用户没用，得告诉他"你少写了 ai、ang 这几个" ——
    /// 所以把音节去掉声母，剩下的就是那个没定义的韵母（`bai` → `ai`，`zhuang` → `uang`）
    pub fn missing_finals(&self, syllables: &[String]) -> Vec<String> {
        let mut missing: Vec<String> = self
            .unencodable(syllables)
            .iter()
            .map(|syllable| final_of(syllable).to_string())
            .collect();
        missing.sort();
        missing.dedup();
        missing
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
        let mut best: Option<&(String, char)> = None;
        for entry in &self.finals {
            let (finals, _) = entry;
            if code.len() > finals.len()
                && code.ends_with(finals.as_str())
                && best
                    .as_ref()
                    .is_none_or(|(best_finals, _)| finals.len() > best_finals.len())
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

/// 从一个音节里切出韵母：`bai` → `ai`，`zhuang` → `uang`，`ai` → `ai`
fn final_of(syllable: &str) -> &str {
    for initial in ["zh", "ch", "sh"] {
        if let Some(rest) = syllable.strip_prefix(initial) {
            return rest;
        }
    }
    match syllable.chars().next() {
        Some(first) if !"aoe".contains(first) => &syllable[first.len_utf8()..],
        _ => syllable,
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
    /// 要不要给"整句"候选（`nihcma` → 你好吗）
    sentence: bool,
}

impl DoublePinyin {
    pub fn new(layout: Layout, syllables: &[String], sentence: bool) -> Self {
        let mut by_code = HashMap::with_capacity(syllables.len() * 2);
        let mut by_first: HashMap<char, Vec<String>> = HashMap::new();
        let mut semicolon = false;
        for syllable in syllables {
            // 感叹词音节（嗯 ng / 呣 m）要跳过：它们的码会跟正常音节撞车 ——
            // 小鹤里 `ng` 既是「嗯」也是「能」(neng)，被「嗯」占了之后打 bung 就出不来「不能」。
            // 反正双拼里也没人用两键打「嗯」（都打 en）
            if UNENCODABLE.contains(&syllable.as_str()) {
                continue;
            }
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
        // 零声母音节：本方案自家规则是"首字母 + 韵母键"（安 = `aj`），
        // 但 Rime / fcitx5 / 搜狗那几家**同时**认全拼（安 = `an`）。
        // 两种都收 —— 按全拼打的人不用先学规则。两字母的这些不可能跟谁撞车
        //（首字母是 a/o/e，这几个不是声母），真撞了就跳过，让原来那个优先
        for syllable in ZERO_INITIAL_FULL {
            if !syllables.iter().any(|known| known == syllable) || by_code.contains_key(*syllable) {
                continue;
            }
            by_code.insert((*syllable).to_string(), (*syllable).to_string());
            by_first
                .entry(syllable.chars().next().unwrap())
                .or_default()
                .push((*syllable).to_string());
        }
        for list in by_first.values_mut() {
            list.sort();
            list.dedup();
        }
        Self {
            by_code,
            by_first,
            semicolon,
            sentence,
        }
    }

    /// 两键码 → 音节（`ni` → `ni`，`hc` → `hao`）
    pub fn decode_syllable(&self, code: &str) -> Option<&str> {
        self.by_code.get(code).map(String::as_str)
    }

    /// 把输入两个一组解成音节。
    ///
    /// 返回 `None` 表示这串键里有解不出音节的组（整个输入作废）；
    /// 否则是「已经解出来的音节」+「最后剩下的那半截键」
    fn decode_pairs(&self, input: &str) -> Option<(Vec<String>, Option<char>)> {
        let chars: Vec<char> = input.chars().collect();
        let mut complete: Vec<String> = Vec::new();

        // 两个一组往下解
        let mut index = 0;
        while index + 1 < chars.len() {
            let pair: String = chars[index..index + 2].iter().collect();
            let syllable = self.decode_syllable(&pair)?;
            complete.push(syllable.to_string());
            index += 2;
        }
        Some((complete, chars.get(index).copied()))
    }

    /// 分段上屏用的前缀码：双拼两键一个音节，所以前缀只切在偶数长度上。
    /// 返回 `(码, 吃了几个字符)`，长的在前
    fn prefix_codes(&self, input: &str) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        let chars = input.chars().count();
        let mut cut = chars.saturating_sub(1);
        while cut >= 4 {
            let head: String = input.chars().take(cut).collect();
            if let Some((syllables, None)) = self.decode_pairs(&head)
                && syllables.len() >= 2
            {
                out.push((syllables.join(" "), cut));
            }
            cut -= 1;
        }
        out
    }

    /// 输入 → 要查的拼音码
    fn lookup_codes(&self, input: &str) -> Vec<String> {
        let Some((complete, tail)) = self.decode_pairs(input) else {
            return Vec::new();
        };
        let Some(first) = tail else {
            // 正好解完，不再往后联想（跟全拼一个规矩：打 ni 就只查 ni）
            return vec![complete.join(" ")];
        };

        // 剩一个键：把它当作某个音节的第一个键，枚举可能的音节
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

    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<Candidate> {
        let mut words = merge(dict, self.name(), &self.lookup_codes(input), limit);
        // 整句候选：键正好两两解完（一组一个音节）时才有得拼
        if self.sentence
            && let Some((syllables, None)) = self.decode_pairs(input)
            && syllables.len() >= 2
        {
            push_sentences(&mut words, dict, self.name(), &syllables, limit);
        }

        let mut out = whole(input, words);
        // 分段匹配：双拼两键一个音节，所以前缀只切在偶数位置上
        out.extend(prefix_candidates(
            dict,
            self.name(),
            &self.prefix_codes(input),
            limit,
        ));
        out.truncate(limit);
        out
    }
}

// ---- 码表方案（五笔这类）-----------------------------------------------------

/// 码表方案：键本身就是码，直接拿去查（五笔、郑码、仓颉……都是这个形状）。
///
/// 词库那张表有 `scheme` 字段，所以一套码表就是一批 `scheme = 'wubi'` 的行。
/// 导入用 `pliers-dict --table 码表.txt --table-scheme wubi`。
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

    fn candidates(&self, dict: &Dict, input: &str, limit: usize) -> Vec<Candidate> {
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
        // 码表方案没有"分段"这回事：码就是码，一次打完
        whole(input, out)
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
        let layout = Layout::preset("natural").unwrap();
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
        let flypy = Layout::preset("flypy").unwrap();
        let mspy = Layout::preset("mspy").unwrap();
        assert_eq!(flypy.encode("hao").as_deref(), Some("hc"));
        assert_eq!(flypy.encode("bei").as_deref(), Some("bw"));
        assert_eq!(flypy.encode("xin").as_deref(), Some("xb"));
        assert_eq!(mspy.encode("xing").as_deref(), Some("x;"));
    }

    #[test]
    fn 每个音节都能编成两键() {
        // 双拼是"两键一音节"，`m`/`n`/`ng`（呣/嗯/唔）这种单键感叹词塞不进这个模型 ——
        // 词表里有，但双拼方案一般打 `en`（恩）。除了它们，别的音节都要能编出来
        for name in ["natural", "flypy", "mspy"] {
            let layout = Layout::preset(name).unwrap();
            for syllable in syllables() {
                if let Some(code) = layout.encode(&syllable) {
                    assert_eq!(code.chars().count(), 2, "{syllable} → {code}");
                }
            }
            assert!(
                layout.unencodable(&syllables()).is_empty(),
                "{name} 漏了：{:?}",
                layout.unencodable(&syllables())
            );
        }
    }

    #[test]
    fn 双拼打你好() {
        let double = DoublePinyin::new(Layout::preset("natural").unwrap(), &syllables(), true);
        // 自然码：ni = n+i，hao = h+k
        assert_eq!(double.lookup_codes("nihk"), ["ni hao"]);
        // 小鹤：hao = h+c
        let flypy = DoublePinyin::new(Layout::preset("flypy").unwrap(), &syllables(), true);
        assert_eq!(flypy.lookup_codes("nihc"), ["ni hao"]);
    }

    #[test]
    fn 零声母音节的全拼也认() {
        // 小鹤自家的规则是「安 = aj」，但大家都习惯按全拼打 —— 两种都得认
        let flypy = DoublePinyin::new(Layout::preset("flypy").unwrap(), &syllables(), true);
        // 小鹤自家的码：安 = a+(an→j) = `aj`，爱 = a+(ai→d) = `ad`；
        // 全拼的 `an` / `ai` 现在也认（欧/恩/儿本来就是全拼，重复插入会被跳过）
        for (code, want) in [
            ("aj", "an"),
            ("an", "an"),
            ("ad", "ai"),
            ("ai", "ai"),
            ("ou", "ou"),
            ("en", "en"),
            ("er", "er"),
        ] {
            assert_eq!(
                flypy.decode_syllable(code),
                Some(want),
                "{code} 该解成 {want}"
            );
        }
        // 声母+韵母的老码一个都不能坏
        assert_eq!(flypy.decode_syllable("ni"), Some("ni"));
        assert_eq!(flypy.decode_syllable("hc"), Some("hao"));
        assert_eq!(flypy.decode_syllable("bung"), None, "这是四键，不是一组");
    }

    #[test]
    fn 每个音节的码都解回自己() {
        // 撞车的后果是"某个音节永远打不出来"：`ng` 这个码被「嗯」占了之后，
        // 小鹤用户打 bung 就再也出不来「不能」
        for name in ["natural", "flypy", "mspy"] {
            let layout = Layout::preset(name).unwrap();
            let double = DoublePinyin::new(layout.clone(), &syllables(), true);
            for syllable in syllables() {
                if UNENCODABLE.contains(&syllable.as_str()) {
                    continue; // 感叹词双拼打不出来（嗯 打 en），不算
                }
                let code = layout.encode(&syllable).unwrap();
                assert_eq!(
                    double.decode_syllable(&code),
                    Some(syllable.as_str()),
                    "{name}：码 {code:?} 解出来不是 {syllable}"
                );
            }
        }
    }

    #[test]
    fn 双拼能打出不能() {
        // 用户报的那个 bug：小鹤 bung = bu + neng = 不能
        let layout = Layout::preset("flypy").unwrap();
        let double = DoublePinyin::new(layout, &syllables(), true);
        assert_eq!(double.lookup_codes("bung"), ["bu neng"]);
        // 自然码和微软的 neng 也在 g 上
        for name in ["natural", "mspy"] {
            let double = DoublePinyin::new(Layout::preset(name).unwrap(), &syllables(), true);
            assert_eq!(double.lookup_codes("bung"), ["bu neng"], "{name}");
        }
    }

    #[test]
    fn 双拼的半截音节也补全() {
        let double = DoublePinyin::new(Layout::preset("natural").unwrap(), &syllables(), true);
        // 只打了 h：所有 h 开头的音节都该试一遍，包括 hao
        let codes = double.lookup_codes("nih");
        assert!(codes.contains(&"ni hao".to_string()), "{codes:?}");
    }

    #[test]
    fn 自定义键位能在预设上改() {
        // 就用自然码，但把 ao 从 k 挪到 c（小鹤的位置）
        let keys: BTreeMap<String, String> =
            [("ao".to_string(), "c".to_string())].into_iter().collect();
        let layout = Layout::preset("natural").unwrap().with_keys(&keys).unwrap();
        assert_eq!(layout.encode("hao").as_deref(), Some("hc"));
        // 没动过的键还按预设来
        assert_eq!(layout.encode("ni").as_deref(), Some("ni"));
        assert_eq!(layout.encode("zhang").as_deref(), Some("vh"));
    }

    #[test]
    fn 键位表可以从零写() {
        // layout = "none" + 自己填一套：这里只填几个，验证写法通了
        let keys: BTreeMap<String, String> = [("zh", "u"), ("ao", "c"), ("ang", "h"), ("an", "j")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let layout = Layout::empty().with_keys(&keys).unwrap();
        assert_eq!(layout.encode("zhang").as_deref(), Some("uh"));
        assert_eq!(layout.encode("hao").as_deref(), Some("hc"));
        // 没定义的韵母就编不出来：ai 没写，所以 bai 出不来 —— unencodable 会列出来
        let missing = layout.unencodable(&syllables());
        assert!(missing.contains(&"bai".to_string()), "{missing:?}");
        // 单字母韵母不用定义：ni 就是 n + i
        assert!(!missing.contains(&"ni".to_string()), "{missing:?}");
    }

    #[test]
    fn 键位写错了要报错() {
        // 值不是单个字符
        let keys: BTreeMap<String, String> =
            [("ao".to_string(), "kk".to_string())].into_iter().collect();
        let err = Layout::preset("natural")
            .unwrap()
            .with_keys(&keys)
            .unwrap_err();
        assert!(err.contains("一个字符"), "{err}");

        // 键不是字母
        let keys: BTreeMap<String, String> =
            [("ao".to_string(), "1".to_string())].into_iter().collect();
        assert!(Layout::preset("natural").unwrap().with_keys(&keys).is_err());
    }

    #[test]
    fn 报错要说缺的是哪个韵母() {
        let mut keys: BTreeMap<String, String> = BTreeMap::new();
        for (finals, key) in NATURAL_FINALS {
            keys.insert((*finals).to_string(), key.to_string());
        }
        keys.remove("ang");
        keys.remove("ai");
        let layout = Layout::empty().with_keys(&keys).unwrap();
        assert_eq!(layout.missing_finals(&syllables()), ["ai", "ang"]);
    }

    #[test]
    fn 漏写韵母会被查出来() {
        // 故意把自然码的 ang 删掉（拿一个不存在的键名去覆盖不会删掉原来的，
        // 这里直接用空表 + 少一个韵母的写法）
        let mut keys: BTreeMap<String, String> = BTreeMap::new();
        for (finals, key) in NATURAL_FINALS {
            keys.insert((*finals).to_string(), key.to_string());
        }
        keys.remove("ang");
        let layout = Layout::empty().with_keys(&keys).unwrap();
        let missing = layout.unencodable(&syllables());
        assert!(missing.contains(&"ang".to_string()), "{missing:?}");
        assert!(missing.contains(&"zhang".to_string()), "{missing:?}");
        // 感叹词不算"漏"（双拼本来就没法打）
        assert!(!missing.contains(&"ng".to_string()));
    }

    #[test]
    fn 微软双拼收分号() {
        let mspy = DoublePinyin::new(Layout::preset("mspy").unwrap(), &syllables(), true);
        assert!(mspy.accepts(';'));
        assert!(mspy.accepts('z'));
        let natural = DoublePinyin::new(Layout::preset("natural").unwrap(), &syllables(), true);
        assert!(!natural.accepts(';'));
    }
}
