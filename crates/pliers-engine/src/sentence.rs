//! 整句候选：把一串音节切成词，拼出一句话。
//!
//! 为什么需要它：词库里存的是**词**。`nihaoma` 这种连着打好几个音节的情况，
//! 库里没有「你好吗」这个词就一个候选都出不来 —— 用户只能打一个词、上屏、再打下一个。
//! 正经输入法这里给的是"整句"候选：把音节切成词，挑最像的一种切法拼起来。
//!
//! 做法是最短路径（Viterbi）：音节序列上从位置 `i` 往后枚举 1..=[`MAX_WORD_SYLLABLES`]
//! 个音节的词，每个查一次库（精确命中，跟别的查询一样快），边权
//! `ln(词权重) + 长词奖励 ×(字数-1)`，最后取总分最高的一条路。
//!
//! 长词奖励不是装饰：单字的词频天生比词高（打 `shijian` 时「是」+「见」两个单字的
//! 权重加起来能压过「时间」），没有奖励就会切成"一个字一个字"的傻句子。

use std::collections::HashSet;

use crate::dict::Dict;

/// 一个词最多几个音节。词库里有 9 个音节的词（「巴布亚长斑裳凤蝶」），
/// 但那些纯属巧合，够到 6 个就覆盖了日常词汇
const MAX_WORD_SYLLABLES: usize = 6;

/// 长词每多一个字加多少分（自然对数单位）：e^18 ≈ 6500 万。
/// 这个数要够大，让「时间」压过「是」+「见」；又不能太大，免得一个生僻长词
/// 把正常的"两字词 + 两字词"挤掉。数字是拿真词库试出来的：
/// ln(是 7969910) + ln(见 589650) = 29.18，ln(时间 332880) = 12.72，
/// 差额 16.46 —— 所以 18 够用还留了点余量
const CHAR_BONUS: f64 = 18.0;

/// 每个位置最多留几条路（k-best 的束宽）。留几条最后就能给几个整句候选
const BEAM: usize = 3;

/// 半截音节补全出来的整句，比最好的低多少分就不要了（自然对数单位）。
/// e^5 ≈ 148：补成 `zai` 能拼出「那我现在」，补成 `zuo` 只能拼出
/// 「那我先做」—— 后者比前者低十几分，是上千倍的差距，列出来只是噪音
const TAIL_MARGIN: f64 = 5.0;

/// 一条路：分数 + 已经拼出来的词
struct Path {
    score: f64,
    words: Vec<String>,
}

impl Path {
    fn text(&self) -> String {
        self.words.concat()
    }
}

/// 从 `start` 往后吃 `len` 个音节，查词并算这条边的分数。
/// 只看权重最高的那个词：一条边上放多个词会让路数爆炸
fn edge(
    dict: &Dict,
    scheme: &str,
    syllables: &[String],
    start: usize,
    len: usize,
) -> Option<(String, f64)> {
    let code = syllables[start..start + len].join(" ");
    let (word, weight) = dict.exact_sentence(scheme, &code, 1).into_iter().next()?;
    let chars = word.chars().count();
    let gain = (weight.max(1) as f64).ln() + CHAR_BONUS * (chars.saturating_sub(1) as f64);
    Some((word, gain))
}

/// 把 `word` 接到 `paths[start]` 的每条路上，落到 `paths[start + len]`。
fn advance(paths: &mut [Vec<Path>], start: usize, len: usize, word: &str, gain: f64) {
    let mut extended = Vec::with_capacity(paths[start].len());
    for path in &paths[start] {
        let mut words = path.words.clone();
        words.push(word.to_string());
        extended.push(Path {
            score: path.score + gain,
            words,
        });
    }
    let target = &mut paths[start + len];
    target.extend(extended);
    target.sort_by(|a, b| b.score.total_cmp(&a.score));
    // 同一个句子可能有好几种切法，留分数最高的那种就够了；
    // 不然前 3 条全是「那我现在」，把「那我先在」这种真正不同的候选挤掉
    let mut seen = HashSet::new();
    target.retain(|path| seen.insert(path.text()));
    target.truncate(BEAM);
}

