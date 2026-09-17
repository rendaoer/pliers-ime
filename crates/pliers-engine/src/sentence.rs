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

/// 音节序列 → 整句候选（分数从高到低，去重后最多 `limit` 条）。
///
/// 只处理**两个音节以上**的输入：一个音节交给"精确查词"那条路，
/// 在这儿拼也拼不出花样（反而会给单字加上长词奖励，把候选搞乱）
pub fn candidates(dict: &Dict, scheme: &str, syllables: &[String], limit: usize) -> Vec<String> {
    let count = syllables.len();
    if count < 2 || limit == 0 {
        return Vec::new();
    }

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
            let code = syllables[start..start + len].join(" ");
            // 只看权重最高的那个词：一条边上放多个词会让路数爆炸
            let Some((word, weight)) = dict.exact(scheme, &code, 1).into_iter().next() else {
                continue;
            };
            let chars = word.chars().count();
            let gain = (weight.max(1) as f64).ln() + CHAR_BONUS * (chars.saturating_sub(1) as f64);

            let mut extended = Vec::with_capacity(paths[start].len());
            for path in &paths[start] {
                let mut words = path.words.clone();
                words.push(word.clone());
                extended.push(Path {
                    score: path.score + gain,
                    words,
                });
            }
            let target = &mut paths[start + len];
            target.extend(extended);
            target.sort_by(|a, b| b.score.total_cmp(&a.score));
            target.truncate(BEAM);
        }
    }

    let mut best = std::mem::take(&mut paths[count]);
    best.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut out: Vec<String> = Vec::with_capacity(limit);
    for path in best {
        let text = path.text();
        if !text.is_empty() && !out.contains(&text) {
            out.push(text);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
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
}
