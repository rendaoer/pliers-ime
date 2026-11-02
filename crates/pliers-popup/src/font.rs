//! 找一套能显示中文的字体，并把字符变成灰度小图。
//!
//! 这是整个候选框里唯一"看系统环境"的地方：字体不在我们手里，得去系统里翻
//! （fontdb 会扫 /usr/share/fonts、~/.local/share/fonts 这些目录）。
//! 找不到也照样能画框，只是框里没字 —— 所以这里的失败一律是"降级"而不是报错。
//!
//! 光栅化用 ab_glyph：候选框只有一行短语，用不上换行 / 双向文字 / 复杂脚本整形，
//! "字符 → 轮廓 → 灰度图"这三步就够了。形状复杂的脚本（阿拉伯文、天城文）得换
//! cosmic-text 那种带整形（shaping）的库，中文和拉丁文不需要。

use std::path::{Path, PathBuf};

use ab_glyph::{Font as _, FontVec, Glyph, PxScale, ScaleFont, VariableFont};

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

/// 家族名里带这些词的，多半是能画中文的字体（发行版名字五花八门，用来兜底）
const CJK_HINTS: &[&str] = &[
    "cjk",
    "source han",
    "noto sans sc",
    "noto serif sc",
    "wenquanyi",
    "wqy",
    "hei",
    "song",
    "kai",
    "ming",
    "yahei",
    "pingfang",
    "sarasa",
    "lxgw",
    "droid sans fallback",
    "unifont",
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
    /// 找一套有中文的字体：先看 `PLIERS_FONT` 环境变量，再问系统
    pub fn load() -> Option<Font> {
        if let Some(path) = std::env::var_os("PLIERS_FONT") {
            let name = format!("{path:?}（PLIERS_FONT 指定）");
            match std::fs::read(Path::new(&path))
                .ok()
                .and_then(|data| Font::from_data(data, 0, name))
            {
                Some(font) => return Some(font),
                None => eprintln!("pliers: 读不出 PLIERS_FONT={path:?}，改用系统字体"),
            }
        }

        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // fontdb 不认识 NixOS 的字体目录（它们不在 /usr/share/fonts，只在 fontconfig
        // 的配置里写着）。把 fontconfig 说过的目录补扫一遍，不然候选框会是空框
        for dir in extra_font_dirs() {
            db.load_fonts_dir(&dir);
        }

        // 1. 按喜欢的家族逐个试，**要求真能画出「你」**
        for family in FAMILIES {
            let Some(id) = best_face_for(&db, family) else {
                continue;
            };
            let Some(font) = load_face(&db, id) else {
                continue;
            };
            if font.covers('你') {
                return Some(font);
            }
        }

        // 2. 家族名里带 CJK 字样的（不同发行版叫法不一样，没法全列出来）
        let ids: Vec<fontdb::ID> = db.faces().map(|face| face.id).collect();
        for id in ids {
            if !looks_cjk(&db, id) {
                continue;
            }
            if let Some(font) = load_face(&db, id)
                && font.covers('你')
            {
                return Some(font);
            }
        }

        // 3. 系统默认字体：可能没有中文，但画页码、英文候选还是清楚的
        let query = fontdb::Query {
            families: &[fontdb::Family::SansSerif],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        };
        let fallback = db
            .query(&query)
            .or_else(|| db.faces().next().map(|face| face.id));
        let font = fallback.and_then(|id| load_face(&db, id))?;
        if !font.covers('你') {
            eprintln!(
                "pliers: 选中的字体没有中文字形（装个 noto-fonts-cjk，\
                 或者用 PLIERS_FONT=/path/to/font.ttc 指定一套）"
            );
        }
        Some(font)
    }

    /// 这个字体叫什么（日志用）
    pub fn name(&self) -> &str {
        &self.name
    }

    fn from_data(data: Vec<u8>, index: u32, name: String) -> Option<Font> {
        let mut inner = FontVec::try_from_vec_and_index(data, index).ok()?;
        // 可变字体（NixOS 的 Noto Sans CJK 就是 VF）默认实例常常是 **Thin**：
        // 不设轴的话，候选框里的字细得像没画。明确把字重放到 400（常规体）。
        // 不是可变字体的话 `set_variation` 直接返回 false，什么都不影响
        let variable = inner.set_variation(b"wght", 400.0);
        let name = if variable {
            // postscript 名字还是 "…-Thin"，不注明一下看日志会以为选错了
            format!("{name}（可变字体，字重设成 400）")
        } else {
            name
        };
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

/// 从 fontdb 里把某个字面加载出来
fn load_face(db: &fontdb::Database, id: fontdb::ID) -> Option<Font> {
    let name = db.face(id)?.post_script_name.clone();
    db.with_face_data(id, |data, index| {
        Font::from_data(data.to_vec(), index, name)
    })
    .flatten()
}

/// 某个家族里跟「常规体」最接近的那个字面。
///
/// 光用 `db.query()` 有个坑：Noto CJK 是 `.ttc` 里的可变字体，`query` 有时会
/// 挑到 Thin 那一份（`NotoSansCJKsc-Thin`），候选框里的字细得像没画。
/// 所以自己按家族 + weight 找，取离 400 最近的；找不到再退回 `query`
fn best_face_for(db: &fontdb::Database, family: &str) -> Option<fontdb::ID> {
    let mut best: Option<(fontdb::ID, u16)> = None;
    for face in db.faces() {
        if face.style != fontdb::Style::Normal {
            continue;
        }
        if !face.families.iter().any(|(name, _)| name == family) {
            continue;
        }
        let distance = face.weight.0.abs_diff(fontdb::Weight::NORMAL.0);
        if best.is_none_or(|(_, closest)| distance < closest) {
            best = Some((face.id, distance));
        }
    }
    best.map(|(id, _)| id).or_else(|| {
        db.query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        })
    })
}

