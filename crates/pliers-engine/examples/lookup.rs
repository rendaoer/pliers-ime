//! 命令行试打：不进合成器、不用键盘，直接看某个输入会出哪些候选、每个键花多久。
//!
//! ```text
//! cargo run -p pliers-engine --release --example lookup -- shijian
//! cargo run -p pliers-engine --example lookup -- nihao --config ~/.config/pliers/config.toml
//! ```
//!
//! 调词库、加词之后用它看效果最快 —— 比开输入法在输入框里打字快多了。

use std::time::Instant;

use pliers_engine::{Action, Config, Engine, KeyInput};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut input = None;
    let mut config = Config::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let path = args.next().ok_or("--config 后面要跟路径")?;
                config = Config::parse(&std::fs::read_to_string(&path)?)?;
            }
            other => input = Some(other.to_string()),
        }
    }
    let input = input.unwrap_or_else(|| "nihao".to_string());

    let started = Instant::now();
    let mut engine = Engine::from_config(&config)?;
    eprintln!(
        "方案 {}，词库 {}（打开用了 {:?}）\n",
        engine.scheme_name(),
        config.dict_path().display(),
        started.elapsed()
    );

    // 一个字母一个字母地敲，看每一步的候选
    let mut typed = String::new();
    let mut worst = std::time::Duration::ZERO;
    for ch in input.chars() {
        typed.push(ch);
        let t0 = Instant::now();
        let action = engine.on_key(KeyInput {
            keycode: 0,
            keysym: ch as u32,
            pressed: true,
            shortcut: false,
            ctrl: false,
            active: true,
        });
        let spent = t0.elapsed();
        worst = worst.max(spent);

        let Action::UpdatePreedit(preedit) = action else {
            println!("{typed:<12} → {action:?}");
            continue;
        };
        let marked: Vec<String> = preedit
            .candidates
            .iter()
            .enumerate()
            .map(|(i, word)| {
                if i == preedit.selected {
                    format!("[{word}]")
                } else {
                    word.clone()
                }
            })
            .collect();
        println!("{typed:<12} {:>7.2?}  {}", spent, marked.join(" "));
    }

    // 空格上屏第一个
    let action = engine.on_key(KeyInput {
        keycode: 57,
        keysym: pliers_engine::KEY_SPACE,
        pressed: true,
        shortcut: false,
        ctrl: false,
        active: true,
    });
    println!("\n空格 → {action:?}");
    println!("最慢的一次按键 {worst:.2?}（每次按键都是一个完整状态机 + 若干次查库）");
    Ok(())
}
