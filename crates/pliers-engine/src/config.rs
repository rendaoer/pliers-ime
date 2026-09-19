//! 配置文件：输入法的"功能"都在这里定义，改行为不用改代码。
//!
//! 放在 `~/.config/pliers/config.toml`（也认 `$XDG_CONFIG_HOME` 和 `$PLIERS_CONFIG`），
//! 没有这个文件就用默认值跑。模板就是同目录的 `config.example.toml`
//! （`pliers --init-config` 会把它写到那个路径）。
//!
//! ```toml
//! [scheme]
//! kind = "full-pinyin"          # full-pinyin | double-pinyin | wubi
//!
//! [dict]
//! path = "~/.local/share/pliers/dict.db"
//! max_candidates = 9
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::dict::{Dict, Result};
use crate::scheme::{CodeTable, DoublePinyin, FullPinyin, Layout, Scheme};
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
    data_dir().join("dict.db")
}

/// 默认的**用户数据**路径。跟词库放在同一个目录，但是**两个文件** ——
/// 词库是派生物（重新下载/重建会整个换掉），用户数据是你自己的
pub fn default_user_path() -> PathBuf {
    data_dir().join("user.db")
}

/// 默认的**英文词表**库路径。它也是个独立文件：可以单独更新（`pliers english fetch`），
/// 换词库、重装输入法都不影响它
pub fn default_english_path() -> PathBuf {
    data_dir().join("english.db")
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local").join("share"))
        .join("pliers")
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
    #[serde(default)]
    pub english: EnglishConfig,
    /// 上一次用过的双拼键位（`pliers set scheme.kind double-pinyin` 切回来时接着用）。
    /// `serde(skip)`：这只是运行期的记忆，配置文件里没这一项
    #[serde(skip)]
    pub last_double_pinyin: Option<(String, BTreeMap<String, String>)>,
}

/// 英文候选（打英文单词时的补全：`hel` + 空格 → `hello`）—— 见 [`crate::english`]
#[derive(Debug, Clone, Deserialize)]
pub struct EnglishConfig {
    /// 要不要给英文候选。关掉就只有中文候选，打英文得整串打完
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 词表库（SQLite：一张 `english(word, weight)` 表）。默认就是上面那个数据目录里的
    /// `english.db`，可以用 `pliers english fetch` 单独更新到新版本
    #[serde(default = "default_english")]
    pub path: String,
    /// 自己**额外**加的词（一行一个）：排在上面的词表前面
    /// （项目名、内部术语、词表里没有的怪词放这儿）。空 = 不加
    #[serde(default)]
    pub extra: String,
    /// 一次最多给几个英文候选
    #[serde(default = "default_english_limit")]
    pub limit: usize,
}

impl Default for EnglishConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_english(),
            extra: String::new(),
            limit: default_english_limit(),
        }
    }
}

fn default_english() -> String {
    default_english_path().to_string_lossy().into_owned()
}

fn default_english_limit() -> usize {
    5
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
    /// 中文标点：中文模式下把 `,` `.` `?` 这些打成全角 `，` `。` `？`
    #[serde(default = "default_true")]
    pub chinese_punctuation: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            toggle_keys: default_toggle_keys(),
            start_mode: Mode::default(),
            indicator: true,
            chinese_punctuation: true,
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
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SchemeConfig {
    /// 全拼：`nihao` + 空格 → 你好
    FullPinyin {
        /// 要不要"整句"候选：库里没有整词时按词拼一句（`nihaoma` → 你好吗）
        #[serde(default = "default_sentence")]
        sentence: bool,
    },
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
        /// 同全拼：要不要整句候选
        #[serde(default = "default_sentence")]
        sentence: bool,
    },
    /// 五笔（以及郑码、仓颉这类"键本身就是码"的码表方案）
    ///
    /// 配置里写 `kind = "wubi"`；郑码/仓颉也走这条，只是把 `name` 换成词库里那套码表的名字。
    /// 老配置里写的 `kind = "table"` 一样认（alias），但文档里不再这么叫了 ——
    /// "table" 看不出是五笔
    #[serde(alias = "table")]
    Wubi {
        /// 词库里 `word.scheme` 用哪个名字（导入时 `--table-scheme` 指定的那个）。
        /// 默认就是 "wubi"：`kind = "wubi"` 一行就够
        #[serde(default = "default_wubi_name")]
        name: String,
    },
}

