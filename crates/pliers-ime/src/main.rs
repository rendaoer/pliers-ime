//! 最小输入法：组装"引擎"和"协议层"。
//!
//! * `pliers-engine`  —— 按键该干什么 + 输入方案（全拼/双拼/码表）+ SQLite 词库
//! * `pliers-popup`   —— 候选框长什么样（找字体、排版、画像素）
//! * `pliers-wayland` —— 跟合成器说协议，把引擎的决定变成 Wayland 请求
//!
//! 行为都在配置文件里（`~/.config/pliers/config.toml`），代码不用改：
//! 换全拼/双拼/五笔、换词库、改候选个数，都是改 TOML。
//!
//! 命令行是**动词在前、种类在后**：`pliers status|path|build|fetch [pinyin|wubi|english]`，
//! 种类只有那三个（字典是个总概念）。跑起来之后还能隔着一条 Unix socket 遥控它
//!（`pliers status` / `reload` / `set scheme.kind double-pinyin`）—— 见 `pliers --help`。

mod args;
mod dict;
mod english;
mod fetch;
mod pick;

use std::io::{IsTerminal, Write};
use std::path::Path;

use pick::{Picker, RawMode};
use pliers_engine::{Config, Engine, Mode};
use pliers_wayland::Options;

fn help() -> String {
    format!(
        "pliers —— 从零手写的 Wayland 中文输入法\n\n\
         用法：pliers [动词] [种类] [选项]        （不带动词 = 启动输入法）\n\n\
         \x20 字典是个总概念，按方案分成几种，**各有各的文件**：\n\
         \x20 pinyin（拼音词库 dict.db）/ wubi（码表库 wubi.db，五笔/郑码/仓颉）/ english（英文词表）。\n\
         \x20 下面四个动词收的都是这几个名字：\n\
         \x20 pliers status [种类]        看现状：不给种类 = 实例 + 所有库，给了 = 只看那一块\n\
         \x20 pliers path   [名字]        文件都在哪（config / dict / wubi / user / english / corpus / socket）\n\
         \x20 pliers build  <种类> [文件]  自己构建（要 pliers-dict：pinyin 下语料，wubi 用自己的码表，\n\
         \x20                            english 可以给一份自己的词表）\n\
         \x20 pliers fetch  <种类|all>    从 Release 下载预构建的（--force 重装、--url 换源）\n\
         \x20 pliers update              哪个不是最新就更新哪个（已经一样就跳过，不白下载）\n\n\
         \x20 装一份能用的：\n\
         \x20 pliers --init [--force] [--build]  写配置 + 装字典（--build = 自己下语料构建）\n\
         \x20 pliers --init-config [--force]     只写配置模板\n\n\
         \x20 配置：改配置文件的（config …）和只改内存的（set …）是两条路 —— 前者留下来的\n\
         \x20 东西下次启动还在，后者重启即失效：试手感用 set，定下来用 config set。\n\
         \x20 pliers config set <项> <值>   写配置文件（保留注释；跑着的实例会自动重读）\n\
         \x20 pliers config set           交互模式：上下选着改（↑↓ + Enter）\n\
         \x20 pliers config show|edit     看配置文件 / 用 $EDITOR 打开\n\
         \x20 pliers set <项> <值>         只改**正在跑的实例**（下次启动就回去了）\n\
         \x20 pliers set                  交互模式：上下选着改\n\
         \x20 pliers reload               让它重新读一遍配置文件\n\n\
         能 set 的项：\n\
         \x20 scheme.kind           full-pinyin | double-pinyin | wubi\n\
         \x20 scheme.layout         natural | flypy | mspy | none   （双拼键位）\n\
         \x20 scheme.sentence       true | false                    （整句候选）\n\
         \x20 scheme.name           <码表名>                        （kind = wubi 时，默认 wubi）\n\
         \x20 dict.path             <拼音词库文件>                  （dict.db）\n\
         \x20 dict.wubi_path        <码表库文件>                    （wubi.db，五笔/郑码/仓颉）\n\
         \x20 dict.user_path        <用户数据文件>                  （选过的词/自己拼的句子；两个库共用）\n\
         \x20 dict.max_candidates   1-9\n\
         \x20 dict.pool_size        <正整数>\n\
         \x20 engine.toggle_keys    ctrl+space,shift                （逗号分隔）\n\
         \x20 engine.start_mode     chinese | english\n\
         \x20 engine.indicator      true | false\n\
         \x20 engine.chinese_punctuation  true | false               （中文标点）\n\
         \x20 english.enabled       true | false                     （英文候选）\n\
         \x20 english.limit         1-9                              （一次几个英文候选）\n\
         \x20 english.path          <词表文件>                        （默认 ~/.local/share/pliers/english.db）\n\
         \x20 english.extra         <自己加的词文件>                  （排在最前面）\n\n\
         路径：pliers path 一屏看全，默认是：\n\
         {}\n\
         {}\n\
         {}\n\
         {}\n\
         调试：PLIERS_DEBUG=1 pliers 把每个按键的判定打到 stderr\n\
         遥控：命令走 Unix socket，路径看 $PLIERS_SOCKET（默认 $XDG_RUNTIME_DIR/pliers.sock）",
        row("配置", pliers_engine::config::config_path().display()),
        row(
            "拼音词库",
            format!(
                "{}（pliers status pinyin 看里面有什么）",
                pliers_engine::config::default_dict_path().display()
            )
        ),
        row(
            "码表库",
            format!(
                "{}（五笔/郑码/仓颉；pliers build wubi <码表.txt> 建一份）",
                pliers_engine::config::default_wubi_path().display()
            )
        ),
        row(
            "英文词表",
            format!(
                "{}（pliers status english 看现在用的是哪份）",
                pliers_engine::config::default_english_path().display()
            )
        )
    )
}

