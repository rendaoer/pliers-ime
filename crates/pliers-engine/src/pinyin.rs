//! 切词：把用户敲的一串字母切成合法音节。
//!
//! 这是全拼最核心、也最容易写错的一块。为什么要自己切：**数据库不能做这件事**。
//! 如果拿 `code LIKE 'ni%'` 去问 SQLite，它会扫出一大片再排序 —— 实测在 20 万行的
//! 表上要 280ms，打一个 `n` 就能卡住输入法。所以切词在内存里做（412 个音节，
//! 一次切分几微秒），数据库那边只回答"这个**完整的**拼音码对应哪些词"（60µs）。
//!
//! 切法说明（跟 Rime 的行为对齐）：
//!
//! * `nihao`  → 完整音节 `ni` + `hao` → 查 `ni hao`
//! * `xian`   → 既能切 `xian`，也能切 `xi an` → 两个码都查，「先」和「西安」都能出来
//! * `nih`    → `ni` 切得动，尾巴 `h` 切不动 → 把 `h` 补全成 ha/hai/…/hao，
//!   于是「你好」在打到第三个字母时就能看见
//! * `niha`   → `ni` 和 `ha` 都切得动，但**最后一个音节照样往后补**：
//!   补出 `ni hao` 才看得见「你好」；再多打一个 `o` 得到精确匹配，「你好」还是第一
//! * `ni`     → 只有一个音节而且已经打完了，**不再往后联想**（否则打 `ni`
//!   会冒出一堆「年/牛/您」）。要「你好」就继续打 `hao`
//! * `n`      → 例外：`n`（嗯）本身是个音节，但只打一个字母的时候，
//!   用户想看的显然是所有 n 开头的字（你/那/年…），所以单字母照样往后联想
//!
//! 为什么"多音节才补最后一个音节"：`shiji` 补出 `shi jian`/`shi jie`，
//! 「时间」「世界」都能看见（好事）；而 `xian` 只有一个音节，补了就会冒出
//! 一堆 `xiang` 的字（坏事）

/// 一次按键最多查几个码。切分分支 × 补全数量可能爆掉，得有个上限：
/// 每个码是一次数据库查询（60µs 上下），32 个也就 2ms
pub const MAX_CODES: usize = 32;
/// 最多考虑几种切法（`xian` 那种歧义一般只有两三种）
const MAX_SEGMENTATIONS: usize = 4;
/// 一个半截音节最多补全成几个音节
const MAX_COMPLETIONS: usize = 24;

/// 这些"感叹词音节"很少落在分段点上（`nihaom` 不该切成 `ni hao m`）
const NOT_A_SEGMENT_TAIL: &[&str] = &["m", "n", "ng", "hm", "hng"];

/// 标准普通话音节表（412 个）。**运行时是从词库里读的**（`syllable` 表），
/// 这份只是给测试和文档用的参考 —— 唯一的例外是导入工具：码表库（五笔/郑码/仓颉）
/// 用不着切词，但要跟拼音词库**结构一样**（音节表是 `Dict::open` 的体检项之一），
/// 所以 `pliers-dict` 拿这份把表填上
pub const SYLLABLES: &str = "\
a ai an ang ao ba bai ban bang bao bei ben beng bi bian biao bie bin bing bo bu ca cai can cang \
cao ce cen ceng cha chai chan chang chao che chen cheng chi chong chou chu chua chuai chuan chuang \
chui chun chuo ci cong cou cu cuan cui cun cuo da dai dan dang dao de dei den deng di dia dian \
diao die ding diu dong dou du duan dui dun duo e ei en er fa fan fang fei fen feng fo fou fu ga \
gai gan gang gao ge gei gen geng gong gou gu gua guai guan guang gui gun guo ha hai han hang hao \
he hei hen heng hong hou hu hua huai huan huang hui hun huo ji jia jian jiang jiao jie jin jing \
jiong jiu ju juan jue jun ka kai kan kang kao ke kei ken keng kong kou ku kua kuai kuan kuang \
kui kun kuo la lai lan lang lao le lei leng li lia lian liang liao lie lin ling liu long lou lu \
luan lun luo lv lve m ma mai man mang mao me mei men meng mi mian miao mie min ming miu mo mou \
mu n na nai nan nang nao ne nei nen neng ng ni nian niang niao nie nin ning niu nong nou nu nuan \
nun nuo nv nve o ou pa pai pan pang pao pei pen peng pi pian piao pie pin ping po pou pu qi qia \
qian qiang qiao qie qin qing qiong qiu qu quan que qun ran rang rao re ren reng ri rong rou ru \
rua ruan rui run ruo sa sai san sang sao se sen seng sha shai shan shang shao she shei shen sheng \
shi shou shu shua shuai shuan shuang shui shun shuo si song sou su suan sui sun suo ta tai tan \
tang tao te tei teng ti tian tiao tie ting tong tou tu tuan tui tun tuo wa wai wan wang wei wen \
weng wo wu xi xia xian xiang xiao xie xin xing xiong xiu xu xuan xue xun ya yan yang yao ye yi \
yin ying yo yong you yu yuan yue yun za zai zan zang zao ze zei zen zeng zha zhai zhan zhang zhao \
zhe zhen zheng zhi zhong zhou zhu zhua zhuai zhuan zhuang zhui zhun zhuo zi zong zou zu zuan zui \
zun zuo";

