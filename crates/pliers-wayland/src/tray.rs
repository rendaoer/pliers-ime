//! 托盘图标：让 Noctalia / waybar / KDE 这类带 tray 的桌面**自动**看见 pliers 的中/英状态。
//!
//! 这就是 Steam 那种"一启动就出现在状态栏"的同一条路：注册一个
//! `org.kde.StatusNotifierItem`（SNI），桌面的 StatusNotifierWatcher 会枚举到它。
//! Wayland 的 input-method 协议本身没有状态字段，所以输入法要自己把状态**说出去**；
//! SNI 是现成的、所有 tray 都认的标准。
//!
//! 图标是 `pliers-popup` 画的单色「中」/「英」/「?」，22/33/44px 各一份
//!（1.5x 屏正好用 33px，不会被缩放糊掉）。托盘每 500ms 通过控制 socket 读一次
//! `status`，菜单里的输入方案/双拼键位/开关都跟着它走；左键直接切中英文，
//! 右键是配置菜单。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Icon, ToolTip, Tray};

use crate::control;

const UNKNOWN: u8 = 0;
const CHINESE: u8 = 1;
const ENGLISH: u8 = 2;

/// 单色前景：白字带一圈深色描边，深/浅 bar 上都看得见
const INK: [u8; 4] = [0xE8, 0xE8, 0xE8, 0xFF];

/// 托盘图标的一套像素图（1x / 1.5x / 2x）
struct Icons {
    unknown: Vec<Icon>,
    chinese: Vec<Icon>,
    english: Vec<Icon>,
}

impl Icons {
    fn for_mode(&self, mode: u8) -> &[Icon] {
        match mode {
            CHINESE => &self.chinese,
            ENGLISH => &self.english,
            _ => &self.unknown,
        }
    }
}

/// 托盘菜单要用的那一小份现状（从 `status` 里挑出来）。
///
/// 不直接存整个 `status`：它带词库大小、路径这些大字符串，菜单用不上；
/// 而且托盘每 500ms 轮询一次，字段越少越轻。
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    mode: u8,
    kind: String,
    layout: String,
    sentence: bool,
    punctuation: bool,
    english: bool,
    indicator: bool,
    candidates: usize,
    toggle: String,
    wubi_ready: bool,
    /// 右键菜单样式：`grouped`（分组子菜单）/ `flat`（一层平铺）
    menu: String,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            mode: UNKNOWN,
            kind: String::new(),
            layout: String::new(),
            sentence: false,
            punctuation: false,
            english: false,
            indicator: false,
            candidates: 9,
            toggle: String::new(),
            wubi_ready: false,
            menu: "grouped".into(),
        }
    }
}

impl Snapshot {
    /// 从控制 socket 的 `OK\t名=值\t…` 那一行里挑字段
    fn parse(reply: &str) -> Self {
        let mut snapshot = Self::default();
        for field in reply.trim_start_matches("OK").split('\t') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "mode" => snapshot.mode = mode_of(value),
                "kind" => snapshot.kind = value.to_string(),
                "layout" => snapshot.layout = value.to_string(),
                "sentence" => snapshot.sentence = value == "true",
                "chinese_punctuation" => snapshot.punctuation = value == "true",
                "english" => snapshot.english = value == "true",
                "indicator" => snapshot.indicator = value == "true",
                "candidates" => snapshot.candidates = value.parse().unwrap_or(9),
                "toggle" => snapshot.toggle = value.to_string(),
                "wubi_ready" => snapshot.wubi_ready = value == "true",
                "tray_menu" => snapshot.menu = value.to_string(),
                _ => {}
            }
        }
        snapshot
    }
}

/// SNI 的 `Tray` 实现：图标/提示/菜单都从 `snapshot` 现算
struct PliersTray {
    snapshot: Snapshot,
    icons: Arc<Icons>,
    socket: PathBuf,
}

impl Tray for PliersTray {
    fn id(&self) -> String {
        "pliers".into()
    }

    fn title(&self) -> String {
        "pliers".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.for_mode(self.snapshot.mode).to_vec()
    }