fn main() {
    if let Err(error) = run() {
        // 自己打错误信息比 `Error: "..."` 好看，也不带 Debug 的引号
        eprintln!("pliers: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // 装一份能用的：写配置 + 装字典（新机器就这一条命令）
        Some("--init") => return dict::init(&args[1..]),
        // 写一份带注释的配置模板，省得手敲（已存在就不覆盖）
        Some("--init-config") => return init_config(args.iter().any(|arg| arg == "--force")),
        // 字典：动词在前、种类（pinyin|wubi|english）在后 —— 下面这五条是一套说法
        // 看现状：不给种类 = 正在跑的实例 + 所有字典
        Some("status") => return status_command(&args[1..]),
        // 文件都在哪（配置文件、两种字典、用户数据、语料、socket）
        Some("path") => return path_command(&args[1..]),
        // 自己构建（pinyin 下语料调 pliers-dict；english 从词表或兜底那份）
        Some("build") => return build_command(&args[1..]),
        // 从 Release 拿预构建的（每种一个资产，能单独更新）
        Some("fetch") => return fetch::command(&args[1..]),
        // 哪个不是最新就更新哪个
        Some("update") => return fetch::update(&args[1..]),
        // 以前是名词在前（pliers dict status / pliers english fetch），现在只留一句"该写什么"
        Some("dict") => return Err(retired("pliers dict", &args[1..])),
        Some("english") => return Err(retired("pliers english", &args[1..])),
        Some("--help" | "-h") => {
            println!("{}", help());
            return Ok(());
        }
        // 遥控正在跑的那个实例。注意这里**不**碰合成器：
        // 起第二个输入法会把 seat 抢走，正在打字的人就被顶掉了
        Some("reload") if args.len() == 1 => {
            return remote(
                "reload",
                "没在跑就不用 reload —— 它启动时自然读配置（想改配置用 pliers config set）",
            );
        }
        // 不带参数 = 交互模式：上下选着改
        Some("set") if args.len() == 1 => return interactive(Target::Runtime),
        // `set` 改的是**正在跑的那个实例**（内存），所以必须有实例；没有的话指条路：
        // 同样这几项用 `pliers config set` 就能离线写进配置文件
        Some("set") => {
            let request = args.join(" ");
            let hint = format!("想改配置文件（不需要实例在跑）：pliers config {request}");
            return remote(&request, &hint);
        }
        // 改配置文件（跟 set 区分开：这个会留下来）
        Some("config") => return config_command(&args[1..]),
        Some(other) => return Err(format!("不认识的参数 {other:?}（pliers --help 看看）").into()),
        None => {}
    }

    // 想换输入方案 → 改配置；想加词/调权重 → 改词库（pliers-dict）；
    // 想换协议/加功能 → 改 pliers-wayland
    let config = Config::load()?;
    let engine = Engine::from_config(&config)?;

    let options = Options {
        // PLIERS_DEBUG=1 时把每个按键的判定过程打出来，排查按键问题用
        debug: std::env::var_os("PLIERS_DEBUG").is_some(),
    };

    if options.debug {
        eprintln!(
            "pliers: 方案={} 词库={} 候选最多 {} 个 配置文件={}",
            engine.scheme_name(),
            config.dict_path().display(),
            config.dict.max_candidates,
            pliers_engine::config::config_path().display(),
        );
    }

    pliers_wayland::run(config, engine, options)
}

/// `pliers status [pinyin|wubi|english]`：看现状。
/// 不给种类 = 正在跑的实例 + 所有字典（每块各管各的，谁也不重复谁的信息）
fn status_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args::kind(args) {
        "" => {
            instance_section();
            println!();
            dict::report()?;
            // 码表库是**另一个文件**，只有真在用（或已经建了）才占一屏 ——
            // 不用五笔的人不必每次看一句"还没有"
            if wubi_matters() {
                println!();
                dict::wubi_report()?;
            }
            println!();
            english::report()?;
        }
        "pinyin" => dict::report()?,
        "wubi" => dict::wubi_report()?,
        "english" => english::report()?,
        other => return Err(unknown_kind("status", other)),
    }
    Ok(())
}

/// 聚合的 `pliers status` 里要不要带码表库那一块：文件在、或者当前方案就是码表
fn wubi_matters() -> bool {
    dict::wubi_path().exists()
        || matches!(
            Config::load().map(|config| config.scheme),
            Ok(pliers_engine::SchemeConfig::Wubi { .. })
        )
}

/// 正在跑的那个实例看到的现状。没在跑**不是错误**（字典那两块照样能看），
/// 所以只报一句，不当成失败
fn instance_section() {
    match ask("status") {
        Ok(Reply::Done(payload)) => {
            let fields = Fields::parse(&payload);
            println!(
                "{}",
                row("实例", format!("正在跑（socket {}）", fields.get("socket")))
            );
            print_status(&fields);
        }
        Ok(Reply::Failed(message)) => println!("{}", row("实例", format!("问不动它：{message}"))),
        Err(_) => println!("{}", row("实例", "没在跑（配置本身：pliers config show）")),
    }
}