impl Default for SchemeConfig {
    fn default() -> Self {
        // 全拼 + 整句候选（`#[default]` 不能标在带字段的变体上，只好手写）
        Self::FullPinyin { sentence: true }
    }
}

fn default_layout() -> String {
    "natural".to_string()
}

fn default_sentence() -> bool {
    true
}

fn default_wubi_name() -> String {
    "wubi".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct DictConfig {
    /// 词库文件（`pliers-dict` 导入出来的那个 SQLite）
    #[serde(default = "default_dict")]
    pub path: String,
    /// 用户数据文件（选过的词、自己拼的句子、拉黑的词）。
    /// 跟词库**分开存**：换词库不会丢它
    #[serde(default = "default_user")]
    pub user_path: String,
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
            user_path: default_user(),
            max_candidates: default_max_candidates(),
            pool_size: default_pool_size(),
        }
    }
}

fn default_dict() -> String {
    default_dict_path().to_string_lossy().into_owned()
}

fn default_user() -> String {
    default_user_path().to_string_lossy().into_owned()
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

    /// 用户数据文件路径（展开过 `~`）。`PLIERS_USER_DB` 优先级最高，跟词库一个规矩
    pub fn user_path(&self) -> PathBuf {
        match std::env::var_os("PLIERS_USER_DB") {
            Some(path) => PathBuf::from(path),
            None => expand(&self.dict.user_path),
        }
    }

    /// 英文词表文件路径（`PLIERS_ENGLISH` 优先，跟词库/用户数据一个规矩）
    pub fn english_path(&self) -> PathBuf {
        match std::env::var_os("PLIERS_ENGLISH") {
            Some(path) => PathBuf::from(path),
            None => expand(&self.english.path),
        }
    }

    /// 按配置造一套输入方案
    pub fn build_scheme(&self, dict: &Dict) -> Result<Box<dyn Scheme>> {
        let syllables = dict.syllables();
        Ok(match &self.scheme {
            SchemeConfig::FullPinyin { sentence } => {
                Box::new(FullPinyin::new(syllables, *sentence))
            }
            SchemeConfig::DoublePinyin {
                layout,
                keys,
                sentence,
            } => {
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
                Box::new(DoublePinyin::new(layout, syllables, *sentence))
            }
            SchemeConfig::Wubi { name } => Box::new(CodeTable::new(name)),
        })
    }

    /// 合成引擎设置：候选个数在 `[dict]` 里，切换键在 `[engine]` 里，英文候选在 `[english]` 里
    pub fn settings(&self) -> Result<Settings> {
        Ok(Settings {
            limit: self.dict.max_candidates,
            pool: self.dict.pool_size,
            chinese_punctuation: self.engine.chinese_punctuation,
            toggle_keys: ToggleKeys::parse(&self.engine.toggle_keys)?,
            start_mode: self.engine.start_mode,
            indicator: self.engine.indicator,
            english: self.english.enabled,
            english_path: (!self.english.path.is_empty()).then(|| self.english_path()),
            english_extra: (!self.english.extra.is_empty()).then(|| expand(&self.english.extra)),
            english_limit: self.english.limit,
        })
    }

    /// 打开词库（+ 用户数据）+ 建好方案，一步到位
    pub fn build_engine_parts(&self) -> Result<(Dict, Box<dyn Scheme>)> {
        let dict = Dict::open(&self.dict_path(), &self.user_path())?;
        let scheme = self.build_scheme(&dict)?;
        Ok((dict, scheme))
    }

    /// 改一项配置（`pliers set <项> <值>` 用），值是命令行上那一串原文。
    ///
    /// 只认下面这些"能在线改"的项，别的项得改配置文件 + `pliers reload`：
    ///
    /// | 项 | 值 |
    /// | --- | --- |
    /// | `scheme.kind` | `full-pinyin` / `double-pinyin` / `table` |
    /// | `scheme.layout` | `natural` / `flypy` / `mspy` / `none`（双拼键位） |
    /// | `scheme.sentence` | `true` / `false`（整句候选） |
    /// | `scheme.name` | 码表名（`kind = "wubi"` 时用，默认 wubi） |
    /// | `dict.path` | 词库文件 |
    /// | `dict.user_path` | 用户数据文件（选过的词/自己拼的句子/拉黑的词） |
    /// | `dict.max_candidates` | 1–9（数字键选词） |
    /// | `dict.pool_size` | 候选池深度 |
    /// | `engine.toggle_keys` | 逗号分隔，如 `ctrl+space,shift` |
    /// | `engine.start_mode` | `chinese` / `english` |
    /// | `engine.indicator` | `true` / `false` |
    /// | `engine.chinese_punctuation` | `true` / `false`（中文标点） |
    /// | `english.enabled` | `true` / `false`（英文候选） |
    /// | `english.limit` | 一次最多几个英文候选 |
    /// | `english.path` | 词表文件（默认 `~/.local/share/pliers/english.txt`） |
    /// | `english.extra` | 自己额外加的英文词（排在最前面） |
    ///
    /// 换 `kind` 时会把另一套方案的字段带过去（比如全拼 → 双拼保留原来的键位），
    /// 省得每换一次都要重设一遍。报错信息是给用户看的，所以都说人话
    pub fn set(&mut self, key: &str, value: &str) -> std::result::Result<(), String> {
        let bad = |what: &str| format!("{key} 只认 {what}，给的是 {value:?}");
        match key {
            "scheme.kind" => {
                let sentence = self.sentence();
                // 现在是双拼的话，把它这套键位记下来：切走再切回来不用重设
                if let SchemeConfig::DoublePinyin { layout, keys, .. } = &self.scheme {
                    self.last_double_pinyin = Some((layout.clone(), keys.clone()));
                }
                let (layout, keys) = self
                    .last_double_pinyin
                    .clone()
                    .unwrap_or_else(|| (default_layout(), BTreeMap::new()));
                self.scheme = match value {
                    "full-pinyin" => SchemeConfig::FullPinyin { sentence },
                    "double-pinyin" => SchemeConfig::DoublePinyin {
                        layout,
                        keys,
                        sentence,
                    },
                    // 五笔（郑码/仓颉也走这条）。老名字 "table" 一样认
                    "wubi" | "table" | "五笔" => {
                        // 码表名字：从别的方案切过来先默认 wubi，要改就用 scheme.name
                        let name = match &self.scheme {
                            SchemeConfig::Wubi { name } => name.clone(),
                            _ => default_wubi_name(),
                        };
                        SchemeConfig::Wubi { name }
                    }
                    _ => return Err(bad("full-pinyin / double-pinyin / wubi")),
                };
            }
            "scheme.layout" => {
                if Layout::preset(value).is_none() {
                    return Err(bad("natural（自然码）/ flypy（小鹤）/ mspy（微软）/ none"));
                }
                match &mut self.scheme {
                    SchemeConfig::DoublePinyin { layout, keys, .. } => {
                        *layout = value.to_string();
                        let remembered = (layout.clone(), keys.clone());
                        self.last_double_pinyin = Some(remembered);
                    }
                    _ => {
                        return Err(
                            "现在不是双拼方案。先 `pliers set scheme.kind double-pinyin`".into(),
                        );
                    }
                }
            }
            "scheme.sentence" => {
                let flag = parse_bool(value).ok_or_else(|| bad("true / false"))?;
                match &mut self.scheme {
                    SchemeConfig::FullPinyin { sentence }
                    | SchemeConfig::DoublePinyin { sentence, .. } => *sentence = flag,
                    SchemeConfig::Wubi { .. } => {
                        return Err("五笔是「键本身就是码」，没有整句候选".into());
                    }
                }
            }
            "scheme.name" => match &mut self.scheme {
                SchemeConfig::Wubi { name } => *name = value.to_string(),
                _ => return Err("scheme.name 只在五笔（kind = \"wubi\"）里有意义".into()),
            },
            "dict.path" => {
                if value.trim().is_empty() {
                    return Err("词库路径不能是空的".into());
                }
                self.dict.path = value.to_string();
            }
            "dict.user_path" => {
                if value.trim().is_empty() {
                    return Err("用户数据路径不能是空的（想清空用户数据就删那个文件）".into());
                }
                self.dict.user_path = value.to_string();
            }
            "dict.max_candidates" => {
                let count: usize = value
                    .parse()
                    .map_err(|_| bad("一个 1-9 的数字（数字键 1-9 选词）"))?;
                if !(1..=9).contains(&count) {
                    return Err(bad("1-9 之间的数字（数字键 1-9 选词）"));
                }
                self.dict.max_candidates = count;
            }
            "dict.pool_size" => {
                let size: usize = value.parse().map_err(|_| bad("一个正整数"))?;
                if size < self.dict.max_candidates {
                    return Err(format!(
                        "池子（{size}）得比一页（{}）大，不然翻页翻不动",
                        self.dict.max_candidates
                    ));
                }
                self.dict.pool_size = size;
            }
            "engine.toggle_keys" => {
                let keys: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                    .map(str::to_string)
                    .collect();
                ToggleKeys::parse(&keys)?; // 先验一遍，报错信息它写得更清楚
                self.engine.toggle_keys = keys;
            }
            "engine.start_mode" => {
                self.engine.start_mode = match value {
                    "chinese" | "中" | "中文" => Mode::Chinese,
                    "english" | "英" | "英文" => Mode::English,
                    _ => return Err(bad("chinese / english")),
                };
            }
            "engine.indicator" => {
                self.engine.indicator = parse_bool(value).ok_or_else(|| bad("true / false"))?;
            }
            "engine.chinese_punctuation" => {
                self.engine.chinese_punctuation =
                    parse_bool(value).ok_or_else(|| bad("true / false"))?;
            }
            "english.enabled" => {
                self.english.enabled = parse_bool(value).ok_or_else(|| bad("true / false"))?;
            }
            "english.limit" => {
                let count: usize = value.parse().map_err(|_| bad("一个正整数"))?;
                if count == 0 || count > 9 {
                    return Err(bad("1-9 之间的数字（英文候选一次给几个）"));
                }
                self.english.limit = count;
            }
            "english.path" => {
                if value.trim().is_empty() {
                    return Err("英文词表路径不能是空的（想清掉就删那个文件）".into());
                }
                self.english.path = value.trim().to_string();
            }
            "english.extra" => {
                // 空串是合法的：回到"不加自己的词"
                self.english.extra = value.trim().to_string();
            }
            other => {
                return Err(format!(
                    "不认识的配置项 {other:?}（能改的：scheme.kind / scheme.layout / \
                     scheme.sentence / scheme.name / dict.path / dict.user_path / \
                     dict.max_candidates / dict.pool_size / engine.toggle_keys / \
                     engine.start_mode / engine.indicator / \
                     engine.chinese_punctuation / english.enabled / english.limit /
                     english.path / english.extra）"
                ));
            }
        }
        Ok(())
    }

    /// 现在这套方案要不要整句候选（换 kind 时把它带过去）
    fn sentence(&self) -> bool {
        match &self.scheme {
            SchemeConfig::FullPinyin { sentence } | SchemeConfig::DoublePinyin { sentence, .. } => {
                *sentence
            }
            SchemeConfig::Wubi { .. } => false,
        }
    }
}

