//! 命令行参数：`pliers <动词> [种类] [位置参数] [--选项]` 这套动词共用一份解析。
//!
//! 四个动词（`status` / `path` / `build` / `fetch`）收的是**同一个种类词表**
//!（pinyin / wubi / english），所以"哪个是种类"这件事只在这里判断一次 ——
//! 以前每个子命令各写一份，结果同一个东西三种说法。

/// 带值的选项：`--url X` / `--scheme X` 里的 `X` 不是位置参数
///（`--url=X` 只有一个词，不用管）
const VALUE_FLAGS: &[&str] = &["url", "scheme"];

/// 位置参数：种类、文件名这些，跳过所有 `--选项`（连带它的值）
pub fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if let Some(flag) = arg.strip_prefix("--") {
            if !flag.contains('=') && VALUE_FLAGS.contains(&flag) {
                rest.next();
            }
            continue;
        }
        out.push(arg.as_str());
    }
    out
}

/// 第 `index` 个位置参数。0 = 种类，1 = 后面的文件名（`pliers build english 我的词表.txt`）
pub fn at(args: &[String], index: usize) -> Option<&str> {
    positional(args).into_iter().nth(index)
}

/// 种类（没写就是空串，意思是"默认那个 / 整套"）
pub fn kind(args: &[String]) -> &str {
    at(args, 0).unwrap_or("")
}

/// `--flag` 在不在
pub fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

/// `--flag 值` / `--flag=值` 都能写
pub fn value_of(args: &[String], flag: &str) -> Option<String> {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == flag {
            return rest.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 种类参数认得出来() {
        assert_eq!(kind(&args(&["english"])), "english");
        assert_eq!(kind(&args(&["--force", "english"])), "english");
        assert_eq!(kind(&args(&["english", "--force"])), "english");
        // `--url` 后面那个是它的值，不是种类
        assert_eq!(kind(&args(&["--url", "http://x/y.zst"])), "");
        assert_eq!(kind(&args(&["--url=http://x/y.zst"])), "");
        assert_eq!(kind(&args(&["--url", "http://x/y.zst", "all"])), "all");
        assert_eq!(kind(&args(&["--force"])), "");
        assert_eq!(kind(&[]), "");
    }

    #[test]
    fn 第二个位置参数是文件名() {
        assert_eq!(
            at(&args(&["english", "我的词表.txt"]), 1),
            Some("我的词表.txt")
        );
        // 选项混在中间也不影响顺序
        assert_eq!(
            at(&args(&["english", "--force", "我的词表.txt"]), 1),
            Some("我的词表.txt")
        );
        assert_eq!(at(&args(&["english"]), 1), None);
    }

    #[test]
    fn 带值的选项两种写法() {
        assert_eq!(
            value_of(&args(&["--url", "http://x"]), "--url").as_deref(),
            Some("http://x")
        );
        assert_eq!(
            value_of(&args(&["--url=http://x"]), "--url").as_deref(),
            Some("http://x")
        );
        assert_eq!(value_of(&args(&["--force"]), "--url"), None);
        assert!(has(&args(&["--force", "english"]), "--force"));
        assert!(!has(&args(&["english"]), "--force"));
    }
}