/// `pliers path [名字]`：文件都在哪。不给名字 = 全列出来，给了 = 只打一行（脚本友好）
fn path_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // 配置坏了也得能问出路径来 —— 那就按默认值算
    let config = Config::load().unwrap_or_default();
    let all = [
        ("config", "配置文件", pliers_engine::config::config_path()),
        ("dict", "中文词库", config.dict_path()),
        ("wubi", "码表库", config.wubi_path()),
        ("user", "用户数据", config.user_path()),
        ("english", "英文词表", config.english_path()),
        ("corpus", "语料缓存", dict::corpus_dir()),
        (
            "socket",
            "遥控 socket",
            pliers_wayland::control::socket_path(),
        ),
    ];
    // 种类名也认：pinyin 就是 dict，wubi 有自己的文件
    let what = match args::kind(args) {
        "pinyin" => "dict",
        other => other,
    };
    if what.is_empty() {
        for (_, label, path) in &all {
            println!("{}", row(label, path.display()));
        }
        return Ok(());
    }
    match all.iter().find(|(name, ..)| *name == what) {
        Some((_, _, path)) => {
            println!("{}", path.display());
            Ok(())
        }
        None => Err(format!(
            "不认识的名字 {what:?}（path 收这些）：\n\
             \x20     config   配置文件\n\
             \x20     dict     拼音词库（pinyin 也是它）\n\
             \x20     wubi     码表库（五笔/郑码/仓颉，跟拼音词库分开一个文件）\n\
             \x20     user     用户数据（选过的词、自己拼的句子；两个库共用这一份）\n\
             \x20     english  英文词表\n\
             \x20     corpus   语料缓存（pliers build pinyin 下的那六个文件）\n\
             \x20     socket   遥控正在跑的实例用的 socket"
        )
        .into()),
    }
}

/// `pliers build <种类> [文件]`：自己构建一份字典
fn build_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args::kind(args) {
        "pinyin" => dict::build(args::has(args, "--refresh")),
        // 码表：`pliers build wubi 我的码表.txt`（--scheme 换码表名字，默认跟方案走）
        "wubi" => dict::build_wubi(
            args::at(args, 1),
            args::value_of(args, "--scheme").as_deref(),
        ),
        // 不给词表就用二进制里那份兜底重建（那条路不需要 pliers-dict）
        "english" => english::build(args::at(args, 1)),
        "" => Err("要构建哪一种？\n\
                   \x20     pliers build pinyin           拼音词库（下 rime-frost 语料，要 pliers-dict）\n\
                   \x20     pliers build wubi <码表.txt>  码表库（五笔/郑码/仓颉，要 pliers-dict）\n\
                   \x20     pliers build english [词表]   英文词表（不给词表就用二进制里那份兜底）"
            .into()),
        other => Err(unknown_kind("build", other)),
    }
}

/// 种类认不出来时报什么（四个动词共用一份词表，所以报错也长得一样）
fn unknown_kind(verb: &str, what: &str) -> Box<dyn std::error::Error> {
    format!(
        "不认识的种类 {what:?}（{verb} 收这些）：\n\
         \x20     pinyin   拼音词库（dict.db，27 MB，能下载也能自己构建）\n\
         \x20     wubi     码表库（wubi.db，五笔/郑码/仓颉，用自己的码表构建）\n\
         \x20     english  英文词表（english.db，约 500 KB）"
    )
    .into()
}

/// 老写法（名词在前）现在统一成动词在前，只留一句"该写什么"
fn retired(name: &str, rest: &[String]) -> Box<dyn std::error::Error> {
    let sub = rest.first().map(String::as_str).unwrap_or("");
    let hint = match (name, sub) {
        ("pliers dict", "build") => "pliers build pinyin",
        ("pliers dict", "path") => "pliers path dict",
        ("pliers dict", "") | ("pliers dict", "status") => "pliers status pinyin",
        ("pliers dict", _) => "pliers status pinyin / pliers fetch pinyin / pliers build pinyin",
        ("pliers english", "build") => "pliers build english",
        ("pliers english", "fetch") => "pliers fetch english",
        ("pliers english", "path") => "pliers path english",
        _ => "pliers status english",
    };
    format!(
        "{name} … 改了：命令都是**动词在前、种类在后**，这条现在写成 {hint}\n\
         \x20     看一眼全部：pliers --help"
    )
    .into()
}

/// 输入法回话的内容
enum Reply {
    /// 成功：后面跟着一串 tab 分隔的 `名=值`（现状）
    Done(String),
    /// 它说不行（配置写错了之类）
    Failed(String),
}

/// 连不上实例时的补充说明。
///
/// 最容易踩的是"`pliers set` 和 `pliers config set` 长得太像"：
/// 前者把命令发给**正在跑的那个进程**（改内存，重启就回去了），后者写**配置文件**（离线也能改）。
/// 所以连不上时得把后者说出来，不然用户只会看到"它在跑吗？"
fn offline_hint(message: &str, hint: &str) -> String {
    format!("{message}\n\x20     {hint}")
}

/// 把一条命令发给正在跑的实例。`Err` = 根本没连上（没有实例在跑）
fn ask(command: &str) -> Result<Reply, String> {
    let path = pliers_wayland::control::socket_path();
    match pliers_wayland::control::send(&path, command) {
        Ok(answer) if answer.starts_with("ERR ") => {
            Ok(Reply::Failed(answer["ERR ".len()..].to_string()))
        }
        Ok(answer) => Ok(Reply::Done(
            answer
                .strip_prefix("OK")
                .unwrap_or(&answer)
                .trim_start_matches('\t')
                .to_string(),
        )),
        Err(e) => Err(format!(
            "连不上正在跑的输入法（{}：{e}）\n\
             \x20     它在跑吗？socket 路径可以用 PLIERS_SOCKET=/别的/路径.sock 指定",
            path.display()
        )),
    }
}

