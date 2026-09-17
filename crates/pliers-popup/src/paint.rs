//! 画布：一块 ARGB8888 的像素缓冲，加两个图元 —— 圆角矩形和一行字。
//!
//! 候选框总共就这两样东西，所以不值得上一个 2D 绘图库。
//!
//! **字节序和预乘**（`wl_shm` 的 ARGB8888 就是这个规矩，写错了颜色会发亮）：
//! 每个像素在内存里是 `B G R A`；而且 alpha 是**预乘**的 ——
//! 50% 透明的白色要存成 `(128,128,128,128)`，不是 `(255,255,255,128)`。

use crate::font::Raster;

/// 一块画布，`pixels` 可以直接丢给 `wl_shm`
pub struct Canvas {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

impl Canvas {
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
        }
    }

    /// 把一个颜色（直通 alpha）按覆盖率混到某个像素上：source-over，预乘
    fn blend(&mut self, x: i32, y: i32, color: [u8; 4], coverage: f32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let src = color[3] as f32 / 255.0 * coverage.clamp(0.0, 1.0);
        if src <= 0.0 {
            return;
        }
        let keep = 1.0 - src;
        let i = ((y * self.width + x) * 4) as usize;
        let dst = &mut self.pixels[i..i + 4];
        for channel in 0..3 {
            // 存的是 B G R，给的是 R G B，所以倒着取
            dst[channel] =
                (color[2 - channel] as f32 * src + dst[channel] as f32 * keep).round() as u8;
        }
        dst[3] = (255.0 * src + dst[3] as f32 * keep).round() as u8;
    }

    /// 圆角矩形。边缘用"到圆角矩形的距离"当覆盖率，天然带抗锯齿
    pub fn round_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        color: [u8; 4],
    ) {
        let radius = radius.min(width / 2.0).min(height / 2.0).max(0.0);
        for py in (y.floor() as i32)..(y + height).ceil() as i32 {
            for px in (x.floor() as i32)..(x + width).ceil() as i32 {
                let coverage = coverage_of(
                    px as f32 + 0.5,
                    py as f32 + 0.5,
                    x,
                    y,
                    width,
                    height,
                    radius,
                );
                self.blend(px, py, color, coverage);
            }
        }
    }

    /// 贴一行已经光栅化好的字：`rax` 是整行的起点，`baseline` 是基线
    pub fn glyphs(&mut self, rasters: &[Raster], rax: f32, baseline: f32, color: [u8; 4]) {
        for raster in rasters {
            let gx = (rax + raster.x).round() as i32 + raster.left;
            let gy = baseline.round() as i32 + raster.top;
            for row in 0..raster.height as i32 {
                for col in 0..raster.width as i32 {
                    let coverage =
                        raster.cov[(row * raster.width as i32 + col) as usize] as f32 / 255.0;
                    self.blend(gx + col, gy + row, color, coverage);
                }
            }
        }
    }
}

/// 点到圆角矩形的有符号距离 → 覆盖率（0 = 完全在外，1 = 完全在内）。
/// 经典写法：先按"圆心矩形"算距离，再减去圆角半径
fn coverage_of(px: f32, py: f32, x: f32, y: f32, width: f32, height: f32, radius: f32) -> f32 {
    let dx = (px - (x + width / 2.0)).abs() - (width / 2.0 - radius);
    let dy = (py - (y + height / 2.0)).abs() - (height / 2.0 - radius);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    let distance = outside + inside - radius;
    // 距离 0 附近 1 个像素宽的过渡带就是抗锯齿
    (0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF];

    fn at(canvas: &Canvas, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * canvas.width + x) * 4) as usize;
        [
            canvas.pixels[i],
            canvas.pixels[i + 1],
            canvas.pixels[i + 2],
            canvas.pixels[i + 3],
        ]
    }

    #[test]
    fn 新建的画布是全透明的() {
        let canvas = Canvas::new(8, 4);
        assert_eq!(canvas.pixels.len(), 8 * 4 * 4);
        assert!(canvas.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn 实心矩形存的是预乘过的_bgra() {
        let mut canvas = Canvas::new(10, 10);
        canvas.round_rect(0.0, 0.0, 10.0, 10.0, 0.0, RED);
        assert_eq!(at(&canvas, 5, 5), [0x00, 0x00, 0xFF, 0xFF]); // B G R A
    }

    #[test]
    fn 半透明会被预乘() {
        let mut canvas = Canvas::new(4, 4);
        canvas.round_rect(0.0, 0.0, 4.0, 4.0, 0.0, [0xFF, 0xFF, 0xFF, 0x80]);
        let [b, g, r, a] = at(&canvas, 2, 2);
        assert_eq!(a, 128);
        // 预乘：通道值应该跟 alpha 差不多，而不是 255
        assert!(
            r.abs_diff(a) <= 1 && g == r && b == r,
            "预乘错了：{r} {g} {b} {a}"
        );
    }

    #[test]
    fn 圆角把四个角削掉了() {
        let mut canvas = Canvas::new(20, 20);
        canvas.round_rect(0.0, 0.0, 20.0, 20.0, 8.0, RED);
        assert_eq!(at(&canvas, 10, 10)[3], 255); // 中心实心
        assert_eq!(at(&canvas, 0, 0)[3], 0); // 角上全透明
        assert_eq!(at(&canvas, 19, 0)[3], 0);
        // 边的中点还在
        assert_eq!(at(&canvas, 10, 0)[3], 255);
    }

    #[test]
    fn 重叠的地方不会越界写() {
        let mut canvas = Canvas::new(6, 6);
        // 故意画到画布外面去
        canvas.round_rect(-4.0, -4.0, 20.0, 20.0, 2.0, RED);
        canvas.glyphs(&[], -100.0, -100.0, RED);
        assert_eq!(canvas.pixels.len(), 6 * 6 * 4);
    }
}