    fn tool_tip(&self) -> ToolTip {
        let description = format!(
            "{} · {} · 左键切中英文，右键配置",
            mode_word(self.snapshot.mode),
            scheme_label(&self.snapshot),
        );
        ToolTip {
            title: "pliers 输入法".into(),
            description,
            icon_pixmap: self.icons.for_mode(self.snapshot.mode).to_vec(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        // `quiet`：托盘自己就是状态反馈，不用再在光标处弹「中/英」。
        // 图标立刻翻过去，控制命令丢到后台线程发 —— 点击不等 socket 往返，
        // 也不卡桌面的 DBus 线程；下一次 500ms 轮询会校正。
        let next = match self.snapshot.mode {
            CHINESE => ENGLISH,
            ENGLISH => CHINESE,
            _ => UNKNOWN,
        };
        if next != UNKNOWN {
            self.snapshot.mode = next;
        }
        fire(self.socket.clone(), "toggle quiet");
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let s = &self.snapshot;
        // 第一行是状态，第二行是菜单样式开关。Noctalia 的分组子菜单是向下展开的
        // 独立弹层（会盖住下面的项、也没有"返回上一级"），所以留一个随时能切到
        // 平铺的逃生口；切完菜单会关掉，重新打开就是新样式。
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: format!("pliers · {} · {}", mode_word(s.mode), scheme_label(s)),
                enabled: false,
                ..Default::default()
            }
            .into(),
            style_item(s),
            MenuItem::Separator,
        ];
        if s.menu == "flat" {
            items.extend(flat_items(s));
        } else {
            items.extend(grouped_items(s));
        }
        items
    }
}

/// 菜单样式开关：分组（好看）/ 平铺（好翻）
fn style_item(s: &Snapshot) -> MenuItem<PliersTray> {
    let (label, next) = if s.menu == "flat" {
        ("切换为分组菜单", "grouped")
    } else {
        ("切换为平铺菜单", "flat")
    };
    StandardItem {
        label: label.into(),
        activate: Box::new(move |tray: &mut PliersTray| {
            tray.snapshot.menu = next.into();
            save(tray, format!("tray.menu {next}"));
        }),
        ..Default::default()
    }
    .into()
}