/// `pliers config ...`：跟配置文件打交道
fn config_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let path = pliers_engine::config::config_path();
    match args.first().map(String::as_str) {
        // 不带项/值 = 交互模式（改的是配置文件）
        Some("set") if args.len() == 1 => interactive(Target::File),
        // 写一项进去（保留注释），再让正在跑的实例立刻重读
        Some("set") => {
            let rest = args[1..].join(" ");
            let (Some(key), Some(value)) = (args.get(1), args.get(2)) else {
                return Err("用法：pliers config set <项> <值>（pliers --help 看能改哪些）".into());
            };
            let _ = rest;
            pliers_engine::config::write_setting(&path, key, value)?;
            println!("写到 {}", path.display());
            println!("  {key} = {value}");
            // 跑着的实例本来也会自己发现（它盯着文件 mtime），这里顺手催一下，反馈快点
            match ask("reload") {
                Ok(Reply::Done(payload)) => {
                    let fields = Fields::parse(&payload);
                    println!("正在跑的实例已经重读：{}", status_summary(&fields));
                }
                Ok(Reply::Failed(message)) => println!("（文件写了，但实例说：{message}）"),
                Err(_) => println!("（现在没有实例在跑，下次启动就用它）"),
            }
            Ok(())
        }
        // 打印配置文件
        Some("show") => match std::fs::read_to_string(&path) {
            Ok(text) => {
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
                Ok(())
            }
            Err(e) => Err(format!("读不了 {}：{e}", path.display()).into()),
        },
        // 路径都归 pliers path 管了（配置文件也是其中一行）
        Some("path") => Err(
            "`pliers config path` 并进了 `pliers path`（那里所有路径都在一块）：\n\
                             \x20     pliers path config"
                .into(),
        ),
        // 用 $EDITOR 打开（跑着的实例会在你保存之后自动重读）
        Some("edit") => {
            if !path.exists() {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, pliers_engine::EXAMPLE_CONFIG)?;
                println!("{} 还不存在，先写了一份带注释的模板", path.display());
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let status = std::process::Command::new(&editor).arg(&path).status()?;
            if !status.success() {
                return Err(format!("{editor} 退出了（{status}）").into());
            }
            println!("存过之后正在跑的实例会自动重读（最多 1 秒）");
            Ok(())
        }
        other => Err(
            format!("不认识的 config 子命令 {other:?}（能用：set / show / path / edit）").into(),
        ),
    }
}

/// 把一条命令发给实例、按结果说话。脚本用：出错就退出码 1
fn remote(command: &str, hint: &str) -> Result<(), Box<dyn std::error::Error>> {
    let answer = ask(command).map_err(|error| offline_hint(&error, hint))?;
    match answer {
        Reply::Done(payload) => {
            print_reply(command, &payload);
            Ok(())
        }
        // 回话已经说清楚哪儿错了，不用再解释一遍，只把退出码给对
        Reply::Failed(message) => {
            eprintln!("{message}");
            std::process::exit(1)
        }
    }
}

/// 实例回的那串 `名=值`（tab 分隔）
struct Fields(Vec<(String, String)>);

impl Fields {
    fn parse(payload: &str) -> Self {
        Self(
            payload
                .split('\t')
                .filter_map(|field| field.split_once('='))
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        )
    }

    fn get(&self, name: &str) -> &str {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map_or("", |(_, value)| value.as_str())
    }
}

/// 现状里的一行：标签补齐到 12 列（中文按两列算，所以"中文词库"和"来源"照样对齐），
/// 值跟在后面。状态、字典、路径几块都用它排版，所以列是对齐的
pub(crate) fn row(label: &str, value: impl std::fmt::Display) -> String {
    format!("{} {value}", pick::pad(label, 12))
}

/// 续行：不写标签，缩进到值那一列
pub(crate) fn note(text: impl std::fmt::Display) -> String {
    format!("{}{text}", " ".repeat(13))
}

/// 文件多大（小文件也别显示成 0 MB）；读不到（还没建）就是 None
pub(crate) fn size(path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    Some(if len >= 1024 * 1024 {
        format!("{} MB", len / 1024 / 1024)
    } else {
        format!("{} KB", len.max(1024) / 1024)
    })
}

fn on_off(value: &str) -> &str {
    if value == "true" { "开" } else { "关" }
}

fn layout_name(layout: &str) -> &str {
    match layout {
        "flypy" => "小鹤",
        "mspy" => "微软",
        "none" => "自定义",
        "" => "—",
        _ => "自然码",
    }
}

/// 现状：一项一行（标签补齐到 12 列，中文按两列算，所以对得齐）
fn status_lines(fields: &Fields) -> Vec<String> {
    let scheme = match fields.get("kind") {
        "double-pinyin" => format!("双拼（{}）", layout_name(fields.get("layout"))),
        "wubi" => format!("五笔（码表：{}）", fields.get("name")),
        _ => format!("全拼（整句候选{}）", on_off(fields.get("sentence"))),
    };
    let toggle = fields.get("toggle");
    vec![
        row("方案", scheme),
        row("词库", fields.get("dict").to_string()),
        row(
            "候选",
            format!(
                "一页 {} 个，池子 {} 个",
                fields.get("candidates"),
                fields.get("pool")
            ),
        ),
        row("用户数据", fields.get("user").to_string()),
        row("英文候选", english_status(fields)),
        row(
            "中英切换",
            if toggle.is_empty() {
                "（没设）".to_string()
            } else {
                toggle.to_string()
            },
        ),
        row("中英提示", on_off(fields.get("indicator")).to_string()),
        row(
            "模式",
            format!("{}（启动时 {}）", fields.get("mode"), fields.get("start")),
        ),
        row("配置文件", fields.get("config").to_string()),
        row("socket", fields.get("socket").to_string()),
    ]
}