/// 切词器。音节表是从词库里读出来的 412 个音节 —— 词库说什么音合法，就是什么
pub struct Segmenter {
    /// 按长度从长到短排好，切的时候优先匹配长音节
    by_length: Vec<String>,
}

impl Segmenter {
    pub fn new(syllables: &[String]) -> Self {
        let mut by_length = syllables.to_vec();
        by_length.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        by_length.dedup();
        Self { by_length }
    }

    /// 输入 → 要查的码（去重、保序、最多 [`MAX_CODES`] 个）
    pub fn lookup_codes(&self, input: &str) -> Vec<String> {
        let mut codes = Vec::new();

        // 1. 能完整切完的切法：`xian` → ["xian"] 和 ["xi","an"] 都查
        let complete = self.segmentations(input, MAX_SEGMENTATIONS);
        for syllables in &complete {
            codes.push(syllables.join(" "));
        }

        // 2. 已经切出两个以上音节了：最后一个音节再往后补一补。
        //    shiji → 除了 shi ji，也查 shi jian / shi jie / shi jing……
        //    （两三个字母的单个音节不补：打 ni 不该冒出「年/牛/您」）
        //
        //    只打了一个字母是例外：`n`、`m`、`a`、`o` 这些"感叹词音节"本身合法，
        //    但用户打一个 `n` 想看的是「你/那/年」这一串，不是「嗯」这一个字
        let one_letter = input.chars().count() == 1;
        if one_letter || complete.iter().any(|syllables| syllables.len() >= 2) {
            for syllables in &complete {
                let Some((last, head)) = syllables.split_last() else {
                    continue;
                };
                // 两个音节以上才补；只打一个字母时（n/m/a）一个音节也补
                if syllables.len() < 2 && !one_letter {
                    continue;
                }
                for longer in self.completions(last) {
                    if longer == last {
                        continue;
                    }
                    // 用 join 拼，别手写空格：前缀为空时（只打了一个字母）会多出一个前导空格
                    let mut parts: Vec<&str> = head.iter().map(String::as_str).collect();
                    parts.push(longer);
                    codes.push(parts.join(" "));
                    if codes.len() >= MAX_CODES {
                        break;
                    }
                }
            }
        }

        // 3. 尾巴上还有半截音节：把最长的、切得动的前缀切出来，剩下那截去补全
        if complete.is_empty() {
            for (prefix, tail) in self.split_tail(input) {
                for syllable in self.completions(&tail) {
                    let mut code = prefix.clone();
                    if !code.is_empty() {
                        code.push(' ');
                    }
                    code.push_str(syllable);
                    codes.push(code);
                    if codes.len() >= MAX_CODES {
                        break;
                    }
                }
                if codes.len() >= MAX_CODES {
                    break;
                }
            }
        }

        codes.dedup();
        codes.truncate(MAX_CODES);
        codes
    }

    /// 所有能把整串切完的切法（音节数少的排前面）。
    /// 整句候选要用它拿到"音节序列"，不是拼好的码
    pub fn complete_segmentations(&self, input: &str) -> Vec<Vec<String>> {
        self.segmentations(input, MAX_SEGMENTATIONS)
    }

    /// 这串字母是不是某个音节的"半截"（`sho` 是 `shou` 的半截，`nih` 不是任何音节的开头）。
    ///
    /// 用来把"拼音打到一半"和"英文单词"分开：打 `sho` 的时候用户是在打 `shou`
    ///（说/手/受），不该冒出 `should` 来抢候选。空串算（还没打呢，当然是中文）
    pub fn is_syllable_prefix(&self, input: &str) -> bool {
        self.by_length
            .iter()
            .any(|syllable| syllable.starts_with(input))
    }

