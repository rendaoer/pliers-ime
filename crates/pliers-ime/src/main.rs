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
