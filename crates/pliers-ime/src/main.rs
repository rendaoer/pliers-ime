//! 最小输入法：组装"引擎"和"协议层"。
//!
//! * `pliers-engine`  —— 按键该干什么 + 输入方案（全拼/双拼/码表）+ SQLite 词库
//! * `pliers-popup`   —— 候选框长什么样（找字体、排版、画像素）
//! * `pliers-wayland` —— 跟合成器说协议，把引擎的决定变成 Wayland 请求
//!
//! 行为都在配置文件里（`~/.config/pliers/config.toml`），代码不用改：
//! 换全拼/双拼/五笔、换词库、改候选个数，都是改 TOML。

use pliers_engine::{Config, Engine};
use pliers_wayland::Options;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // 写一份带注释的配置模板，省得手敲（已存在就不覆盖）
        Some("--init-config") => return init_config(args.iter().any(|arg| arg == "--force")),
        Some("--help" | "-h") => {
            println!(
                "pliers —— 从零手写的 Wayland 中文输入法\n\n\
                 用法：pliers [选项]\n\
                 \x20 --init-config [--force]  写一份配置模板到配置路径\n\
                 \x20 --help                    看这个\n\n\
                 配置：{}\n\
                 词库：用 `cargo run -p pliers-dict --release -- --help` 看怎么生成\n\
                 调试：PLIERS_DEBUG=1 pliers 会把每个按键的判定打到 stderr",
                pliers_engine::config::config_path().display()
            );
            return Ok(());
        }
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

    pliers_wayland::run(engine, options)
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
    println!("编辑它，然后重启输入法（配置只在启动时读一次）");
    Ok(())
}
