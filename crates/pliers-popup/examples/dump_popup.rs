//! 把候选框画成 PNG —— 不开输入法、不用合成器，直接看长相：
//!
//! ```text
//! cargo run -p pliers-popup --example dump_popup [输出路径]
//! ```
//!
//! 默认写到 `target/popup.png`。输出会放大 3 倍，底下垫棋盘格：
//! 能一眼看出哪里是透明的（圆角、框底那点半透明）。

use pliers_popup::{Image, Painter, Preedit};

/// 输出放大几倍
const ZOOM: u32 = 3;
/// 棋盘格边长（放大后的像素）
const CHECKER: u32 = 12;
/// 每行之间留多少空隙
const MARGIN: u32 = 16;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/popup.png".to_string());

    let painter = Painter::new();
    println!("找到字体：{}", painter.has_font());

    // 几组典型输入：主候选、换了候选、只有原文、候选多到快满
    let cases: Vec<(&str, Preedit)> = vec![
        (
            "nihao（主候选）",
            composition("nihao", &["你好", "尼好", "妮好", "拟好"], 0),
        ),
        (
            "nihao（选中第 3 个）",
            composition("nihao", &["你好", "尼好", "妮好", "拟好"], 2),
        ),
        (
            "ni（5 个候选）",
            composition("ni", &["你", "尼", "泥", "拟", "逆"], 0),
        ),
        ("ni（第 2 页 / 共 7 页，右下角是页码）", {
            let mut preedit = composition("ni", &["你", "尼", "泥", "拟", "逆"], 2);
            preedit.page = 1;
            preedit.pages = 7;
            preedit
        }),
        (
            "aaaa（查不到，只显示原文）",
            composition("aaaa", &["aaaa"], 0),
        ),
    ];

    let mut images: Vec<Image> = cases
        .iter()
        .map(|(_, preedit)| painter.render(preedit, 1))
        .collect();
    // 最后两行是中英文切换提示（不带序号的那种）
    let images_notice: Vec<(&str, Image)> = vec![
        ("切到中文的提示", painter.render_notice("中", 1)),
        ("切到英文的提示", painter.render_notice("英", 1)),
    ];
    let labels: Vec<&str> = cases
        .iter()
        .map(|(label, _)| *label)
        .chain(images_notice.iter().map(|(label, _)| *label))
        .collect();
    images.extend(images_notice.into_iter().map(|(_, image)| image));

    let width = images.iter().map(|i| i.width as u32).max().unwrap_or(1) * ZOOM + 2 * MARGIN;
    let height = images.iter().map(|i| i.height as u32).sum::<u32>() * ZOOM
        + MARGIN * (images.len() as u32 + 1);

    let mut canvas = checkerboard(width, height);
    let mut y = MARGIN;
    for (image, label) in images.iter().zip(&labels) {
        composite(&mut canvas, width, MARGIN, y, image);
        println!("{label}: {}x{} 逻辑像素", image.width, image.height);
        y += image.height as u32 * ZOOM + MARGIN;
    }

    write_png(&path, width, height, &canvas).expect("写 PNG");
    println!("写到 {path}");
}

fn composition(text: &str, candidates: &[&str], selected: usize) -> Preedit {
    Preedit {
        text: text.to_string(),
        candidates: candidates.iter().map(|s| s.to_string()).collect(),
        selected,
        page: 0,
        pages: 1,
    }
}

/// 灰白棋盘格当底：候选框哪里透明，这里就能透出来
fn checkerboard(width: u32, height: u32) -> Vec<u8> {
    let mut px = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let dark = (x / CHECKER + y / CHECKER).is_multiple_of(2);
            let v = if dark { 0x66 } else { 0x99 };
            let i = ((y * width + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[v, v, v, 0xFF]);
        }
    }
    px
}

/// 把候选框贴到棋盘格上（放大 ZOOM 倍）。
/// 候选框的像素是**预乘**过的 BGRA，所以直接用 over 公式叠上去就行
fn composite(canvas: &mut [u8], canvas_width: u32, x0: u32, y0: u32, image: &Image) {
    for y in 0..image.height as u32 {
        for x in 0..image.width as u32 {
            let i = ((y * image.width as u32 + x) * 4) as usize;
            let (b, g, r, a) = (
                image.pixels[i] as u32,
                image.pixels[i + 1] as u32,
                image.pixels[i + 2] as u32,
                image.pixels[i + 3] as u32,
            );
            let keep = 255 - a;
            for dy in 0..ZOOM {
                for dx in 0..ZOOM {
                    let (px, py) = (x0 + x * ZOOM + dx, y0 + y * ZOOM + dy);
                    let j = ((py * canvas_width + px) * 4) as usize;
                    // 画布是 RGBA，候选框像素是 BGRA，这里要换回来
                    for (channel, src) in [r, g, b].into_iter().enumerate() {
                        canvas[j + channel] = (src + canvas[j + channel] as u32 * keep / 255) as u8;
                    }
                    canvas[j + 3] = 0xFF;
                }
            }
        }
    }
}

/// 存成 PNG（用 png crate，dev-dependency，不进正式二进制）
fn write_png(
    path: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(std::fs::File::create(path)?),
        width,
        height,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}