/// 英文候选那一行：关掉就说"关"；开着的话把词表大小也报出来
///（词表大小只有跑着的实例知道 —— 它是起引擎时读进内存的，所以可能缺这个字段）
fn english_status(fields: &Fields) -> String {
    if fields.get("english") == "false" {
        return "关".to_string();
    }
    let limit = fields.get("english_limit");
    let limit = if limit.is_empty() { "—" } else { limit };
    let source = fields.get("english_source");
    let where_from = if source.is_empty() {
        String::new()
    } else {
        format!(" · {source}")
    };
    match fields.get("english_words") {
        "" | "0" => format!("开（一次最多 {limit} 个）{where_from}"),
        words => format!("开（词表 {words} 个词，一次最多 {limit} 个{where_from}）"),
    }
}

/// 一行摘要（`config set` 之后报一句就够了，不用贴整块现状）
fn status_summary(fields: &Fields) -> String {
    let scheme = match fields.get("kind") {
        "double-pinyin" => format!("双拼（{}）", layout_name(fields.get("layout"))),
        "wubi" => format!("五笔（码表：{}）", fields.get("name")),
        _ => "全拼".to_string(),
    };
    format!(
        "{scheme} · 一页 {} 个候选 · {}模式",
        fields.get("candidates"),
        fields.get("mode")
    )
}

fn print_status(fields: &Fields) {
    for line in status_lines(fields) {
        println!("{line}");
    }
}

/// 按命令决定先说一句什么，再把现状贴出来
fn print_reply(command: &str, payload: &str) {
    let fields = Fields::parse(payload);
    match command.split(' ').next().unwrap_or("status") {
        "reload" => println!("重新读了配置文件，现在是："),
        "set" if !fields.get("changed").is_empty() => {
            println!("改好了：{}", fields.get("changed"));
        }
        _ => {}
    }
    print_status(&fields);
}

/// 一项配置怎么改
enum Kind {
    /// 从这几个值里上下选
    Choice(&'static [&'static str]),
    /// 手输（预填现在这个值）
    Text,
}

/// 交互模式里能改的项：路径、名字、改法、现状看哪个字段
const ITEMS: &[(&str, &str, Kind, &str)] = &[
    (
        "scheme.kind",
        "输入方案",
        Kind::Choice(&["full-pinyin", "double-pinyin", "wubi"]),
        "kind",
    ),
    (
        "scheme.layout",
        "双拼键位",
        Kind::Choice(&["natural", "flypy", "mspy", "none"]),
        "layout",
    ),
    (
        "scheme.sentence",
        "整句候选",
        Kind::Choice(&["true", "false"]),
        "sentence",
    ),
    ("scheme.name", "码表名", Kind::Text, "name"),
    ("dict.path", "拼音词库文件", Kind::Text, "dict_path"),
    (
        "dict.max_candidates",
        "一页候选数",
        Kind::Choice(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),
        "candidates",
    ),
    ("dict.pool_size", "候选池深度", Kind::Text, "pool"),
    (
        "engine.toggle_keys",
        "中英切换键",
        Kind::Choice(&["ctrl+space", "shift", "ctrl+space,shift"]),
        "toggle",
    ),
    (
        "engine.start_mode",
        "启动模式",
        Kind::Choice(&["chinese", "english"]),
        "start",
    ),
    (
        "engine.indicator",
        "切换提示",
        Kind::Choice(&["true", "false"]),
        "indicator",
    ),
    (
        "engine.chinese_punctuation",
        "中文标点",
        Kind::Choice(&["true", "false"]),
        "chinese_punctuation",
    ),
    (
        "english.enabled",
        "英文候选",
        Kind::Choice(&["true", "false"]),
        "english",
    ),
    (
        "english.limit",
        "英文候选个数",
        Kind::Choice(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),
        "english_limit",
    ),
    ("english.path", "英文词表", Kind::Text, "english_path"),
    ("dict.user_path", "用户数据文件", Kind::Text, "user_path"),
    // 三个"文件在哪"的项排在一起（加在最后，前面那些项的编号就不会变——
    // mock 测试里的交互模式断言是按编号走的）
    ("dict.wubi_path", "码表库文件", Kind::Text, "wubi_path"),
];

/// 菜单里除了改配置，还有这两件事
const ACTIONS: &[(&str, &str)] = &[("reload", "重新读配置文件"), ("status", "看完整现状")];

/// 现状怎么显示：开关类显示成「开/关」（值本身还是 true/false，比对时用原文）
fn show_value(kind: &Kind, raw: &str) -> String {
    if raw.is_empty() {
        return "—".to_string();
    }
    match kind {
        Kind::Choice(allowed) if *allowed == ["true", "false"] => on_off(raw).to_string(),
        _ => raw.to_string(),
    }
}