/// 分组子菜单：好看、紧凑，但 Noctalia 里子菜单弹出来之后退不回一级
fn grouped_items(s: &Snapshot) -> Vec<MenuItem<PliersTray>> {
    vec![
        // Noctalia 里任何“叶子项”点下去都会关掉整个菜单；所以这个“关闭”项得
        // 做成一个带隐藏子项的 SubMenu：点击时宿主走 openSubmenuAtLevel(0, ...)，
        // 先把已经打开的子菜单收掉，再发现这层没有可显示的子项，就不会再开新的。
        // 它必须放在所有分组**上面**：子菜单是从分组行往下弹的，放在下面会被盖住。
        SubMenu {
            label: "关闭二级菜单".into(),
            submenu: vec![
                StandardItem {
                    label: String::new(),
                    visible: false,
                    ..Default::default()
                }
                .into(),
            ],
            ..Default::default()
        }
        .into(),
        SubMenu {
            label: format!("输入方案：{}", kind_label(&s.kind)),
            submenu: vec![
                RadioGroup {
                    selected: kind_index(&s.kind),
                    select: Box::new(|tray: &mut PliersTray, index: usize| {
                        let kind = ["full-pinyin", "double-pinyin", "wubi"][index.min(2)];
                        tray.snapshot.kind = kind.into();
                        if kind != "double-pinyin" {
                            tray.snapshot.layout.clear();
                        }
                        if kind == "wubi" {
                            tray.snapshot.sentence = false;
                        }
                        save(tray, format!("scheme.kind {kind}"));
                    }),
                    options: vec![
                        RadioItem {
                            label: "全拼".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "双拼".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: if s.wubi_ready {
                                "五笔".into()
                            } else {
                                "五笔（没有 wubi.db）".into()
                            },
                            enabled: s.wubi_ready,
                            ..Default::default()
                        },
                    ],
                }
                .into(),
            ],
            ..Default::default()
        }
        .into(),
        SubMenu {
            label: format!("双拼键位：{}", layout_label(&s.layout)),
            enabled: s.kind == "double-pinyin",
            submenu: vec![
                RadioGroup {
                    selected: layout_index(&s.layout),
                    select: Box::new(|tray: &mut PliersTray, index: usize| {
                        let layout = ["natural", "flypy", "mspy"][index.min(2)];
                        tray.snapshot.layout = layout.into();
                        save(tray, format!("scheme.layout {layout}"));
                    }),
                    options: vec![
                        RadioItem {
                            label: "自然码".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "小鹤".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "微软".into(),
                            ..Default::default()
                        },
                    ],
                }
                .into(),
            ],
            ..Default::default()
        }
        .into(),
        SubMenu {
            label: format!("中英切换键：{}", toggle_label(&s.toggle)),
            submenu: vec![
                RadioGroup {
                    selected: toggle_index(&s.toggle),
                    select: Box::new(|tray: &mut PliersTray, index: usize| {
                        let keys = ["ctrl+space", "shift", "ctrl+space,shift"][index.min(2)];
                        tray.snapshot.toggle = keys.into();
                        save(tray, format!("engine.toggle_keys {keys}"));
                    }),
                    options: vec![
                        RadioItem {
                            label: "Ctrl+空格".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "轻按 Shift".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "两个都要".into(),
                            ..Default::default()
                        },
                    ],
                }
                .into(),
            ],
            ..Default::default()
        }
        .into(),
        SubMenu {
            label: format!("候选个数：{}", s.candidates),
            submenu: vec![
                RadioGroup {
                    selected: candidates_index(s.candidates),
                    select: Box::new(|tray: &mut PliersTray, index: usize| {
                        let count = [5, 7, 9][index.min(2)];
                        tray.snapshot.candidates = count;
                        save(tray, format!("dict.max_candidates {count}"));
                    }),
                    options: vec![
                        RadioItem {
                            label: "5".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "7".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "9".into(),
                            ..Default::default()
                        },
                    ],
                }
                .into(),
            ],
            ..Default::default()
        }
        .into(),
        MenuItem::Separator,
        CheckmarkItem {
            label: "整句候选".into(),
            checked: s.sentence,
            enabled: s.kind != "wubi",
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.sentence;
                tray.snapshot.sentence = next;
                save(tray, format!("scheme.sentence {next}"));
            }),
            ..Default::default()
        }
        .into(),
        CheckmarkItem {
            label: "中文标点".into(),
            checked: s.punctuation,
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.punctuation;
                tray.snapshot.punctuation = next;
                save(tray, format!("engine.chinese_punctuation {next}"));
            }),
            ..Default::default()
        }
        .into(),
        CheckmarkItem {
            label: "英文候选".into(),
            checked: s.english,
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.english;
                tray.snapshot.english = next;
                save(tray, format!("english.enabled {next}"));
            }),
            ..Default::default()
        }
        .into(),
        CheckmarkItem {
            label: "中英提示".into(),
            checked: s.indicator,
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.indicator;
                tray.snapshot.indicator = next;
                save(tray, format!("engine.indicator {next}"));
            }),
            ..Default::default()
        }
        .into(),
        MenuItem::Separator,
        StandardItem {
            label: "重新读配置".into(),
            activate: Box::new(|tray: &mut PliersTray| {
                fire(tray.socket.clone(), "reload");
            }),
            ..Default::default()
        }
        .into(),
    ]
}

