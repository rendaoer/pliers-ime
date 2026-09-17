//! 候选框的"内容"：找字体、排版、画像素、给共享内存。完全不碰 Wayland。
//!
//! 之所以把这层单独拆出来：**画什么**和**怎么贴到合成器上**是两件事。以后想换成
//! egui / Slint / tiny-skia 来画同一个框，只要还吐出同样格式的像素，协议那侧一行都不用改。
//!
//! 对外只有一个入口：[`Painter::render`] 把引擎给的 [`Preedit`] 变成一块
//! ARGB8888 像素（`wl_shm` 能直接用的那种），再加一个 [`shm_file`] 负责弄块内存。
//!
//! 想看画出来什么样、又不想开输入法：
//!
//! ```text
//! cargo run -p pliers-popup --example dump_popup        # 存成 PNG
//! ```

mod font;
mod paint;

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

// 候选框要显示的就是引擎给的那份组词状态，原样重导出一份，
// 免得用的人还得同时依赖 pliers-engine
pub use pliers_engine::Preedit;

use font::{Font, Raster, ink_bounds};
use paint::Canvas;

// ---- 版面（逻辑像素；真正画的时候统一乘屏幕缩放）----------------------------

/// 候选框高度
const HEIGHT: f32 = 42.0;
/// 整框左右留白
const PAD: f32 = 10.0;
/// 每个候选自己的左右留白
const ITEM_PAD: f32 = 10.0;
/// 候选之间的间隔
const GAP: f32 = 6.0;
/// 序号和词之间的间隔
const INDEX_GAP: f32 = 6.0;
/// 整框圆角
const RADIUS: f32 = 10.0;
/// 选中项那块"药丸"的圆角，以及它上下比整框缩进多少
const ITEM_RADIUS: f32 = 7.0;
const ITEM_INSET: f32 = 4.0;
/// 词和序号的字号
const FONT_SIZE: f32 = 19.0;
const INDEX_SIZE: f32 = 11.0;
/// 最多显示几个候选（超出的要翻页，这里先不做）
const MAX_ITEMS: usize = 9;
/// 单个候选最多画几个字（防止某个词特别长把框撑爆）
const MAX_CHARS: usize = 24;

// ---- 配色（直通 alpha，画的时候才预乘）--------------------------------------
// 字节序是 R G B A，跟像素里的 B G R A 不一样，paint 模块负责倒过来

/// 框底：深灰，稍微透一点，底下是啥还能隐约看见
const BG: [u8; 4] = [0x20, 0x20, 0x24, 0xF0];
/// 普通候选的字
const FG: [u8; 4] = [0xEE, 0xEE, 0xF2, 0xFF];
/// 序号（暗一点，别抢戏）
const DIM: [u8; 4] = [0x9A, 0x9A, 0xA2, 0xFF];
/// 选中项：橙底 + 深色字
const SEL_BG: [u8; 4] = [0xFF, 0xA8, 0x4F, 0xFF];
const SEL_FG: [u8; 4] = [0x1A, 0x14, 0x0C, 0xFF];

/// 一帧画好的候选框，`pixels` 就是 `wl_shm` 里要的那块字节
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

impl Image {
    /// 每行多少字节（ARGB8888 = 每像素 4 字节）
    pub fn stride(&self) -> i32 {
        self.width * 4
    }
}

/// 画候选框的东西：其实就是一套字体，画一帧用一次
pub struct Painter {
    font: Option<Font>,
}

impl Default for Painter {
    fn default() -> Self {
        Self::new()
    }
}

impl Painter {
    /// 找字体要扫一遍系统字体目录（本机 30ms 左右），所以整个进程只做一次
    pub fn new() -> Self {
        let font = Font::load();
        if std::env::var_os("PLIERS_DEBUG").is_some() {
            match &font {
                Some(font) => eprintln!(
                    "pliers: 候选框字体 = {}（汉字{}）",
                    font.name(),
                    if font.covers('你') {
                        "有"
                    } else {
                        "没有！"
                    }
                ),
                None => eprintln!("pliers: 没找到字体，候选框只能画个空框"),
            }
        }
        Self { font }
    }

    /// 找到字体了吗（没找到的话框里没字，只有底和选中色块）
    pub fn has_font(&self) -> bool {
        self.font.is_some()
    }

    /// 画一帧。`scale` 是屏幕缩放：2 表示每个逻辑像素画成 2×2 个物理像素
    /// （合成器那边配 `wl_surface.set_buffer_scale`，不然会被当成两倍大）
    pub fn render(&self, preedit: &Preedit, scale: i32) -> Image {
        let scale = scale.clamp(1, 4) as f32;
        let items: Vec<Item> = preedit
            .candidates
            .iter()
            .take(MAX_ITEMS)
            .enumerate()
            .map(|(index, text)| {
                self.item(
                    &(index + 1).to_string(),
                    text,
                    index == preedit.selected,
                    scale,
                )
            })
            .collect();
        self.draw(items, scale)
    }