/// 主菜单都有哪些项（值怎么显示、可选项是啥，排版交给 [`pick::Picker`]）
fn menu_entries(fields: &Fields) -> Vec<pick::Item> {
    let mut items: Vec<pick::Item> = ITEMS
        .iter()
        .map(|(_, label, kind, field)| {
            let options = match kind {
                Kind::Choice(allowed) => allowed.join(" / "),
                Kind::Text => "手输".to_string(),
            };
            pick::Item::new(*label).detail(show_value(kind, fields.get(field)), options)
        })
        .collect();
    items.extend(ACTIONS.iter().map(|(_, label)| pick::Item::new(*label)));
    items
}

/// 真正的交互模式：上下选的列表（要真终端；管道进来就退回行式，见 [`interactive`]）
fn fancy(target: Target) -> Result<(), Box<dyn std::error::Error>> {
    // 整个会话停在原始模式：中途出错也会因为 Drop 恢复终端
    let _raw = RawMode::enable()?;

    let mut fields = match target {
        Target::File => fields_from_config(&read_config()?),
        Target::Runtime => match ask("status") {
            Ok(Reply::Done(payload)) => Fields::parse(&payload),
            Ok(Reply::Failed(message)) => return Err(format!("没拿到现状：{message}").into()),
            Err(error) => {
                return Err(offline_hint(
                    &error,
                    "`pliers set` 改的是正在跑的实例；改配置文件用 `pliers config set`",
                )
                .into());
            }
        },
    };
    println!("改的是{}：", target.label());
    print_status(&fields);
    println!();

    // 改完一项之后，光标停回刚才那一项上：连改几项不用重新挪
    let mut last = 0usize;
    loop {
        let mut menu = Picker::new("要改哪一项？", menu_entries(&fields));
        menu.focus(last);
        let Some(choice) = menu.run()? else { break };
        last = choice;

        // 改配置
        if let Some((key, label, kind, field)) = ITEMS.get(choice) {
            let current = fields.get(field).to_string();
            let shown = show_value(kind, &current);
            let value = match kind {
                Kind::Choice(allowed) => {
                    let items = allowed
                        .iter()
                        .map(|value| pick::Item::new(*value).marked(*value == current))
                        .collect();
                    let mut values = Picker::new(format!("{label}改成？（现在 {shown}）"), items);
                    // 光标先停在现在这个值上
                    if let Some(index) = allowed.iter().position(|value| *value == current) {
                        values.focus(index);
                    }
                    values.run()?.map(|index| allowed[index].to_string())
                }
                Kind::Text => pick::input_line(
                    &format!("{label}改成？（回车不改，现在 {shown}）"),
                    &current,
                )?,
            };
            let Some(value) = value else { continue };
            if value == current {
                println!("没改：还是 {current}");
            } else {
                apply(target, key, &value, &mut fields)?;
            }
            continue;
        }

        // 或者做件事
        match ACTIONS.get(choice - ITEMS.len()) {
            Some(("reload", _)) => apply(target, "reload", "", &mut fields)?,
            Some(("status", _)) => {
                println!("现在跑着的是：");
                print_status(&fields);
                println!();
            }
            _ => {}
        }
    }
    println!("好，就这样。");
    Ok(())
}

/// 行式（管道/脚本）里那份清单：编号 + key + 现状
fn print_items(fields: &Fields) {
    println!("能改的项（输编号，或者直接 `<项> <值>`；q 退出）：");
    for (index, (key, label, kind, field)) in ITEMS.iter().enumerate() {
        let options = match kind {
            Kind::Choice(allowed) => allowed.join(" / "),
            Kind::Text => "手输".to_string(),
        };
        println!(
            "  {:>2}) {}  {key:<20} 现在 {}  可选 {options}",
            index + 1,
            pick::pad(label, 12),
            show_value(kind, fields.get(field))
        );
    }
    for (index, (key, label)) in ACTIONS.iter().enumerate() {
        println!("  {:>2}) {label}   [{key}]", ITEMS.len() + index + 1);
    }
}

/// 交互模式改哪儿：跑着的实例，还是配置文件
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// `pliers set`：只改正在跑的实例
    Runtime,
    /// `pliers config set`：写进配置文件（实例会自动重读）
    File,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Self::Runtime => "正在跑的实例（重启就回去了）",
            Self::File => "配置文件（写进去，跑着的实例自动重读）",
        }
    }
}

/// 从配置文件内容算出"现状"字段：没有实例在跑、或者改的就是文件时用它
fn fields_from_config(config: &Config) -> Fields {
    let (kind, layout, name, sentence) = match &config.scheme {
        pliers_engine::SchemeConfig::FullPinyin { sentence } => ("full-pinyin", "", "", *sentence),
        pliers_engine::SchemeConfig::DoublePinyin {
            layout, sentence, ..
        } => ("double-pinyin", layout.as_str(), "", *sentence),
        pliers_engine::SchemeConfig::Wubi { name } => ("wubi", "", name.as_str(), false),
    };
    let dict = config.dict_path();
    let dict = match std::fs::metadata(&dict) {
        Ok(meta) => format!("{}（{} MB）", dict.display(), meta.len() / 1024 / 1024),
        Err(_) => format!("{}（打不开？）", dict.display()),
    };
    let fields = [
        format!("kind={kind}"),
        format!("layout={layout}"),
        format!("name={name}"),
        format!("sentence={sentence}"),
        format!("dict={dict}"),
        format!("dict_path={}", config.dict_path().display()),
        format!("wubi_path={}", config.wubi_path().display()),
        format!(
            "user={}",
            match std::fs::metadata(config.user_path()) {
                Ok(meta) => format!(
                    "{}（{} KB）",
                    config.user_path().display(),
                    meta.len().max(1024) / 1024
                ),
                Err(_) => format!("{}（还没有，用着会自动建）", config.user_path().display()),
            }
        ),
        format!("user_path={}", config.user_path().display()),
        format!("candidates={}", config.dict.max_candidates),
        format!("pool={}", config.dict.pool_size),
        format!("toggle={}", config.engine.toggle_keys.join(",")),
        format!(
            "start={}",
            match config.engine.start_mode {
                Mode::Chinese => "chinese",
                Mode::English => "english",
            }
        ),
        format!("indicator={}", config.engine.indicator),
        format!("chinese_punctuation={}", config.engine.chinese_punctuation),
        format!("english={}", config.english.enabled),
        format!("english_limit={}", config.english.limit),
        format!("english_path={}", config.english.path),
        format!("english_extra={}", config.english.extra),
        format!("english_source={}", config.english_path().display()),
        format!("config={}", pliers_engine::config::config_path().display()),
    ];
    Fields::parse(&fields.join("\t"))
}

