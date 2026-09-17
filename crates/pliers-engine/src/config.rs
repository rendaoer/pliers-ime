//! 配置文件：输入法的"功能"都在这里定义，改行为不用改代码。
//!
//! 放在 `~/.config/pliers/config.toml`（也认 `$XDG_CONFIG_HOME` 和 `$PLIERS_CONFIG`），
//! 没有这个文件就用默认值跑。模板就是同目录的 `config.example.toml`
//! （`pliers --init-config` 会把它写到那个路径）。
//!
//! ```toml
//! [scheme]
//! kind = "full-pinyin"          # full-pinyin | double-pinyin | table
//!
//! [dict]
//! path = "~/.local/share/pliers/dict.db"
//! max_candidates = 9
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::dict::{Dict, Result};
use crate::scheme::{DoublePinyin, FullPinyin, Layout, Scheme, Table};
use crate::{Mode, Settings, ToggleKeys};

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
    #[serde(default)]
    pub engine: EngineConfig,
}

/// 引擎行为：中英文怎么切之类
#[derive(Debug, Clone, Deserialize)]
pub struct EngineConfig {
    /// 中英文切换键，可以写多个：`"ctrl+space"`、`"shift"`。空数组 = 不要切换键
    #[serde(default = "default_toggle_keys")]
    pub toggle_keys: Vec<String>,
    /// 启动时用哪种模式：`"chinese"` / `"english"`
    #[serde(default)]
    pub start_mode: Mode,
    /// 切换时在光标处弹一下「中」/「英」
    #[serde(default = "default_true")]
    pub indicator: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            toggle_keys: default_toggle_keys(),
            start_mode: Mode::default(),
            indicator: true,
        }
    }
}

fn default_toggle_keys() -> Vec<String> {
    vec!["ctrl+space".to_string()]
}

fn default_true() -> bool {
    true
}

/// 用哪套输入方案。`kind` 决定后面跟哪些字段
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SchemeConfig {
    /// 全拼：`nihao` + 空格 → 你好
    #[default]
    FullPinyin,
    /// 双拼：两键一个音节
    DoublePinyin {
        /// 预设键位：`natural`(自然码) / `flypy`(小鹤) / `mspy`(微软双拼) /
        /// `none`（空表，全靠下面的 `keys` 自己填）
        #[serde(default = "default_layout")]
        layout: String,
        /// 自定义键位。写在预设**之上**：改一个键就只写那一个；
        /// `layout = "none"` 时这就是全部键位。
        /// 键名 `zh`/`ch`/`sh` 是声母，别的都当韵母
        #[serde(default)]
        keys: BTreeMap<String, String>,
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
    /// 候选框一页显示几个
    #[serde(default = "default_max_candidates")]
    pub max_candidates: usize,
    /// 一次准备多少个候选 —— 翻页能翻多深就靠它
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,
}

impl Default for DictConfig {
    fn default() -> Self {
        Self {
            path: default_dict(),
            max_candidates: default_max_candidates(),
            pool_size: default_pool_size(),
        }
    }
}

fn default_dict() -> String {
    default_dict_path().to_string_lossy().into_owned()
}

fn default_max_candidates() -> usize {
    9
}

fn default_pool_size() -> usize {
    90
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
            SchemeConfig::DoublePinyin { layout, keys } => {
                let base = if layout == "none" {
                    Layout::empty()
                } else {
                    Layout::preset(layout).ok_or_else(|| {
                        format!("不认识的双拼键位 {layout:?}（可选 natural / flypy / mspy / none）")
                    })?
                };
                let layout = base.with_keys(keys)?;
                // 键位漏写一个韵母，表现是"有些字怎么打都打不出来"，特别难查 ——
                // 所以在启动时就把编不出来的音节列出来
                let missing = layout.missing_finals(syllables);
                if !missing.is_empty() {
                    // 只报"缺哪个韵母"，别把三百个音节甩出来
                    let shown: Vec<&str> = missing.iter().take(12).map(String::as_str).collect();
                    let more = if missing.len() > shown.len() {
                        format!(" 等 {} 个", missing.len())
                    } else {
                        String::new()
                    };
                    return Err(format!(
                        "这套双拼键位缺韵母：{}{more}（把它们补进 [scheme.keys]，比如 `{} = \"x\"`）",
                        shown.join(" "),
                        shown.first().copied().unwrap_or("ao")
                    )
                    .into());
                }
                Box::new(DoublePinyin::new(layout, syllables))
            }
            SchemeConfig::Table { name } => Box::new(Table::new(name)),
        })
    }

    /// 合成引擎设置：候选个数在 `[dict]` 里，切换键在 `[engine]` 里
    pub fn settings(&self) -> Result<Settings> {
        Ok(Settings {
            limit: self.dict.max_candidates,
            pool: self.dict.pool_size,
            toggle_keys: ToggleKeys::parse(&self.engine.toggle_keys)?,
            start_mode: self.engine.start_mode,
            indicator: self.engine.indicator,
        })
    }

    /// 打开词库 + 建好方案，一步到位
    pub fn build_engine_parts(&self) -> Result<(Dict, Box<dyn Scheme>)> {
        let dict = Dict::open(&self.dict_path())?;
        let scheme = self.build_scheme(&dict)?;
        Ok((dict, scheme))
    }
}