/// 一层平铺：每组一个灰色标题 + 直接的选项，点一下就能选，不会被盖住
fn flat_items(s: &Snapshot) -> Vec<MenuItem<PliersTray>> {
    let mut items: Vec<MenuItem<PliersTray>> = Vec::new();

    items.push(section(format!("输入方案：{}", kind_label(&s.kind))));
    items.push(
        RadioGroup {
            selected: kind_index(&s.kind),
            select: Box::new(|tray: &mut PliersTray, index: usize| {
                let kind = ["full-pinyin", "double-pinyin", "wubi"][index.min(2)];
                tray.snapshot.kind = kind.into();
                if kind != "double-pinyin" {
                    tray.snapshot.layout.clear();
                }
                if kind == "wubi" {
                    tray.snapshot.sentence = false;
                }
                save(tray, format!("scheme.kind {kind}"));
            }),
            options: vec![
                RadioItem {
                    label: "全拼".into(),
                    ..Default::default()
                },
                RadioItem {
                    label: "双拼".into(),
                    ..Default::default()
                },
                RadioItem {
                    label: if s.wubi_ready {
                        "五笔".into()
                    } else {
                        "五笔（无 wubi.db）".into()
                    },
                    enabled: s.wubi_ready,
                    ..Default::default()
                },
            ],
        }
        .into(),
    );

    // 不是双拼时这一组整块收起来：灰着一整组反而更碍事
    if s.kind == "double-pinyin" {
        items.push(section(format!("双拼键位：{}", layout_label(&s.layout))));
        items.push(
            RadioGroup {
                selected: layout_index(&s.layout),
                select: Box::new(|tray: &mut PliersTray, index: usize| {
                    let layout = ["natural", "flypy", "mspy"][index.min(2)];
                    tray.snapshot.layout = layout.into();
                    save(tray, format!("scheme.layout {layout}"));
                }),
                options: vec![
                    RadioItem {
                        label: "自然码".into(),
                        ..Default::default()
                    },
                    RadioItem {
                        label: "小鹤".into(),
                        ..Default::default()
                    },
                    RadioItem {
                        label: "微软".into(),
                        ..Default::default()
                    },
                ],
            }
            .into(),
        );
    }

    items.push(section(format!("中英切换键：{}", toggle_label(&s.toggle))));
    items.push(
        RadioGroup {
            selected: toggle_index(&s.toggle),
            select: Box::new(|tray: &mut PliersTray, index: usize| {
                let keys = ["ctrl+space", "shift", "ctrl+space,shift"][index.min(2)];
                tray.snapshot.toggle = keys.into();
                save(tray, format!("engine.toggle_keys {keys}"));
            }),
            options: vec![
                RadioItem {
                    label: "Ctrl+空格".into(),
                    ..Default::default()
                },
                RadioItem {
                    label: "轻按 Shift".into(),
                    ..Default::default()
                },
                RadioItem {
                    label: "两个都要".into(),
                    ..Default::default()
                },
            ],
        }
        .into(),
    );

    items.push(section(format!("候选个数：{}", s.candidates)));
    items.push(
        RadioGroup {
            selected: candidates_index(s.candidates),
            select: Box::new(|tray: &mut PliersTray, index: usize| {
                let count = [5, 7, 9][index.min(2)];
                tray.snapshot.candidates = count;
                save(tray, format!("dict.max_candidates {count}"));
            }),
            options: vec![
                RadioItem {
                    label: "5 个".into(),
                    ..Default::default()
                },
                RadioItem {
                    label: "7 个".into(),
                    ..Default::default()
                },
                RadioItem {
                    label: "9 个".into(),
                    ..Default::default()
                },
            ],
        }
        .into(),
    );

    items.push(MenuItem::Separator);
    items.push(
        CheckmarkItem {
            label: "整句候选".into(),
            checked: s.sentence,
            enabled: s.kind != "wubi",
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.sentence;
                tray.snapshot.sentence = next;
                save(tray, format!("scheme.sentence {next}"));
            }),
            ..Default::default()
        }
        .into(),
    );
    items.push(
        CheckmarkItem {
            label: "中文标点".into(),
            checked: s.punctuation,
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.punctuation;
                tray.snapshot.punctuation = next;
                save(tray, format!("engine.chinese_punctuation {next}"));
            }),
            ..Default::default()
        }
        .into(),
    );
    items.push(
        CheckmarkItem {
            label: "英文候选".into(),
            checked: s.english,
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.english;
                tray.snapshot.english = next;
                save(tray, format!("english.enabled {next}"));
            }),
            ..Default::default()
        }
        .into(),
    );
    items.push(
        CheckmarkItem {
            label: "中英提示".into(),
            checked: s.indicator,
            activate: Box::new(|tray: &mut PliersTray| {
                let next = !tray.snapshot.indicator;
                tray.snapshot.indicator = next;
                save(tray, format!("engine.indicator {next}"));
            }),
            ..Default::default()
        }
        .into(),
    );

    items.push(MenuItem::Separator);
    items.push(
        StandardItem {
            label: "重新读配置".into(),
            activate: Box::new(|tray: &mut PliersTray| {
                fire(tray.socket.clone(), "reload");
            }),
            ..Default::default()
        }
        .into(),
    );

    items
}

/// 菜单里的分组标题：灰掉、点不了，只给下面几项起个名（顺便显示当前值）
fn section(label: String) -> MenuItem<PliersTray> {
    StandardItem {
        label,
        enabled: false,
        ..Default::default()
    }
    .into()
}

/// 后台发一条控制命令：菜单/点击回调不能阻塞桌面的 DBus 线程
fn fire(socket: PathBuf, command: impl Into<String>) {
    let command = command.into();
    std::thread::spawn(move || {
        let _ = control::send(&socket, &command);
    });
}

