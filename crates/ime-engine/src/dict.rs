//! 词表：想加词就往 `DICT` 里加一行。
//!
//! 每个拼音对应**一串**候选词，第一个是"主候选"（空格直接上屏的那个）。
//! key 一律写小写，查表时把输入转成小写再比 —— 这样大写锁定（Caps Lock）打出来的
//! `NIHAO` 也能转成「你好」，而查不到的输入原样返回、大小写照旧保留。
//!
//! 真词库（几万条 + 拼音切分 + 词频）以后换掉这个数组就行，接口不用动。

/// 拼音 → 候选词（第一个是主候选）
pub const DICT: &[(&str, &[&str])] = &[
    ("ni", &["你", "尼", "泥", "拟", "逆"]),
    ("hao", &["好", "号", "浩", "豪", "耗"]),
    ("nihao", &["你好", "尼好", "妮好", "拟好"]),
    ("wo", &["我", "握", "沃", "窝"]),
    ("ai", &["爱", "哎", "艾", "唉"]),
    ("xiexie", &["谢谢", "写写"]),
    ("beijing", &["北京", "背景", "北境"]),
    ("zhongguo", &["中国"]),
    ("shijie", &["世界", "事迹", "时节"]),
    ("zaijian", &["再见"]),
];

/// 输入 → 候选词列表（至少一个）。
///
/// * 查得到：词表里那一串
/// * 查不到：把用户打的原文当成唯一候选 —— 这样空格上屏的就是原文，
///   候选框里也能看见自己在打什么，不用为"没查到"单开一条分支
/// * 空输入：没有候选（也就没有候选框）
pub fn candidates(input: &str) -> Vec<String> {
    if input.is_empty() {
        return Vec::new();
    }
    let lower = input.to_ascii_lowercase();
    match DICT.iter().find(|(pinyin, _)| *pinyin == lower) {
        Some((_, words)) => words.iter().map(|word| (*word).to_string()).collect(),
        None => vec![input.to_string()],
    }
}

/// 主候选（= 候选列表的第一个）；查不到就是原文
pub fn convert(input: &str) -> String {
    candidates(input).into_iter().next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 查得到就转换() {
        assert_eq!(convert("nihao"), "你好");
    }

    #[test]
    fn 大小写不敏感() {
        assert_eq!(convert("NIHAO"), "你好");
        assert_eq!(convert("NiHao"), "你好");
    }

    #[test]
    fn 查不到原样返回() {
        assert_eq!(convert("aaaaAAAA"), "aaaaAAAA");
        assert_eq!(candidates("aaaaAAAA"), ["aaaaAAAA"]);
    }

    #[test]
    fn 一个拼音多个候选() {
        assert_eq!(candidates("nihao"), ["你好", "尼好", "妮好", "拟好"]);
        assert_eq!(candidates("nihao")[0], convert("nihao")); // 主候选永远排第一
    }

    #[test]
    fn 空输入没有候选() {
        assert!(candidates("").is_empty());
    }
}
