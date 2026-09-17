//! 配置文件：输入法的"功能"都在这里定义，改行为不用改代码。
//!
//! 放在 `~/.config/pliers/config.toml`（也认 `$XDG_CONFIG_HOME` 和 `$PLIERS_CONFIG`），
//! 没有这个文件就用默认值跑。完整示例见仓库根目录的 `pliers.example.toml`。
//!
//! ```toml
//! [scheme]
//! kind = "full-pinyin"          # full-pinyin | double-pinyin | table
//!
//! [dict]
//! path = "~/.local/share/pliers/dict.db"
//! max_candidates = 9
//! ```

use std::path::PathBuf;

use serde::Deserialize;

use crate::dict::{Dict, Result};
use crate::scheme::{DoublePinyin, FullPinyin, Layout, Scheme, Table};

/// 默认配置文件路径（找不到就用内置默认值）
pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PLIERS_CONFIG") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("pliers").join("config.toml")
}

/// 默认词库路径
pub fn default_dict_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local").join("share"));
    base.join("pliers").join("dict.db")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 把路径里的 `~` 展开（TOML 里写 `~/.local/...` 比写死 /home/xxx 舒服）
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub scheme: SchemeConfig,
    #[serde(default)]
    pub dict: DictConfig,
}

/// 用哪套输入方案。`kind` 决定后面跟哪些字段
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SchemeConfig {
    /// 全拼：`nihao` + 空格 → 你好
    #[default]
    FullPinyin,
    /// 双拼：两键一个音节，`layout` 选键位
    DoublePinyin {
        /// `natural`(自然码) / `flypy`(小鹤) / `mspy`(微软双拼)
        #[serde(default = "default_layout")]
        layout: String,
    },
    /// 码表方案：五笔、郑码、仓颉这类"键本身就是码"的
    Table {
        /// 词库里 `word.scheme` 用哪个名字（导入时 `--table-scheme` 指定的那个）
        name: String,
    },
}

fn default_layout() -> String {
    "natural".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct DictConfig {
    /// 词库文件（`pliers-dict` 导入出来的那个 SQLite）
    #[serde(default = "default_dict")]
    pub path: String,
    /// 候选框最多显示几个
    #[serde(default = "default_max_candidates")]
    pub max_candidates: usize,
}

impl Default for DictConfig {
    fn default() -> Self {
        Self {
            path: default_dict(),
            max_candidates: default_max_candidates(),
        }
    }
}

fn default_dict() -> String {
    default_dict_path().to_string_lossy().into_owned()
}

fn default_max_candidates() -> usize {
    9
}

impl Config {
    /// 找配置文件并读进来。没有配置文件是正常的 —— 用默认值
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读不了配置文件 {}：{e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("配置文件 {} 有问题：{e}", path.display()).into())
    }

    /// 解析一段 TOML（配置文件的正文）
    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// 词库文件路径（展开过 `~`）。
    /// `PLIERS_DICT` 环境变量优先级最高 —— 临时换个库看看效果时很方便
    pub fn dict_path(&self) -> PathBuf {
        match std::env::var_os("PLIERS_DICT") {
            Some(path) => PathBuf::from(path),
            None => expand(&self.dict.path),
        }
    }

    /// 按配置造一套输入方案
    pub fn build_scheme(&self, dict: &Dict) -> Result<Box<dyn Scheme>> {
        let syllables = dict.syllables();
        Ok(match &self.scheme {
            SchemeConfig::FullPinyin => Box::new(FullPinyin::new(syllables)),
            SchemeConfig::DoublePinyin { layout } => {
                let preset = Layout::preset(layout).ok_or_else(|| {
                    format!("不认识的双拼键位 {layout:?}（可选 natural / flypy / mspy）")
                })?;
                Box::new(DoublePinyin::new(preset, syllables))
            }
            SchemeConfig::Table { name } => Box::new(Table::new(name)),
        })
    }

    /// 打开词库 + 建好方案，一步到位
    pub fn build_engine_parts(&self) -> Result<(Dict, Box<dyn Scheme>)> {
        let dict = Dict::open(&self.dict_path())?;
        let scheme = self.build_scheme(&dict)?;
        Ok((dict, scheme))
    }
}

/// 一份带注释的配置模板（`pliers --init-config` 会写到配置文件路径）
pub const EXAMPLE: &str = r#"# pliers 配置
# 放在 ~/.config/pliers/config.toml 就会生效；删掉它就用默认值

[scheme]
# 用哪套输入方案：
#   full-pinyin    全拼（nihao → 你好）
#   double-pinyin  双拼（两键一个音节）
#   table          码表方案（五笔这类，需要自己导入码表）
kind = "full-pinyin"

# 双拼键位，只在 kind = "double-pinyin" 时看：
#   natural = 自然码   flypy = 小鹤双拼   mspy = 微软双拼
# layout = "natural"

# 码表方案的库内名字，只在 kind = "table" 时看：
# name = "wubi"

[dict]
# 词库文件（cargo run -p pliers-dict --release -- --help 看怎么生成）
# path = "~/.local/share/pliers/dict.db"
# 候选框最多显示几个
# max_candidates = 9
"#;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn 默认就是全拼() {
        let config = Config::parse("").unwrap();
        assert!(matches!(config.scheme, SchemeConfig::FullPinyin));
        assert_eq!(config.dict.max_candidates, 9);
    }

    #[test]
    fn 能配双拼() {
        let config = Config::parse(
            r#"
            [scheme]
            kind = "double-pinyin"
            layout = "flypy"

            [dict]
            max_candidates = 5
            path = "/tmp/x.db"
            "#,
        )
        .unwrap();
        match &config.scheme {
            SchemeConfig::DoublePinyin { layout } => assert_eq!(layout, "flypy"),
            other => panic!("{other:?}"),
        }
        assert_eq!(config.dict.max_candidates, 5);
        assert_eq!(config.dict_path(), Path::new("/tmp/x.db"));
    }

    #[test]
    fn 能配五笔码表() {
        let config = Config::parse("[scheme]\nkind = \"table\"\nname = \"wubi\"\n").unwrap();
        match &config.scheme {
            SchemeConfig::Table { name } => assert_eq!(name, "wubi"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn 波浪号会展开() {
        // SAFETY: 测试是单线程跑这一段
        unsafe { std::env::set_var("HOME", "/home/tester") };
        assert_eq!(expand("~/a/b"), Path::new("/home/tester/a/b"));
        assert_eq!(expand("/abs/path"), Path::new("/abs/path"));
    }

    #[test]
    fn 配置写错了要报错而不是默默用默认值() {
        // kind 拼错
        assert!(Config::parse("[scheme]\nkind = \"quanpin\"\n").is_err());
        // 双拼键位名字不认识（build_scheme 里会报错，这里先验预设表）
        let config =
            Config::parse("[scheme]\nkind = \"double-pinyin\"\nlayout = \"zi Ran Ma\"\n").unwrap();
        match &config.scheme {
            SchemeConfig::DoublePinyin { layout } => assert!(Layout::preset(layout).is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn 模板本身必须是合法配置() {
        let config = Config::parse(EXAMPLE).expect("示例配置要能解析");
        assert!(matches!(config.scheme, SchemeConfig::FullPinyin));
    }
}
