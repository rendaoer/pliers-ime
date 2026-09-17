//! 找一套能显示中文的字体，并把字符变成灰度小图。
//!
//! 这是整个候选框里唯一"看系统环境"的地方：字体不在我们手里，得去系统里翻
//! （fontdb 会扫 /usr/share/fonts、~/.local/share/fonts 这些目录）。
//! 找不到也照样能画框，只是框里没字 —— 所以这里的失败一律是"降级"而不是报错。
//!
//! 光栅化用 ab_glyph：候选框只有一行短语，用不上换行 / 双向文字 / 复杂脚本整形，
//! "字符 → 轮廓 → 灰度图"这三步就够了。形状复杂的脚本（阿拉伯文、天城文）得换
//! cosmic-text 那种带整形（shaping）的库，中文和拉丁文不需要。

use std::path::Path;

use ab_glyph::{Font as _, FontVec, Glyph, PxScale, ScaleFont};

/// 优先找的字体家族，越靠前越想要。前几个覆盖了绝大多数 Linux 发行版装的中文字体
const FAMILIES: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "Noto Sans CJK TC",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
    "Microsoft YaHei",
    "PingFang SC",
];

/// 一个字光栅化之后的样子。
///
/// `x` 是它在整行里的横向位置，`(left, top)` 是它相对**基线原点**的位置
/// （x 向右、y 向下，画的时候只要把灰度图贴到 `(x + left, 基线 + top)` 就行）。
pub struct Raster {
    pub x: f32,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// width×height 的覆盖率（0..255），行优先
    pub cov: Vec<u8>,
}

/// 一组字画出来占的竖直范围 `(上, 下)`，相对基线，上小下大。
/// 竖直居中的时候用它 —— 用字体的 ascent/descent 居中会偏：
/// Noto CJK 的行高留白很大，汉字实际只占中间一小条
pub fn ink_bounds(rasters: &[Raster]) -> (f32, f32) {
    let mut top = f32::MAX;
    let mut bottom = f32::MIN;
    for raster in rasters {
        top = top.min(raster.top as f32);
        bottom = bottom.max((raster.top + raster.height as i32) as f32);
    }
    if top > bottom {
        (0.0, 0.0)
    } else {
        (top, bottom)
    }
}

/// 一套加载好的字体
pub struct Font {
    inner: FontVec,
    /// 字体名（只用来打日志：候选框里全是豆腐块的时候好知道选错哪套字体了）
    name: String,
    /// ab_glyph 的 `PxScale` 是**行高**不是字号：中文行高一般是字号的 1.4 倍上下，
    /// 直接把字号填进去，汉字会小一大圈。这里存下换算比例，
    /// 让下面所有 `size` 参数都是真正的字号（em）
    ratio: f32,
}

impl Font {
    /// 找一套有中文的字体：先看 `IME_AA_FONT` 环境变量，再问系统
    pub fn load() -> Option<Font> {
        if let Some(path) = std::env::var_os("IME_AA_FONT") {
            let name = format!("{path:?}（IME_AA_FONT 指定）");
            match std::fs::read(Path::new(&path))
                .ok()
                .and_then(|data| Font::from_data(data, 0, name))
            {
                Some(font) => return Some(font),
                None => eprintln!("ime-aa: 读不出 IME_AA_FONT={path:?}，改用系统字体"),
            }
        }

        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut families: Vec<fontdb::Family> = FAMILIES
            .iter()
            .map(|name| fontdb::Family::Name(name))
            .collect();
        families.push(fontdb::Family::SansSerif); // 实在没有中文字体就用系统默认的
        let query = fontdb::Query {
            families: &families,
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        };
        let id = db.query(&query)?;
        let name = db.face(id)?.post_script_name.clone();
        let font = db.with_face_data(id, |data, index| {
            Font::from_data(data.to_vec(), index, name)
        })??;
        if !font.covers('你') {
            eprintln!(
                "ime-aa: 选中的字体没有中文字形（装个 noto-fonts-cjk，\
                 或者用 IME_AA_FONT=/path/to/font.ttc 指定一套）"
            );
        }
        Some(font)
    }

    /// 这个字体叫什么（日志用）
    pub fn name(&self) -> &str {
        &self.name
    }