/// 一份带注释的配置模板。
///
/// 就是 `crates/pliers-engine/config.example.toml` 那个文件本身 ——
/// 直接嵌进来，免得"代码里一份、仓库里一份"两边写着写着就不一样了。
/// `pliers --init-config` 会把它写到 [`config_path`]。
pub const EXAMPLE: &str = include_str!("../config.example.toml");

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
            SchemeConfig::DoublePinyin { layout, .. } => assert_eq!(layout, "flypy"),
            other => panic!("{other:?}"),
        }
        assert_eq!(config.dict.max_candidates, 5);
        assert_eq!(config.dict_path(), Path::new("/tmp/x.db"));
    }

    #[test]
    fn 双拼键位可以自己加() {
        let dict = crate::dict::testing::sample_dict();
        let config = Config::parse(
            r#"
            [scheme]
            kind = "double-pinyin"
            layout = "natural"

            [scheme.keys]
            ao = "c"
            "#,
        )
        .unwrap();
        match &config.scheme {
            SchemeConfig::DoublePinyin { keys, .. } => assert_eq!(keys["ao"], "c"),
            other => panic!("{other:?}"),
        }
        // 能建出来就说明键位是完整的（不完整会报错）
        assert!(config.build_scheme(&dict).is_ok());
    }

    #[test]
    fn 键位从零写也行() {
        let dict = crate::dict::testing::sample_dict();
        // 一套完整的自定义键位（这里直接抄自然码，证明"从零写"这条路是通的）
        let mut keys = String::new();
        for (finals, key) in [
            ("zh", 'v'),
            ("ch", 'i'),
            ("sh", 'u'),
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
        ] {
            keys.push_str(&format!("{finals} = \"{key}\"\n"));
        }
        let config = Config::parse(&format!(
            "[scheme]\nkind = \"double-pinyin\"\nlayout = \"none\"\n[scheme.keys]\n{keys}"
        ))
        .unwrap();
        assert!(config.build_scheme(&dict).is_ok());
    }

    #[test]
    fn 键位漏了韵母会在启动时说不清哪里错() {
        let dict = crate::dict::testing::sample_dict();
        let config = Config::parse(
            "[scheme]\nkind = \"double-pinyin\"\nlayout = \"none\"\n[scheme.keys]\nzh = \"v\"\n",
        )
        .unwrap();
        let message = match config.build_scheme(&dict) {
            Ok(_) => panic!("键位不完整就该报错"),
            Err(e) => e.to_string(),
        };
        assert!(message.contains("缺韵母"), "{message}");
        assert!(
            message.contains("scheme.keys"),
            "错误信息要指出改哪儿：{message}"
        );
        assert!(
            message.contains("ai") || message.contains("ao"),
            "要说清缺哪个：{message}"
        );
    }

    #[test]
    fn 切换键和初始模式可以配() {
        // 默认：Ctrl+空格，中文
        let settings = Config::parse("").unwrap().settings().unwrap();
        assert!(settings.toggle_keys.ctrl_space);
        assert!(!settings.toggle_keys.shift_tap);
        assert_eq!(settings.start_mode, Mode::Chinese);
        assert!(settings.indicator);

        // 两个都要
        let config =
            Config::parse("[engine]\ntoggle_keys = [\"ctrl+space\", \"shift\"]\n").unwrap();
        let settings = config.settings().unwrap();
        assert!(settings.toggle_keys.ctrl_space && settings.toggle_keys.shift_tap);

        // 空数组 = 不要切换键
        let config = Config::parse("[engine]\ntoggle_keys = []\n").unwrap();
        assert!(!config.settings().unwrap().toggle_keys.enabled());

        // 启动就是英文 + 不要提示
        let config =
            Config::parse("[engine]\nstart_mode = \"english\"\nindicator = false\n").unwrap();
        let settings = config.settings().unwrap();
        assert_eq!(settings.start_mode, Mode::English);
        assert!(!settings.indicator);
    }

    #[test]
    fn 切换键写错了要报错() {
        let config = Config::parse("[engine]\ntoggle_keys = [\"ctrl+alt+del\"]\n").unwrap();
        let message = match config.settings() {
            Ok(_) => panic!("不认识的切换键该报错"),
            Err(e) => e.to_string(),
        };
        assert!(
            message.contains("ctrl+space"),
            "错误信息该说清能写什么：{message}"
        );

        // start_mode 只认 chinese / english
        assert!(Config::parse("[engine]\nstart_mode = \"zh\"\n").is_err());
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
            SchemeConfig::DoublePinyin { layout, .. } => assert!(Layout::preset(layout).is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn 模板本身必须是合法配置() {
        let config = Config::parse(EXAMPLE).expect("仓库里那份示例配置要能解析");
        assert!(matches!(config.scheme, SchemeConfig::FullPinyin));
    }
}