/// 命令行上的 true/false（也认 1/0、on/off、yes/no，敲着顺手）
fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" | "开" => Some(true),
        "false" | "0" | "off" | "no" | "关" => Some(false),
        _ => None,
    }
}

/// 把一项配置**写回配置文件**（`pliers config set` 用）。
///
/// 用 `toml_edit` 而不是"读出结构再整个序列化"：注释、空行、自己排的顺序都留着 ——
/// 手写的配置文件被工具洗一遍是最气人的。写之前先拿 [`Config::set`] 验一遍，
/// 所以不存在"写进去一个跑不起来的配置"
pub fn write_setting(path: &Path, key: &str, value: &str) -> std::result::Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读不了 {}：{e}", path.display())),
    };

    // 先验值：解析失败 / 值不合法都在这儿拦住
    let mut check = if text.trim().is_empty() {
        Config::default()
    } else {
        Config::parse(&text).map_err(|e| format!("现在的配置文件有问题，先修好它：{e}"))?
    };
    check.set(key, value)?;

    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("现在的配置文件解析不了：{e}"))?;
    let (section, name) = key
        .split_once('.')
        .ok_or_else(|| format!("配置项要写成 `[段].名字`，比如 scheme.kind，给的是 {key:?}"))?;
    if doc.get(section).is_none() {
        doc[section] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let table = doc[section]
        .as_table_mut()
        .ok_or_else(|| format!("配置文件里的 [{section}] 不是一张表"))?;
    // 换值的时候把原来那行的"装饰"接过来 —— 行尾注释、缩进都挂在 Value 的 decor 上，
    // 不接的话 `kind = "full-pinyin"   # 说明` 里的注释就没了
    let mut item = toml_value(key, value);
    if let (Some(old), Some(new)) = (table.get(name), item.as_value_mut())
        && let Some(old) = old.as_value()
    {
        *new.decor_mut() = old.decor().clone();
    }
    table[name] = item;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了目录 {}：{e}", dir.display()))?;
    }
    std::fs::write(path, doc.to_string()).map_err(|e| format!("写不了 {}：{e}", path.display()))?;
    Ok(())
}