    /// 中英文模式提示（就一个「中」/「英」）：不带序号，别让它看着像个候选
    pub fn render_notice(&self, label: &str, scale: i32) -> Image {
        let scale = scale.clamp(1, 4) as f32;
        self.draw(vec![self.item("", label, true, scale)], scale)
    }

    /// 把排好版的候选画成像素
    fn draw(&self, items: Vec<Item>, scale: f32) -> Image {
        let px = |logical: f32| logical * scale;
        if items.is_empty() {
            // 没在组词。调用方本来就不该画（见 pliers-wayland 的 sync_popup），
            // 真调到了就交张空图，免得算出 0 宽还去建缓冲区
            return Image {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        }

        let height = px(HEIGHT).round();
        let width: f32 = px(2.0 * PAD)
            + items.iter().map(|item| item.width).sum::<f32>()
            + px(GAP) * (items.len() - 1) as f32;

        // 2. 竖直居中：按所有字的**墨迹**算基线，而不是字体的 ascent/descent
        //   （中文行高留白大，按行高居中会看着偏上）
        let top = items.iter().map(|item| item.ink.0).fold(f32::MAX, f32::min);
        let bottom = items.iter().map(|item| item.ink.1).fold(f32::MIN, f32::max);
        let baseline = (height - (bottom - top)) / 2.0 - top;

        // 3. 画：底 → 选中色块 → 字
        let mut canvas = Canvas::new(width.round() as i32, height.round() as i32);
        canvas.round_rect(0.0, 0.0, width, height, px(RADIUS), BG);
        let mut x = px(PAD);
        for item in &items {
            if item.selected {
                canvas.round_rect(
                    x,
                    px(ITEM_INSET),
                    item.width,
                    height - px(2.0 * ITEM_INSET),
                    px(ITEM_RADIUS),
                    SEL_BG,
                );
            }
            canvas.glyphs(&item.index, x + px(ITEM_PAD), baseline, DIM);
            let text_x = x
                + px(ITEM_PAD)
                + item.index_width
                + if item.index_width > 0.0 {
                    px(INDEX_GAP)
                } else {
                    0.0
                };
            canvas.glyphs(
                &item.text,
                text_x,
                baseline,
                if item.selected { SEL_FG } else { FG },
            );
            x += item.width + px(GAP);
        }

        Image {
            width: canvas.width,
            height: canvas.height,
            pixels: canvas.pixels,
        }
    }

    /// 排一个候选：`[序号] [词]`。`number` 给空串就是不带序号（模式提示用）
    fn item(&self, number: &str, text: &str, selected: bool, scale: f32) -> Item {
        // 词太长就砍掉：候选框不该因为词库里有个怪物词就横穿整个屏幕
        let text: String = text.chars().take(MAX_CHARS).collect();

        let (number_rasters, number_width) = self.rasterize(number, INDEX_SIZE, scale);
        let (text_rasters, text_width) = self.rasterize(&text, FONT_SIZE, scale);
        let gap = if number.is_empty() {
            0.0
        } else {
            INDEX_GAP * scale
        };
        let width = 2.0 * ITEM_PAD * scale + number_width + gap + text_width;
        // 序号和词的墨迹范围并起来，供整行竖直居中
        let ink = union(ink_bounds(&number_rasters), ink_bounds(&text_rasters));
        Item {
            index: number_rasters,
            text: text_rasters,
            index_width: number_width,
            width,
            ink,
            selected,
        }
    }

    /// 光栅化一小段字，返回 `(每个字的灰度图, 这段字多宽)`
    fn rasterize(&self, text: &str, size: f32, scale: f32) -> (Vec<Raster>, f32) {
        match &self.font {
            Some(font) => {
                let size = size * scale;
                (font.glyphs(text, size), font.width(text, size))
            }
            // 没字体也得能画：按"汉字一个字宽、字母半个字宽"估宽度，
            // 光栅是空的 —— 于是框的形状是对的，只是里面没字
            None => (Vec::new(), estimate_width(text, size * scale)),
        }
    }
}

/// 一个候选在版面里的样子
struct Item {
    /// 序号的光栅（x 相对本项左边）
    index: Vec<Raster>,
    /// 词的光栅（x 相对序号右边那段留白的起点）
    text: Vec<Raster>,
    index_width: f32,
    /// 整个候选占多宽
    width: f32,
    /// 字的墨迹范围，相对基线
    ink: (f32, f32),
    selected: bool,
}

fn union(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0.min(b.0), a.1.max(b.1))
}

/// 没有字体时的宽度估算
fn estimate_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|ch| if ch.is_ascii() { size * 0.55 } else { size })
        .sum()
}

/// 把一帧像素写进共享内存文件的 `offset` 处
pub fn write(file: &File, offset: u64, image: &Image) -> io::Result<()> {
    file.write_all_at(&image.pixels, offset)
}