/// 菜单里改一项：乐观更新 snapshot，真正的 `save`（写回 config.toml）交给后台
fn save(tray: &mut PliersTray, command: String) {
    fire(tray.socket.clone(), format!("save {command}"));
}

/// 起托盘。`socket` 是输入法自己的控制 socket（`Control::path()`）。
///
/// 起不来不是致命错误：没有 SNI host（比如纯 sway 没装 tray）就安静地不显示，
/// 输入法照常打字。`debug` 时才把原因打到 stderr。
pub fn spawn(socket: &Path, debug: bool) {
    let socket = socket.to_path_buf();
    let icons = Arc::new(render_icons());
    let tray = PliersTray {
        snapshot: Snapshot::default(),
        icons,
        socket: socket.clone(),
    };

    let handle = match tray.assume_sni_available(true).spawn() {
        Ok(handle) => handle,
        Err(e) => {
            if debug {
                eprintln!("pliers: 托盘图标没起来（{e}）");
            }
            return;
        }
    };

    let spawned = std::thread::Builder::new()
        .name("pliers-tray".into())
        .spawn(move || {
            // 主循环这时候才开始 serve_control：别只睡一个固定时间 ——
            // 合成器慢/机器忙的时候 200ms 可能还没轮到命令 socket。
            // 多试几次，真答上了再画，免得托盘先闪一下「?」
            let initial = (0..10)
                .find_map(|_| {
                    std::thread::sleep(Duration::from_millis(100));
                    read_state(&socket)
                })
                .unwrap_or_default();
            let _ = handle.update(|tray| tray.snapshot = initial.clone());
            poll(handle, socket, initial, debug);
        });
    if let Err(e) = spawned
        && debug
    {
        eprintln!("pliers: 托盘线程起不来：{e}");
    }
}

/// 每 500ms 问一次 `status`，快照变了就通知 SNI host 重画图标/菜单
fn poll(handle: Handle<PliersTray>, socket: PathBuf, initial: Snapshot, debug: bool) {
    let mut last = initial;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        // 这一轮没问到（正在重载配置/切词库，主循环一时没空）：保持上一份状态，
        // 别把图标翻成「?」—— 500ms 后接着问就是了
        let Some(state) = read_state(&socket) else {
            continue;
        };
        if state == last {
            continue;
        }
        last = state.clone();
        // update 返回 None = 托盘服务已经关了（桌面重启等），线程可以收工
        if handle.update(|tray| tray.snapshot = state).is_none() {
            return;
        }
        if debug {
            eprintln!("pliers: 托盘状态 → {}", mode_word(last.mode));
        }
    }
}

/// 问一次现状；没问着（socket 不在、实例正忙、回了个 ERR）就返回 `None`
fn read_state(socket: &Path) -> Option<Snapshot> {
    let reply = control::send(socket, "status").ok()?;
    if !reply.starts_with("OK") {
        return None;
    }
    Some(Snapshot::parse(&reply))
}

fn mode_of(value: &str) -> u8 {
    match value {
        "中" => CHINESE,
        "英" => ENGLISH,
        _ => UNKNOWN,
    }
}

fn mode_word(mode: u8) -> &'static str {
    match mode {
        CHINESE => "中文",
        ENGLISH => "英文",
        _ => "没在运行",
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "full-pinyin" => "全拼",
        "double-pinyin" => "双拼",
        "wubi" => "五笔",
        "" => "未知",
        _ => "未知",
    }
}

fn scheme_label(s: &Snapshot) -> String {
    match s.kind.as_str() {
        "full-pinyin" => "全拼".into(),
        "double-pinyin" => format!("双拼·{}", layout_label(&s.layout)),
        "wubi" => "五笔".into(),
        "" => "未知".into(),
        other => other.into(),
    }
}

fn layout_label(layout: &str) -> &'static str {
    match layout {
        "natural" => "自然码",
        "flypy" => "小鹤",
        "mspy" => "微软",
        "" => "—",
        _ => "自定义",
    }
}

fn toggle_label(toggle: &str) -> &'static str {
    match toggle {
        "ctrl+space" => "Ctrl+空格",
        "shift" => "轻按 Shift",
        "ctrl+space,shift" => "两个都要",
        "" => "无",
        _ => "自定义",
    }
}