/// Viterbi 前半截：算出盖住每个前缀位置的最好几条路。
fn build_paths(dict: &Dict, scheme: &str, syllables: &[String]) -> Vec<Vec<Path>> {
    let count = syllables.len();
    // paths[i]：盖住前 i 个音节的最好的几条路
    let mut paths: Vec<Vec<Path>> = Vec::with_capacity(count + 1);
    paths.resize_with(count + 1, Vec::new);
    paths[0].push(Path {
        score: 0.0,
        words: Vec::new(),
    });

    for start in 0..count {
        if paths[start].is_empty() {
            continue; // 这个位置前面已经走不通了
        }
        for len in 1..=MAX_WORD_SYLLABLES.min(count - start) {
            let Some((word, gain)) = edge(dict, scheme, syllables, start, len) else {
                continue;
            };
            advance(&mut paths, start, len, &word, gain);
        }
    }
    paths
}

/// 收尾：按分数从高到低、按文字去重、最多 `limit` 条。
fn finish(mut best: Vec<Path>, limit: usize) -> Vec<(String, f64)> {
    best.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut out: Vec<(String, f64)> = Vec::with_capacity(limit);
    for path in best {
        let text = path.text();
        if !text.is_empty() && !out.iter().any(|(old, _)| *old == text) {
            out.push((text, path.score));
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// 音节序列 → 整句候选（分数从高到低，去重后最多 `limit` 条）。
///
/// 只处理**两个音节以上**的输入：一个音节交给"精确查词"那条路，
/// 在这儿拼也拼不出花样（反而会给单字加上长词奖励，把候选搞乱）
pub fn candidates(dict: &Dict, scheme: &str, syllables: &[String], limit: usize) -> Vec<String> {
    scored_candidates(dict, scheme, syllables, limit)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// 跟 [`candidates`] 一样，但把分数一起带出来。
pub fn scored_candidates(
    dict: &Dict,
    scheme: &str,
    syllables: &[String],
    limit: usize,
) -> Vec<(String, f64)> {
    if syllables.len() < 2 || limit == 0 {
        return Vec::new();
    }
    let mut paths = build_paths(dict, scheme, syllables);
    finish(std::mem::take(&mut paths[syllables.len()]), limit)
}

/// 句尾只打了半截时拼整句：`prefix` 是已经解出来的完整音节，`tails` 是
/// 最后一个键可能补成的所有音节（`z` → zai/zan/za…）。
///
/// 跟「每个补全各跑一遍完整 Viterbi」相比，前缀那半截的 DP 只算一次，
/// 每个补全只补最后的边 —— `nawoxmz` 有二十来种补全，这样才不至于
/// 每次按键多花十几毫秒。
pub fn candidates_with_tail(
    dict: &Dict,
    scheme: &str,
    prefix: &[String],
    tails: &[String],
    limit: usize,
) -> Vec<String> {
    let count = prefix.len();
    if count == 0 || tails.is_empty() || limit == 0 {
        return Vec::new();
    }

    let paths = build_paths(dict, scheme, prefix);
    let mut finals: Vec<Path> = Vec::new();
    for tail in tails {
        // 句尾那个词必须含 tail，所以从 start 到句尾正好是「剩下几个前缀音节 + tail」
        for start in 0..=count {
            let len = count + 1 - start;
            if len > MAX_WORD_SYLLABLES {
                continue;
            }
            let mut parts: Vec<&str> = prefix[start..].iter().map(String::as_str).collect();
            parts.push(tail);
            let code = parts.join(" ");
            let Some((word, weight)) = dict.exact_sentence(scheme, &code, 1).into_iter().next()
            else {
                continue;
            };
            let chars = word.chars().count();
            let gain = (weight.max(1) as f64).ln() + CHAR_BONUS * (chars.saturating_sub(1) as f64);
            for path in &paths[start] {
                let mut words = path.words.clone();
                words.push(word.clone());
                finals.push(Path {
                    score: path.score + gain,
                    words,
                });
            }
        }
    }

    let out = finish(finals, limit);
    // 明显不如第一名的补全就不列了：`nawoxmz` 该给「那我现在」，
    // 不是「那我先做」「那我先最」这种句尾随便补一个字的噪音
    let best = out
        .first()
        .map(|(_, score)| *score)
        .unwrap_or(f64::NEG_INFINITY);
    out.into_iter()
        .take_while(|(_, score)| *score >= best - TAIL_MARGIN)
        .map(|(text, _)| text)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::testing::sample_dict;

    fn syllables(input: &[&str]) -> Vec<String> {
        input.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn 库里没有整词也能拼出句子() {
        // 「你好吗」不在词库里：你好 + 吗 拼出来
        let dict = sample_dict();
        let out = candidates(&dict, "pinyin", &syllables(&["ni", "hao", "ma"]), 3);
        assert_eq!(out[0], "你好吗", "{out:?}");
    }

    #[test]
    fn 优先用长词而不是一个字一个字() {
        // shi + jian：单字「是」「见」的权重加起来比「时间」高，
        // 靠长词奖励才能选对
        let dict = sample_dict();
        let out = candidates(&dict, "pinyin", &syllables(&["shi", "jian"]), 3);
        assert_eq!(out[0], "时间", "{out:?}");
    }

    #[test]
    fn 一元词频模型分不清你号码和你好吗() {
        // 诚实记一笔：`nihaoma` 的两个切法「你好|吗」和「你|号码」在词频模型里
        // 是同一档次（都是 2 个词、3 个字），谁在前面全看库里那两个词的权重谁高，
        // 所以词库里没有整词时可能先给一个不太对的切法。用户选过一次「你好」之后
        // （user_word 加权重），「你好吗」就会翻上来 —— 见 engine 的测试
        let dict = sample_dict();
        let out = candidates(&dict, "pinyin", &syllables(&["ni", "hao", "ma"]), 3);
        assert!(out.contains(&"你好吗".to_string()), "{out:?}");
    }

    #[test]
    fn 两字词加单字也能拼() {
        let dict = sample_dict();
        let out = candidates(&dict, "pinyin", &syllables(&["ni", "hao", "bu"]), 3);
        assert_eq!(out[0], "你好不", "{out:?}");
    }

    #[test]
    fn 一个音节不拼句() {
        let dict = sample_dict();
        assert!(candidates(&dict, "pinyin", &syllables(&["ni"]), 3).is_empty());
    }

    #[test]
    fn 查不到的读音就拼不出来() {
        let dict = sample_dict();
        // 词库里没有任何 qq 相关的词
        assert!(candidates(&dict, "pinyin", &syllables(&["qq", "qq"]), 3).is_empty());
    }

    #[test]
    fn 候选不会重复() {
        let dict = sample_dict();
        let out = candidates(&dict, "pinyin", &syllables(&["ni", "hao"]), 3);
        let mut sorted = out.clone();
        sorted.dedup();
        assert_eq!(sorted, out, "同一个句子不该出现两次：{out:?}");
    }

    #[test]
    fn 半截句尾补全后拼整句() {
        let dict = sample_dict();
        // na wo xian + z：补成 zai 才是「那我现在」，补成 zuo 只是「那我先做」
        let out = candidates_with_tail(
            &dict,
            "pinyin",
            &syllables(&["na", "wo", "xian"]),
            &syllables(&["zuo", "zai"]),
            5,
        );
        assert_eq!(out.first().map(String::as_str), Some("那我现在"), "{out:?}");
        assert!(
            !out.iter().any(|text| text == "那我先做"),
            "差一百多倍的补全不该列出来：{out:?}"
        );
    }

    #[test]
    fn 误选一两次不带偏整句切法() {
        let dict = sample_dict();
        // 用户顺手点过两次「那我先」：单字候选里它该往前挪，
        // 但整句不该因此把「那我现在」挤掉
        dict.note_used("那我先").unwrap();
        dict.note_used("那我先").unwrap();
        let out = candidates_with_tail(
            &dict,
            "pinyin",
            &syllables(&["na", "wo", "xian"]),
            &syllables(&["zai"]),
            5,
        );
        assert_eq!(out.first().map(String::as_str), Some("那我现在"), "{out:?}");

        // 选到第三次，说明是真习惯，整句也跟着调
        dict.note_used("那我先").unwrap();
        let out = candidates_with_tail(
            &dict,
            "pinyin",
            &syllables(&["na", "wo", "xian"]),
            &syllables(&["zai"]),
            5,
        );
        assert_eq!(out.first().map(String::as_str), Some("那我先在"), "{out:?}");
    }

    #[test]
    fn 一个音节加半截也拼句() {
        let dict = sample_dict();
        // 只有一个完整音节时也要能拼：na + 半截 z
        let out = candidates_with_tail(
            &dict,
            "pinyin",
            &syllables(&["na"]),
            &syllables(&["zai", "zuo"]),
            5,
        );
        assert_eq!(out.first().map(String::as_str), Some("那在"), "{out:?}");
    }
}