    /// 分段上屏用的前缀码：把输入按音节边界切成"前一段 + 剩下的一截"，
    /// 返回 `(前一段的码, 它吃了几个字符)`，**长的前缀排在前面**。
    ///
    /// `nihaoma` → `[("ni hao", 5), ("ni", 2)]`：先试「你好」，不行再看「你」，
    /// 选中的候选上屏之后 `ma` 接着组 —— 不用一次性把整串匹配完
    pub fn prefix_codes(&self, input: &str) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        if input.chars().count() < 2 {
            return out; // 一个字母没什么好"分段"的
        }
        // 每个字符位置都当一次切点，但只有切出来能完整成音节的才算
        for cut in (1..input.len()).rev() {
            if !input.is_char_boundary(cut) {
                continue;
            }
            let (head, _tail) = input.split_at(cut);
            let Some(syllables) = self.segmentations(head, 1).into_iter().next() else {
                continue;
            };
            if syllables.is_empty() {
                continue;
            }
            // 尾巴是"感叹词音节"（呣 m、嗯 ng）的不算分段点：
            // 不然 `nihaom` 会切出 `ni hao m`，白白占掉一个前缀名额
            if syllables
                .last()
                .is_some_and(|syllable| NOT_A_SEGMENT_TAIL.contains(&syllable.as_str()))
            {
                continue;
            }
            let consumed = head.chars().count();
            let code = syllables.join(" ");
            if !out.iter().any(|(old, _)| *old == code) {
                out.push((code, consumed));
            }
        }
        out
    }

    /// 所有能把整串切完的切法（最多 `max` 种），音节数少的排前面
    fn segmentations(&self, input: &str, max: usize) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        self.walk(input, Vec::new(), &mut out, max);
        out.sort_by_key(Vec::len);
        out.truncate(max);
        out
    }

    fn walk(&self, rest: &str, done: Vec<String>, out: &mut Vec<Vec<String>>, max: usize) {
        if out.len() >= max * 8 {
            return; // 剪枝：够用了，别再往下找了
        }
        if rest.is_empty() {
            out.push(done);
            return;
        }
        for syllable in &self.by_length {
            if let Some(tail) = rest.strip_prefix(syllable.as_str()) {
                let mut next = done.clone();
                next.push(syllable.clone());
                self.walk(tail, next, out, max);
            }
        }
    }

    /// 切不干净时用：`("ni", "h")` —— 前面切得动的那部分 + 后面那截半成品。
    /// 前缀从长到短试，最多给几种
    fn split_tail(&self, input: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        // 从最长前缀开始：先看能不能把前 n-1 个字符切完，最后一段当半截音节
        for cut in (0..input.len()).rev() {
            let (head, tail) = input.split_at(cut);
            if tail.is_empty() || self.completions(tail).next().is_none() {
                continue; // 这截尾巴补不成任何音节，不算数
            }
            // head 为空 = 整个输入就是个半截音节（比如只打了一个 n）
            let segments = if head.is_empty() {
                vec![Vec::new()]
            } else {
                self.segmentations(head, 1)
            };
            for syllables in segments {
                out.push((syllables.join(" "), tail.to_string()));
                if out.len() >= MAX_SEGMENTATIONS {
                    return out;
                }
            }
        }
        out
    }

    /// 以 `prefix` 开头的音节（按字典序）
    fn completions(&self, prefix: &str) -> impl Iterator<Item = &str> {
        self.by_length
            .iter()
            .filter(move |syllable| syllable.starts_with(prefix))
            .map(|syllable| syllable.as_str())
            .take(MAX_COMPLETIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segmenter() -> Segmenter {
        Segmenter::new(
            &SYLLABLES
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn 简单的两音节() {
        // 首选切法（音节最少）排最前面，另外还有 "ni ha o" 这种切法，
        // 它们查出来的是生僻词，权重低，影响不到候选顺序
        let codes = segmenter().lookup_codes("nihao");
        assert_eq!(codes[0], "ni hao");
        assert!(codes.iter().all(|code| code.starts_with("ni ha")));
    }

    #[test]
    fn 歧义切法都要查() {
        // xian 既可以是「先」也可以是「西安」
        let codes = segmenter().lookup_codes("xian");
        assert!(codes.contains(&"xian".to_string()), "{codes:?}");
        assert!(codes.contains(&"xi an".to_string()), "{codes:?}");
    }

    #[test]
    fn 只打一个字母要往后联想() {
        // n 本身是音节（嗯），但只打一个 n 时要出所有 n 开头的字
        let codes = segmenter().lookup_codes("n");
        assert!(
            codes.contains(&"n".to_string()),
            "精确匹配也要查：{codes:?}"
        );
        assert!(codes.contains(&"ni".to_string()), "{codes:?}");
        assert!(codes.contains(&"na".to_string()), "{codes:?}");
        // a 同理：啊/爱/安/昂 都该出来
        let codes = segmenter().lookup_codes("a");
        assert!(codes.contains(&"ai".to_string()), "{codes:?}");
        assert!(codes.contains(&"an".to_string()), "{codes:?}");
    }

    #[test]
    fn 单个完整音节不再往后联想() {
        // 打 ni 就该只查 ni，不能冒出「年/牛/您」
        assert_eq!(segmenter().lookup_codes("ni"), ["ni"]);
        // xian 同理：不能因为"xiang 以 xian 开头"就冒出一堆「想/香」
        let codes = segmenter().lookup_codes("xian");
        assert!(!codes.contains(&"xiang".to_string()), "{codes:?}");
    }

    #[test]
    fn 多音节输入会补全最后一个音节() {
        // shiji → 除了 shi ji，还要查 shi jian / shi jie，这样「时间」「世界」能提前出现
        let codes = segmenter().lookup_codes("shiji");
        assert!(codes.contains(&"shi ji".to_string()), "{codes:?}");
        assert!(codes.contains(&"shi jian".to_string()), "{codes:?}");
        assert!(codes.contains(&"shi jie".to_string()), "{codes:?}");
    }

    #[test]
    fn 半截音节会补全() {
        // nih：ni 切得动，h 切不动 → 补成 ni ha / ni hai / … / ni hao
        let codes = segmenter().lookup_codes("nih");
        assert!(codes.contains(&"ni hao".to_string()), "{codes:?}");
        assert!(codes.contains(&"ni ha".to_string()), "{codes:?}");
        assert!(codes.len() > 5);
    }

    #[test]
    fn 只打一个字母时候选来自所有同首字母音节() {
        let codes = segmenter().lookup_codes("n");
        assert!(codes.contains(&"ni".to_string()), "{codes:?}");
        assert!(codes.contains(&"na".to_string()), "{codes:?}");
        assert!(codes.contains(&"nuo".to_string()), "{codes:?}");
        assert!(codes.len() <= MAX_CODES);
    }

    #[test]
    fn 前缀码从长到短_最后到单字() {
        // nihaoma 的分段前缀：先「ni hao」(吃 5 个字符)，再「ni ha」(吃 4 个)，
        // 最后是单音节的「ni」(吃 2 个)—— 分词最低到单字
        let codes = segmenter().prefix_codes("nihaoma");
        assert_eq!(codes[0], ("ni hao".to_string(), 5), "{codes:?}");
        assert!(codes.contains(&("ni ha".to_string(), 4)), "{codes:?}");
        assert_eq!(codes.last(), Some(&("ni".to_string(), 2)), "{codes:?}");
        assert!(
            codes.windows(2).all(|pair| pair[0].1 > pair[1].1),
            "长的在前短的在后：{codes:?}"
        );
        // 整串本身不算"前缀"
        assert!(
            codes.iter().all(|(code, _)| code != "ni hao ma"),
            "{codes:?}"
        );
        // 尾巴是感叹词音节的也不切（`nihaom` 不该切出 `ni hao m`）
        assert!(
            segmenter()
                .prefix_codes("nihaom")
                .iter()
                .all(|(code, _)| !code.ends_with('m')),
            "{:?}",
            segmenter().prefix_codes("nihaom")
        );
    }

    #[test]
    fn 短输入不做分段() {
        // 一个字母：本来就是单字候选，没什么好分的
        assert!(segmenter().prefix_codes("n").is_empty());
        // nih：尾巴 h 切不动，但前面「ni」是完整音节 —— 单字分段还是给
        assert_eq!(segmenter().prefix_codes("nih"), [("ni".to_string(), 2)]);
    }

    #[test]
    fn 打错了就没有码() {
        // q 后面跟不出任何音节
        assert!(segmenter().lookup_codes("qqq").is_empty());
    }

    #[test]
    fn 认得音节前缀() {
        let segmenter = segmenter();
        // 拼音打到一半：还挂在某个音节上（`sho` 是 `shou` 的半截）
        for half in ["", "s", "sh", "sho", "zh", "zho", "ni", "xia", "n"] {
            assert!(segmenter.is_syllable_prefix(half), "{half:?} 该算音节前缀");
        }
        // 切不动、也不是任何音节的开头：这是英文（或者打错了）
        for other in ["nih", "hel", "hello", "v", "vv", "shou1"] {
            assert!(!segmenter.is_syllable_prefix(other), "{other:?} 不该算");
        }
    }

    #[test]
    fn 码的数量有上限() {
        // 长输入 + 歧义切分也不能把查询数炸掉
        for input in ["z", "zh", "sh", "zhongguo", "xianxianxian", "nihaonihao"] {
            let codes = segmenter().lookup_codes(input);
            assert!(codes.len() <= MAX_CODES, "{input}: {codes:?}");
        }
    }

    #[test]
    fn 三个音节以上也行() {
        assert_eq!(segmenter().lookup_codes("zhongguo")[0], "zhong guo");
        assert_eq!(segmenter().lookup_codes("beijing")[0], "bei jing");
    }
}