/// 交互模式。
///
/// 真终端里是"上下选"的列表（[`fancy`]）；stdin/stdout 不是终端（管道、脚本、测试）时退回
/// 行式：列出来、输编号或 `<项> <值>` —— 那样才能自动化跑
fn interactive(target: Target) -> Result<(), Box<dyn std::error::Error>> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return fancy(target);
    }
    let mut fields = match target {
        // 改文件：现状就从文件读（不需要有实例在跑）
        Target::File => fields_from_config(&read_config()?),
        Target::Runtime => match ask("status") {
            Ok(Reply::Done(payload)) => Fields::parse(&payload),
            Ok(Reply::Failed(message)) => return Err(format!("没拿到现状：{message}").into()),
            Err(error) => {
                return Err(offline_hint(
                    &error,
                    "`pliers set` 改的是正在跑的实例；改配置文件用 `pliers config set`",
                )
                .into());
            }
        },
    };
    println!("改的是{}：", target.label());
    print_status(&fields);
    println!();
    print_items(&fields);

    let stdin = std::io::stdin();
    loop {
        print!("\n> ");
        std::io::stdout().flush()?;
        let Some(line) = read_line(&stdin)? else {
            println!();
            break; // Ctrl+D
        };
        let line = line.trim();
        match line {
            "" | "?" | "ls" | "list" | "help" => print_items(&fields),
            "q" | "quit" | "exit" => break,
            "s" | "status" => print_status(&fields),
            "r" | "reload" => apply(target, "reload", "", &mut fields)?,
            _ => {
                let number = line.parse::<usize>().ok();
                // 编号也可能是"做件事"那两个（它们从 ITEMS.len()+1 开始编号）
                if let Some(action) = number
                    .filter(|index| *index > ITEMS.len())
                    .and_then(|index| ACTIONS.get(index - ITEMS.len() - 1))
                {
                    apply(target, action.0, "", &mut fields)?;
                    continue;
                }
                let (key, value) = match number {
                    Some(index) if (1..=ITEMS.len()).contains(&index) => {
                        let (key, label, kind, field) = &ITEMS[index - 1];
                        let allowed = match kind {
                            Kind::Choice(allowed) => allowed.join(" / "),
                            Kind::Text => "手输".to_string(),
                        };
                        print!(
                            "{label}现在是 {}，改成？（回车不改，可选 {allowed}）\n> ",
                            show_value(kind, fields.get(field))
                        );
                        std::io::stdout().flush()?;
                        match read_line(&stdin)? {
                            Some(value) if !value.trim().is_empty() => {
                                (key.to_string(), value.trim().to_string())
                            }
                            _ => continue,
                        }
                    }
                    Some(index) => {
                        println!("没有第 {index} 项（一共 {} 项）", ITEMS.len());
                        continue;
                    }
                    None if line.contains(' ') => match line.split_once(' ') {
                        Some((key, value)) => (key.to_string(), value.trim().to_string()),
                        None => continue,
                    },
                    None => {
                        println!("输编号（1-{}）或 `<项> <值>`，q 退出", ITEMS.len());
                        continue;
                    }
                };
                apply(target, &key, &value, &mut fields)?;
            }
        }
    }
    Ok(())
}

/// 交互模式里改一项：失败只说一句，不退出（还能接着改）
fn apply(
    target: Target,
    key: &str,
    value: &str,
    fields: &mut Fields,
) -> Result<(), Box<dyn std::error::Error>> {
    match target {
        // 只改跑着的实例
        Target::Runtime => {
            if key == "reload" {
                match ask("reload")? {
                    Reply::Done(payload) => {
                        *fields = Fields::parse(&payload);
                        println!();
                        print_status(fields);
                    }
                    Reply::Failed(message) => println!("没成功：{message}"),
                }
                return Ok(());
            }
            match ask(&format!("set {key} {value}"))? {
                Reply::Done(payload) => {
                    let fresh = Fields::parse(&payload);
                    if !fresh.get("changed").is_empty() {
                        println!("改好了：{}", fresh.get("changed"));
                    }
                    *fields = fresh;
                    println!();
                    print_status(fields);
                }
                Reply::Failed(message) => println!("没改成：{message}"),
            }
        }
        // 写进配置文件；跑着的实例顺手催一下重读，然后现状按文件重算
        Target::File if key == "reload" => {
            let _ = ask("reload");
            *fields = fields_from_config(&read_config()?);
            println!("重新读了配置文件");
            println!();
            print_status(fields);
        }
        Target::File => {
            let path = pliers_engine::config::config_path();
            match pliers_engine::config::write_setting(&path, key, value) {
                Ok(()) => {
                    println!("写进 {}：{key} = {value}", path.display());
                    if let Ok(Reply::Done(_)) = ask("reload") {
                        println!("正在跑的实例已经重读");
                    }
                    *fields = fields_from_config(&read_config()?);
                    println!();
                    print_status(fields);
                }
                Err(e) => println!("没写成：{e}"),
            }
        }
    }
    Ok(())
}