    fn from_data(data: Vec<u8>, index: u32, name: String) -> Option<Font> {
        let inner = FontVec::try_from_vec_and_index(data, index).ok()?;
        let upem = inner.units_per_em().unwrap_or(1000.0);
        let ratio = inner.height_unscaled() / upem;
        let ratio = if ratio.is_finite() && ratio > 0.1 {
            ratio
        } else {
            1.0
        };
        Some(Font { inner, name, ratio })
    }

    /// 这套字体有某个字的字形吗（没有的话画出来是空白）
    pub fn covers(&self, ch: char) -> bool {
        self.inner.glyph_id(ch).0 != 0
    }

    /// 字号 → ab_glyph 要的 PxScale
    fn px(&self, size: f32) -> PxScale {
        PxScale::from(size * self.ratio)
    }

    /// 一行字有多宽（含字距，不含任何留白）
    pub fn width(&self, text: &str, size: f32) -> f32 {
        let scaled = self.inner.as_scaled(self.px(size));
        text.chars()
            .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
            .sum()
    }

    /// 把一行字光栅化，返回每个字的位置和灰度图。
    ///
    /// 一帧也就十来个字，直接重画；真要抠性能的话可以在 `(char, size)` 上做缓存。
    pub fn glyphs(&self, text: &str, size: f32) -> Vec<Raster> {
        let scale = self.px(size);
        let scaled = self.inner.as_scaled(scale);
        let mut out = Vec::with_capacity(text.chars().count());
        let mut pen = 0.0f32;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            // 空格之类没有轮廓的字：只推进笔，不贴图
            let glyph = Glyph {
                id,
                scale,
                position: ab_glyph::point(0.0, 0.0),
            };
            if let Some(outlined) = scaled.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                let width = (bounds.max.x - bounds.min.x).round() as u32;
                let height = (bounds.max.y - bounds.min.y).round() as u32;
                let mut cov = vec![0u8; (width * height) as usize];
                if width > 0 && height > 0 {
                    outlined.draw(|x, y, coverage| {
                        if let Some(pixel) = cov.get_mut((y * width + x) as usize) {
                            *pixel = (coverage * 255.0).round() as u8;
                        }
                    });
                }
                out.push(Raster {
                    x: pen,
                    left: bounds.min.x.round() as i32,
                    top: bounds.min.y.round() as i32,
                    width,
                    height,
                    cov,
                });
            }
            pen += scaled.h_advance(id);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试里不挑字体：找得到就用（本机有 Noto CJK），找不到就跳过 ——
    /// 不然这个 crate 的测试在没装中文字体的机器上全红
    fn font() -> Option<Font> {
        Font::load()
    }

    #[test]
    fn 汉字比字母宽() {
        let Some(font) = font() else { return };
        let han = font.width("你", 20.0);
        let latin = font.width("n", 20.0);
        assert!(han > latin, "汉字 {han} 应该比字母 {latin} 宽");
        // 汉字是全角：字号多大就多宽（±10% 给不同字体的差别留余地）
        assert!((han - 20.0).abs() < 2.0, "20 号汉字宽 {han}，应该差不多 20");
    }

    #[test]
    fn 光栅化出来的字有墨() {
        let Some(font) = font() else { return };
        let rasters = font.glyphs("你好", 20.0);
        assert_eq!(rasters.len(), 2);
        for raster in &rasters {
            assert!(raster.width > 0 && raster.height > 0);
            assert_eq!(raster.cov.len(), (raster.width * raster.height) as usize);
            assert!(raster.cov.iter().any(|&c| c > 200), "应该有实心的笔画");
        }
        // 第二个字排在第一个字右边
        assert!(rasters[1].x >= rasters[0].x + 15.0);
    }

    #[test]
    fn 空格没有轮廓但也有宽度() {
        let Some(font) = font() else { return };
        assert!(font.glyphs(" ", 20.0).is_empty());
        assert!(font.width(" ", 20.0) > 0.0);
    }

    #[test]
    fn 墨迹范围贴着字() {
        let Some(font) = font() else { return };
        let rasters = font.glyphs("你好", 20.0);
        let (top, bottom) = ink_bounds(&rasters);
        // 相对基线：汉字往上占大半，往下探一点（不是整行 1.4 倍的行高）
        assert!(top < -12.0 && top > -20.0, "上边界 {top}");
        assert!(bottom > 0.0 && bottom < 6.0, "下边界 {bottom}");
        assert_eq!(ink_bounds(&[]), (0.0, 0.0));
    }
}