/// 建一块共享内存文件当 wl_shm pool 的后备存储。
/// `/dev/shm` 是 tmpfs，性能最好；沙箱/容器里没有就退回临时目录。
/// 返回的 fd 一直开着，所以路径当场就删掉了，不会留垃圾文件。
pub fn shm_file(size: usize) -> io::Result<File> {
    let name = format!("pliers-popup-{}", std::process::id());
    let mut last_err = None;
    for dir in ["/dev/shm", "/tmp"] {
        let path = std::path::Path::new(dir).join(&name);
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path);
        match opened {
            Ok(file) => {
                file.set_len(size as u64)?;
                let _ = std::fs::remove_file(&path);
                return Ok(file);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("至少试过一个目录"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一份组词状态
    fn preedit(candidates: &[&str], selected: usize) -> Preedit {
        Preedit {
            text: "nihao".to_string(),
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            selected,
        }
    }

    /// 某个像素
    fn at(image: &Image, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * image.width + x) * 4) as usize;
        [
            image.pixels[i],
            image.pixels[i + 1],
            image.pixels[i + 2],
            image.pixels[i + 3],
        ]
    }

    #[test]
    fn 没候选就是空图() {
        let image = Painter::new().render(&Preedit::default(), 1);
        assert_eq!(image.width, 0);
        assert!(image.pixels.is_empty());
    }

    #[test]
    fn 框随候选个数变宽() {
        let painter = Painter::new();
        let one = painter.render(&preedit(&["你好"], 0), 1);
        let three = painter.render(&preedit(&["你好", "尼好", "妮好"], 0), 1);
        assert!(three.width > one.width);
        assert_eq!(one.height, three.height);
        assert_eq!(one.height, HEIGHT as i32);
    }

    #[test]
    fn 像素缓冲大小跟宽高对得上() {
        let image = Painter::new().render(&preedit(&["你好", "尼好"], 0), 1);
        assert_eq!(
            image.pixels.len(),
            (image.width * image.height * 4) as usize
        );
        assert_eq!(image.stride(), image.width * 4);
    }

    #[test]
    fn 四角是透明的圆角() {
        let image = Painter::new().render(&preedit(&["你好"], 0), 1);
        assert_eq!(at(&image, 0, 0)[3], 0, "左上角该被圆角削掉");
        assert_eq!(at(&image, image.width - 1, 0)[3], 0);
        // 框中间是不透明的深灰底
        let middle = at(&image, image.width / 2, image.height - 2);
        assert!(middle[3] > 200, "框底该是不透明的：{middle:?}");
    }

    #[test]
    fn 选中项是橙色的() {
        let painter = Painter::new();
        let first = painter.render(&preedit(&["你好", "尼好"], 0), 1);
        let second = painter.render(&preedit(&["你好", "尼好"], 1), 1);
        // 两份图的尺寸一样，只有那块橙色药丸挪了位置 —— 直接比像素
        assert_eq!(first.width, second.width);
        assert_ne!(first.pixels, second.pixels);

        // 第一个候选所在的位置：选中时发橙（R 最大），没选中时是深灰底
        let x = 14;
        let y = first.height / 2;
        assert!(at(&first, x, y)[2] > 200, "选中项该是橙底");
        assert!(at(&second, x, y)[2] < 100, "没选中就是深灰底");
    }

    /// 数一数某个横向区间里满足条件的像素
    fn count(image: &Image, from_x: i32, to_x: i32, ok: impl Fn(u8, u8, u8) -> bool) -> usize {
        let mut n = 0;
        for y in 0..image.height {
            for x in from_x..to_x {
                let [b, g, r, _] = at(image, x, y);
                if ok(r, g, b) {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn 框里真的画了字() {
        let painter = Painter::new();
        if !painter.has_font() {
            return; // 没中文字体的机器上跳过
        }
        // 两个候选、选中第二个：左半边是"深底浅字"，数它的笔画像素；
        // 右半边该是一大块橙色药丸
        let image = painter.render(&preedit(&["你好", "尼好"], 1), 1);
        let half = image.width / 2;
        let strokes = count(&image, 0, half, |r, g, b| r > 150 && g > 150 && b > 150);
        let pill = count(&image, half, image.width, |r, g, b| {
            r > 200 && (120..200).contains(&g) && b < 120
        });
        assert!(
            strokes > 100,
            "左边候选该有上百个浅色笔画像素，实际 {strokes}"
        );
        assert!(pill > 500, "右边该有一大块橙色（选中的那个），实际 {pill}");
    }

    #[test]
    fn 缩放两倍像素翻倍() {
        let painter = Painter::new();
        let one = painter.render(&preedit(&["你好"], 0), 1);
        let two = painter.render(&preedit(&["你好"], 0), 2);
        assert_eq!(two.width, one.width * 2);
        assert_eq!(two.height, one.height * 2);
    }

    #[test]
    fn 能写进共享内存文件() {
        let image = Painter::new().render(&preedit(&["你好"], 0), 1);
        let file = shm_file(image.pixels.len() + 64).expect("建共享内存文件");
        write(&file, 64, &image).expect("写像素");

        let mut back = vec![0u8; image.pixels.len()];
        file.read_exact_at(&mut back, 64).expect("读回来");
        assert_eq!(back, image.pixels);
    }
}