/// 读并解析配置文件（不存在就当默认值）
fn read_config() -> Result<Config, Box<dyn std::error::Error>> {
    let path = pliers_engine::config::config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)?;
    Config::parse(&text)
}

fn read_line(stdin: &std::io::Stdin) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    if stdin.read_line(&mut line)? == 0 {
        Ok(None) // EOF
    } else {
        Ok(Some(line))
    }
}

/// 把配置模板写到配置路径，并告诉用户改完要重启
fn init_config(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = pliers_engine::config::config_path();
    if path.exists() && !force {
        println!("{} 已经在了（要覆盖就加 --force）", path.display());
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, pliers_engine::EXAMPLE_CONFIG)?;
    println!("写到 {}", path.display());
    println!("编辑它，然后 `pliers reload` 让它生效（不用重启）");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 服务端回话的样子（字段顺序跟 status_fields 一致）
    const PAYLOAD: &str = concat!(
        "kind=double-pinyin\tlayout=flypy\tname=\tsentence=true\t",
        "dict=/home/dao/.local/share/pliers/dict.db（131 MB）\t",
        "dict_path=/home/dao/.local/share/pliers/dict.db\t",
        "user=/home/dao/.local/share/pliers/user.db（8 KB）\t",
        "user_path=/home/dao/.local/share/pliers/user.db\t",
        "candidates=9\tpool=90\ttoggle=ctrl+space\tstart=chinese\tindicator=true\t",
        "chinese_punctuation=true\tenglish=true\tenglish_limit=5\tenglish_path=\t",
        "english_words=25223\tmode=中\tconfig=/home/dao/.config/pliers/config.toml\t",
        "socket=/run/user/1000/pliers.sock",
    );

    #[test]
    fn 状态按字段渲染成多行() {
        let fields = Fields::parse(PAYLOAD);
        let lines = status_lines(&fields);
        assert_eq!(lines.len(), 10, "一项一行：{lines:?}");
        assert!(lines[0].contains("双拼（小鹤）"), "{:?}", lines[0]);
        assert!(
            lines[1].contains("/home/dao/.local/share"),
            "{:?}",
            lines[1]
        );
        assert!(lines[2].contains("一页 9 个，池子 90 个"), "{:?}", lines[2]);
        assert!(lines[3].contains("user.db"), "{:?}", lines[3]);
        assert!(
            lines[4].contains("开（词表 25223 个词，一次最多 5 个）"),
            "{:?}",
            lines[4]
        );
        assert!(lines[7].contains("中（启动时 chinese）"), "{:?}", lines[7]);
    }

    #[test]
    fn 状态标签按显示宽度对齐() {
        let fields = Fields::parse(PAYLOAD);
        let labels = [
            "方案",
            "词库",
            "候选",
            "用户数据",
            "英文候选",
            "中英切换",
            "中英提示",
            "模式",
            "配置文件",
            "socket",
        ];
        for (line, label) in status_lines(&fields).iter().zip(labels) {
            // 每行都是「标签补齐到 12 列 + 一个空格 + 值」，所以值都从第 14 列开始
            let prefix = format!("{} ", pick::pad(label, 12));
            assert!(line.starts_with(&prefix), "该以「{prefix}」开头：{line:?}");
            assert!(line.len() > prefix.len(), "每行都该有值：{line:?}");
        }
        // 中文一个字两列，所以「中英切换」也是 12 列宽
        assert_eq!(pick::width("中英切换"), 8);
        assert_eq!(pick::width(&pick::pad("中英切换", 12)), 12);
    }

    #[test]
    fn 全拼和五笔也认得出来() {
        let full = Fields::parse("kind=full-pinyin\tsentence=false");
        assert!(status_lines(&full)[0].contains("全拼（整句候选关）"));
        let wubi = Fields::parse("kind=wubi\tname=wubi");
        assert!(status_lines(&wubi)[0].contains("五笔（码表：wubi）"));
    }

    #[test]
    fn 没设切换键时说清楚() {
        let fields = Fields::parse("kind=full-pinyin\ttoggle=");
        assert!(status_lines(&fields)[5].contains("（没设）"));
    }

    #[test]
    fn 菜单列出所有项和现状() {
        let fields = Fields::parse(PAYLOAD);
        let items = menu_entries(&fields);
        assert_eq!(items.len(), ITEMS.len() + ACTIONS.len());
        assert_eq!(items[0].label, "输入方案");
        assert_eq!(items[0].now, "double-pinyin");
        assert_eq!(items[ITEMS.len()].label, "重新读配置文件");
    }

    #[test]
    fn 开关类显示成开和关() {
        let fields = Fields::parse(PAYLOAD); // sentence=true / indicator=true
        let sentence = menu_entries(&fields)
            .into_iter()
            .find(|item| item.label == "整句候选")
            .expect("菜单里该有整句候选");
        assert_eq!(sentence.now, "开");
        assert_eq!(sentence.options, "true / false");
    }
}
