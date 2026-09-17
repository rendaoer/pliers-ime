//! 最小输入法：组装"引擎"和"协议层"。
//!
//! * `ime-engine`  —— 按键该干什么（拼音 buffer、词表、提交规则），纯逻辑，可单测
//! * `ime-popup`   —— 候选框长什么样（找字体、排版、画像素）
//! * `ime-wayland` —— 跟合成器说协议，把引擎的决定变成 Wayland 请求
//!
//! 需要哪个 crate 的细节就去读哪个，这里保持一眼能看完。

use ime_engine::Engine;
use ime_wayland::Options;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 想换词库/换组词规则 → 改 ime-engine；想换候选框长相 → 改 ime-popup；
    // 想换协议/加功能 → 改 ime-wayland
    let engine = Engine::new();

    let options = Options {
        // IME_AA_DEBUG=1 时把每个按键的判定过程打出来，排查按键问题用
        debug: std::env::var_os("IME_AA_DEBUG").is_some(),
    };

    ime_wayland::run(engine, options)
}