// 下面几个 `index` 都是"当前值在选项里的下标"；认不出来的自定义值返回
// 选项个数（= 一个都不选中），别硬指到某个预设上、误导人
fn kind_index(kind: &str) -> usize {
    match kind {
        "full-pinyin" => 0,
        "double-pinyin" => 1,
        "wubi" => 2,
        _ => 3,
    }
}

fn layout_index(layout: &str) -> usize {
    match layout {
        "natural" => 0,
        "flypy" => 1,
        "mspy" => 2,
        _ => 3,
    }
}

fn toggle_index(toggle: &str) -> usize {
    match toggle {
        "ctrl+space" => 0,
        "shift" => 1,
        "ctrl+space,shift" => 2,
        _ => 3,
    }
}

fn candidates_index(count: usize) -> usize {
    match count {
        5 => 0,
        7 => 1,
        9 => 2,
        _ => 3,
    }
}

fn render_icons() -> Icons {
    let painter = pliers_popup::Painter::new();
    Icons {
        unknown: glyphs(&painter, "?"),
        chinese: glyphs(&painter, "中"),
        english: glyphs(&painter, "英"),
    }
}

/// 同一套图标画 22px / 33px / 44px 三份，托盘按自己的尺寸挑。
/// 透明底、不填色 —— 颜色交给桌面主题，中/英靠字区分。
fn glyphs(painter: &pliers_popup::Painter, text: &str) -> Vec<Icon> {
    // 22 = 1x，33 = 1.5x，44 = 2x。给 1.5x 屏一张整数倍的图，避免被缩放糊掉
    [22, 33, 44]
        .into_iter()
        .map(|size| to_icon(&painter.render_glyph(text, size, INK)))
        .collect()
}