/// 这套字体的家族名看起来像中文字体吗
fn looks_cjk(db: &fontdb::Database, id: fontdb::ID) -> bool {
    let Some(face) = db.face(id) else {
        return false;
    };
    face.families.iter().any(|(family, _)| {
        let family = family.to_lowercase();
        CJK_HINTS.iter().any(|hint| family.contains(hint))
    })
}

/// fontdb 的 `load_system_fonts()` 只看"教科书位置"（/usr/share/fonts 之类）。
/// NixOS 的字体全在 /nix/store，位置只写在 fontconfig 的配置文件里 ——
/// 这里把它们找出来补扫一遍。不链接 libfontconfig，也就没有额外的系统依赖。
fn extra_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. fontconfig 的配置文件（NixOS 在 /etc/fonts/conf.d/00-nixos-cache.conf 里
    //    写着 /nix/store/...-noto-fonts-cjk-...，标准位置里一个都没有）
    for conf in fontconfig_files() {
        if let Ok(text) = std::fs::read_to_string(&conf) {
            parse_fontconfig_dirs(&text, &mut dirs);
        }
    }

    // 2. 常见位置（fontdb 可能已经扫过，重复扫没关系）
    for dir in [
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/run/current-system/sw/share/fonts",
    ] {
        dirs.push(PathBuf::from(dir));
    }

    // 3. Nix profile / XDG data dir 下面的字体
    if let Some(value) = std::env::var_os("NIX_PROFILES") {
        for base in std::env::split_paths(&value) {
            dirs.push(base.join("share/fonts"));
        }
    }
    if let Some(value) = std::env::var_os("XDG_DATA_DIRS") {
        for base in std::env::split_paths(&value) {
            dirs.push(base.join("fonts"));
        }
    }

    // 4. 用户自己的字体目录
    if let Some(home) = home() {
        dirs.push(home.join(".nix-profile/share/fonts"));
        dirs.push(home.join(".local/state/nix/profile/share/fonts"));
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }

    // 去重 + 只留真存在的目录
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|dir| dir.is_dir() && seen.insert(dir.clone()));
    dirs
}

/// 跟 fontconfig 有关的配置文件：主配置 + conf.d 里的每一份 + 用户自己的
fn fontconfig_files() -> Vec<PathBuf> {
    let mut files = vec![PathBuf::from("/etc/fonts/fonts.conf")];
    if let Ok(entries) = std::fs::read_dir("/etc/fonts/conf.d") {
        let mut extra: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "conf"))
            .collect();
        extra.sort();
        files.extend(extra);
    }
    if let Some(home) = home() {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        files.push(config.join("fontconfig/fonts.conf"));
    }
    files
}

/// 把 fontconfig 配置里的 `<dir>...</dir>` 抠出来。
///
/// 只认用得上的两种写法：光秃秃的路径，和 `prefix="xdg"`（相对 XDG data dir）。
/// `<include>` 不管 —— 上面已经把 conf.d 里的文件都单独读了一遍
fn parse_fontconfig_dirs(text: &str, out: &mut Vec<PathBuf>) {
    let mut rest = text;
    while let Some(start) = rest.find("<dir") {
        let after = &rest[start + 4..];
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        let body = &after[gt + 1..];
        let Some(end) = body.find("</dir>") else {
            break;
        };
        let value = body[..end].trim();
        rest = &body[end + "</dir>".len()..];
        if value.is_empty() {
            continue;
        }
        let path = match attr(attrs, "prefix").as_deref() {
            Some("xdg") => xdg_data_home().join(value),
            Some(_) => continue, // relative 之类的不认
            None => match value.strip_prefix("~/") {
                Some(rest) => match home() {
                    Some(home) => home.join(rest),
                    None => continue,
                },
                None => PathBuf::from(value),
            },
        };
        out.push(path);
    }
}

/// XML 属性：`prefix="xdg"` → `xdg`（单双引号都认）
fn attr(attrs: &str, name: &str) -> Option<String> {
    let key = format!("{name}=");
    let start = attrs.find(&key)?;
    let rest = &attrs[start + key.len()..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn xdg_data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/usr/share"))
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

    #[test]
    fn 认得_fontconfig_里的目录() {
        // NixOS 的 00-nixos-cache.conf 就长这样
        let text = r#"<fontconfig>
          <dir>/nix/store/xxx-noto-fonts-cjk/share/fonts</dir>
          <dir prefix="xdg">fonts</dir>
          <cachedir>/nix/store/yyy-fc-cache</cachedir>
        </fontconfig>"#;
        let mut dirs = Vec::new();
        parse_fontconfig_dirs(text, &mut dirs);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert_eq!(
            dirs[0],
            PathBuf::from("/nix/store/xxx-noto-fonts-cjk/share/fonts")
        );
        assert!(dirs[1].ends_with("fonts"), "{:?}", dirs[1]);
    }

    #[test]
    fn xml_属性单双引号都认() {
        assert_eq!(attr(r#" prefix="xdg""#, "prefix").as_deref(), Some("xdg"));
        assert_eq!(attr(r#" prefix='xdg'"#, "prefix").as_deref(), Some("xdg"));
        assert_eq!(attr(" prefix=xdg", "prefix"), None);
        assert_eq!(attr("", "prefix"), None);
    }
}