/// 值按配置项的类型写：数字写成整数、开关写成 true/false、切换键写成数组，别的都是字符串
fn toml_value(key: &str, value: &str) -> toml_edit::Item {
    match key {
        "dict.max_candidates" | "dict.pool_size" | "english.limit" => match value.parse::<i64>() {
            Ok(number) => toml_edit::value(number),
            Err(_) => toml_edit::value(value),
        },
        "scheme.sentence"
        | "engine.indicator"
        | "engine.chinese_punctuation"
        | "english.enabled" => match parse_bool(value) {
            Some(flag) => toml_edit::value(flag),
            None => toml_edit::value(value),
        },
        "engine.toggle_keys" => {
            let mut array = toml_edit::Array::new();
            for key in value.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                array.push(key);
            }
            toml_edit::value(array)
        }
        _ => toml_edit::value(value),
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
    fn 在线改方案_换_kind_会带走键位和整句开关() {
        let mut config = Config::default();
        config.set("scheme.kind", "double-pinyin").unwrap();
        config.set("scheme.layout", "flypy").unwrap();
        config.set("scheme.sentence", "false").unwrap();
        assert!(matches!(
            &config.scheme,
            SchemeConfig::DoublePinyin { layout, sentence, .. } if layout == "flypy" && !*sentence
        ));

        // 切到全拼再切回双拼：键位和整句开关都还在，不用重设
        config.set("scheme.kind", "full-pinyin").unwrap();
        assert!(matches!(
            config.scheme,
            SchemeConfig::FullPinyin { sentence: false }
        ));
        config.set("scheme.kind", "double-pinyin").unwrap();
        assert!(matches!(
            &config.scheme,
            SchemeConfig::DoublePinyin { layout, sentence, .. } if layout == "flypy" && !*sentence
        ));
    }

    #[test]
    fn 在线改配置_值不对就报错而且不改动() {
        let mut config = Config::default();
        assert!(
            config
                .set("scheme.layout", "flpy")
                .unwrap_err()
                .contains("flypy")
        );
        assert!(config.set("dict.max_candidates", "99").is_err());
        assert!(config.set("dict.max_candidates", "x").is_err());
        assert!(
            config.set("dict.pool_size", "3").is_err(),
            "池子比一页小，翻页就翻不动了"
        );
        assert!(config.set("engine.toggle_keys", "ctrl+alt").is_err());
        assert!(config.set("engine.indicator", "maybe").is_err());
        assert!(config.set("engine.start_mode", "火星文").is_err());
        assert!(
            config
                .set("nosuch.key", "1")
                .unwrap_err()
                .contains("不认识的配置项")
        );
        // 报错的那几项都没生效
        assert_eq!(config.dict.max_candidates, 9);
        assert_eq!(config.dict.pool_size, 90);
    }

    #[test]
    fn 在线改配置_词库和开关() {
        let mut config = Config::default();
        config.set("dict.path", "/tmp/别的库.db").unwrap();
        config.set("dict.max_candidates", "5").unwrap();
        config.set("dict.pool_size", "50").unwrap();
        config
            .set("engine.toggle_keys", "ctrl+space, shift")
            .unwrap();
        config.set("engine.indicator", "off").unwrap();
        config.set("engine.chinese_punctuation", "off").unwrap();
        config.set("engine.start_mode", "english").unwrap();
        assert_eq!(config.dict.path, "/tmp/别的库.db");
        assert_eq!(config.dict.max_candidates, 5);
        assert_eq!(config.dict.pool_size, 50);
        assert_eq!(config.engine.toggle_keys, ["ctrl+space", "shift"]);
        assert!(!config.engine.indicator);
        assert!(!config.engine.chinese_punctuation);
        assert_eq!(config.engine.start_mode, Mode::English);
    }

    #[test]
    fn 在线改配置_英文候选() {
        let mut config = Config::default();
        // 默认是开着的，一次 5 个，词表指向数据目录里那个文件
        let settings = config.settings().unwrap();
        assert!(settings.english);
        assert_eq!(settings.english_limit, 5);
        assert_eq!(
            settings.english_path.unwrap(),
            default_english_path(),
            "默认词表是独立那个文件"
        );
        assert!(settings.english_extra.is_none());

        config.set("english.enabled", "off").unwrap();
        config.set("english.limit", "3").unwrap();
        config.set("english.path", "~/words.txt").unwrap();
        config.set("english.extra", "~/my-words.txt").unwrap();
        let settings = config.settings().unwrap();
        assert!(!settings.english);
        assert_eq!(settings.english_limit, 3);
        // `~` 要展开（词表路径跟词库路径一个规矩）
        for path in [
            settings.english_path.clone().unwrap(),
            settings.english_extra.clone().unwrap(),
        ] {
            assert!(!path.to_string_lossy().starts_with('~'), "{path:?}");
        }

        // 词表路径不能空（想清掉就删文件）；自己加词那份清空 = 不加
        assert!(config.set("english.path", "").is_err());
        config.set("english.extra", "").unwrap();
        assert!(config.settings().unwrap().english_extra.is_none());

        // 个数得是 1-9
        assert!(config.set("english.limit", "0").is_err());
        assert!(config.set("english.limit", "99").is_err());
        assert_eq!(config.english.limit, 3, "报错了就不该改动");
    }

    #[test]
    fn 写回配置文件_注释和别的项都留着() {
        let path = std::env::temp_dir().join(format!(
            "pliers-config-test-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(
            &path,
            "# 我手写的注释\n[scheme]\nkind = \"full-pinyin\"   # 行尾也要留着\n\n[dict]\nmax_candidates = 9\n",
        )
        .unwrap();

        write_setting(&path, "scheme.kind", "double-pinyin").unwrap();
        write_setting(&path, "scheme.layout", "flypy").unwrap();
        write_setting(&path, "dict.max_candidates", "5").unwrap();
        write_setting(&path, "engine.toggle_keys", "ctrl+space,shift").unwrap();
        // 英文候选这几项：开关要写成 true/false、个数要写成整数（写成字符串下次就读不了）
        write_setting(&path, "english.enabled", "false").unwrap();
        write_setting(&path, "english.limit", "3").unwrap();
        write_setting(&path, "english.path", "~/words.txt").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 我手写的注释"), "注释没了：\n{text}");
        assert!(text.contains("# 行尾也要留着"), "行尾注释没了：\n{text}");
        let config = Config::parse(&text).unwrap();
        assert!(matches!(
            &config.scheme,
            SchemeConfig::DoublePinyin { layout, .. } if layout == "flypy"
        ));
        assert_eq!(config.dict.max_candidates, 5);
        assert_eq!(config.engine.toggle_keys, ["ctrl+space", "shift"]);
        assert!(!config.english.enabled);
        assert_eq!(config.english.limit, 3);
        assert_eq!(config.english.path, "~/words.txt");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn 写回配置文件_值和项都要先验过() {
        let path = std::env::temp_dir().join(format!(
            "pliers-config-test-bad-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        assert!(write_setting(&path, "scheme.layout", "flpy").is_err());
        assert!(write_setting(&path, "nosuch.key", "1").is_err());
        assert!(!path.exists(), "验不过就不该写文件");
    }

    #[test]
    fn 在线改配置_码表才有名字() {
        let mut config = Config::default();
        assert!(config.set("scheme.name", "wubi").is_err(), "全拼没有码表名");
        config.set("scheme.kind", "wubi").unwrap();
        assert!(matches!(&config.scheme, SchemeConfig::Wubi { name } if name == "wubi"));
        config.set("scheme.name", "cangjie").unwrap();
        assert!(matches!(&config.scheme, SchemeConfig::Wubi { name } if name == "cangjie"));
        assert!(
            config.set("scheme.layout", "flypy").is_err(),
            "码表没有键位"
        );
    }

    #[test]
    fn 默认就是全拼() {
        let config = Config::parse("").unwrap();
        assert!(matches!(config.scheme, SchemeConfig::FullPinyin { .. }));
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
            SchemeConfig::Wubi { name } => assert_eq!(name, "wubi"),
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
        assert!(matches!(config.scheme, SchemeConfig::FullPinyin { .. }));
    }
}