/// 候选框那套 BGRA **预乘** → SNI 要的 ARGB（网络字节序、非预乘）
fn to_icon(image: &pliers_popup::Image) -> Icon {
    let mut data = Vec::with_capacity(image.pixels.len());
    for pixel in image.pixels.as_chunks::<4>().0 {
        let (b, g, r, a) = (pixel[0], pixel[1], pixel[2], pixel[3]);
        let un = |channel: u8| -> u8 {
            if a == 0 {
                0
            } else {
                ((channel as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
            }
        };
        data.extend_from_slice(&[a, un(r), un(g), un(b)]);
    }
    Icon {
        width: image.width,
        height: image.height,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_pixel(b: u8, g: u8, r: u8, a: u8) -> pliers_popup::Image {
        pliers_popup::Image {
            width: 1,
            height: 1,
            pixels: vec![b, g, r, a],
        }
    }

    #[test]
    fn 预乘_bgra_转成_argb() {
        // 预乘的半透明像素：R=48,G=32,B=16,A=128
        // 反预乘后 R=96,G=64,B=32，SNI 要的是 A,R,G,B
        let icon = to_icon(&one_pixel(16, 32, 48, 128));
        assert_eq!(icon.data, [128, 96, 64, 32]);

        // 不透明像素原样搬，只换字节序
        let icon = to_icon(&one_pixel(1, 2, 3, 255));
        assert_eq!(icon.data, [255, 3, 2, 1]);

        // 全透明像素就是全零
        let icon = to_icon(&one_pixel(9, 9, 9, 0));
        assert_eq!(icon.data, [0, 0, 0, 0]);
    }

    #[test]
    fn 解析_status_快照() {
        let reply = "OK\tkind=double-pinyin\tlayout=flypy\tsentence=true\
                     \tdict=…\tcandidates=7\tpool=90\ttoggle=ctrl+space,shift\
                     \tindicator=false\tchinese_punctuation=true\tenglish=false\
                     \tenglish_limit=5\tmode=英";
        let snapshot = Snapshot::parse(reply);
        assert_eq!(snapshot.mode, ENGLISH);
        assert_eq!(snapshot.kind, "double-pinyin");
        assert_eq!(snapshot.layout, "flypy");
        assert!(snapshot.sentence);
        assert!(snapshot.punctuation);
        assert!(!snapshot.english);
        assert!(!snapshot.indicator);
        assert_eq!(snapshot.candidates, 7);
        assert_eq!(snapshot.toggle, "ctrl+space,shift");
    }

    fn tray_with(kind: &str) -> PliersTray {
        PliersTray {
            snapshot: Snapshot {
                mode: CHINESE,
                kind: kind.into(),
                layout: if kind == "double-pinyin" {
                    "flypy".into()
                } else {
                    String::new()
                },
                sentence: true,
                punctuation: true,
                english: true,
                indicator: true,
                candidates: 9,
                toggle: "ctrl+space".into(),
                wubi_ready: false,
                menu: "grouped".into(),
            },
            icons: Arc::new(Icons {
                unknown: Vec::new(),
                chinese: Vec::new(),
                english: Vec::new(),
            }),
            socket: PathBuf::new(),
        }
    }

    fn radio_groups(menu: &[MenuItem<PliersTray>]) -> Vec<&RadioGroup<PliersTray>> {
        menu.iter()
            .filter_map(|item| match item {
                MenuItem::RadioGroup(group) => Some(group),
                _ => None,
            })
            .collect()
    }

    fn flat_tray(kind: &str) -> PliersTray {
        let mut tray = tray_with(kind);
        tray.snapshot.menu = "flat".into();
        tray
    }

    #[test]
    fn 平铺菜单只有一层() {
        // 平铺模式是为了绕开 Noctalia 的"二级菜单退不回一级"：选项全在同一层
        let menu = flat_tray("double-pinyin").menu();
        assert!(
            menu.iter()
                .all(|item| !matches!(item, MenuItem::SubMenu(_))),
            "平铺模式不该有 SubMenu"
        );

        let groups = radio_groups(&menu);
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].selected, 1); // 双拼
        assert_eq!(groups[1].selected, 1); // 小鹤
        assert_eq!(groups[2].selected, 0); // Ctrl+空格
        assert_eq!(groups[3].selected, 2); // 9 个
    }

    #[test]
    fn 分组菜单保持子菜单() {
        let menu = tray_with("double-pinyin").menu();
        let submenus: Vec<&ksni::menu::SubMenu<PliersTray>> = menu
            .iter()
            .filter_map(|item| match item {
                MenuItem::SubMenu(submenu) => Some(submenu),
                _ => None,
            })
            .collect();
        assert_eq!(submenus.len(), 5, "四个设置组 + 一个“关闭二级菜单”");
        assert!(
            submenus
                .iter()
                .any(|submenu| submenu.label == "关闭二级菜单"),
            "分组模式要有能收掉子菜单的项"
        );
    }

    #[test]
    fn 关闭二级菜单项带一个隐藏子项() {
        // 宿主只对 hasSubmenu 的项走 openSubmenuAtLevel（会先关旧子菜单）；
        // 所以“关闭”项必须带一个隐藏子项，但真正可显示的项是空的。
        let menu = tray_with("double-pinyin").menu();
        let close = menu
            .iter()
            .find_map(|item| match item {
                MenuItem::SubMenu(submenu) if submenu.label == "关闭二级菜单" => {
                    Some(submenu)
                }
                _ => None,
            })
            .expect("分组菜单里要有“关闭二级菜单”");
        assert_eq!(close.submenu.len(), 1);
        match &close.submenu[0] {
            MenuItem::Standard(item) => assert!(!item.visible, "子项必须是隐藏的"),
            _ => panic!("子项应该是个隐藏的 StandardItem"),
        }
    }

    #[test]
    fn 平铺模式不是双拼就不占键位那一组() {
        let menu = flat_tray("full-pinyin").menu();
        assert_eq!(radio_groups(&menu).len(), 3);
    }

    #[test]
    fn 平铺菜单认不出的自定义值不硬指一个预设() {
        let mut tray = flat_tray("double-pinyin");
        tray.snapshot.toggle = "super+space".into();
        tray.snapshot.candidates = 6;
        let menu = tray.menu();
        let groups = radio_groups(&menu);
        assert_eq!(groups[2].selected, 3, "自定义切换键不该冒充 Ctrl+空格");
        assert_eq!(groups[3].selected, 3, "6 个候选不该冒充 9 个");
    }

    #[test]
    fn 两种菜单都有样式开关() {
        for style in ["grouped", "flat"] {
            let mut tray = tray_with("double-pinyin");
            tray.snapshot.menu = style.into();
            let menu = tray.menu();
            let labels: Vec<&str> = menu
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Standard(item) => Some(item.label.as_str()),
                    _ => None,
                })
                .collect();
            let want = if style == "flat" {
                "切换为分组菜单"
            } else {
                "切换为平铺菜单"
            };
            assert!(labels.contains(&want), "{style} 模式缺样式开关");
        }
    }
}
