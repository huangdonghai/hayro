//! Dark Reader 风格的颜色映射。
//!
//! 颜色数学移植自 [darkreader/darkreader](https://github.com/darkreader/darkreader)
//! （MIT License）。核心不是简单反相，而是按绘制角色使用不同的分段 HSL 映射：
//!
//! - 背景（`Surface`）的明度被压进 `[0, 0.4]`，暗色表面保持暗、白纸落到 pole；
//! - 前景（`Text` / `Ink`）的明度被抬进 `[0.55, 0.9]`，暗色文字变亮但不会纯白；
//! - 边框（`Border`）被收进 `[0.2, 0.5]` 的中间带。
//!
//! 两个 pole 默认是 `#181a1b`（暗色纸面）和 `#e8e6e3`（亮色文字）。中性色会继承
//! pole 的色相/饱和度，所以灰字会带一点暖调、纸面会带一点蓝绿，而不是纯黑白。

use vello_cpu::color::{AlphaColor, Srgb};

/// 一次绘制的颜色角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub(crate) enum PaintRole {
    /// 文字（glyph run、Type3 字形内部）。
    Text,
    /// 描边、表格线。
    Border,
    /// 大面积填充：页面底色、表格背景、色块。
    Surface,
    /// 小面积深色填充：图标、项目符号、矢量轮廓文字、下划线。
    Ink,
}

#[derive(Clone, Copy, Debug)]
struct Hsl {
    h: f32,
    s: f32,
    l: f32,
}

/// 暗色主题参数。
///
/// `background_color` 同时用于未绘制页面的底色（`PixmapSettings::bg_color`）。
#[derive(Clone, Copy, Debug)]
pub struct DarkTheme {
    /// 纸面颜色，也是暗色 pole。
    pub background_color: AlphaColor<Srgb>,
    /// 文字颜色，也是亮色 pole。
    pub text_color: AlphaColor<Srgb>,
    /// 1.0 = 不变。
    pub brightness: f32,
    /// 1.0 = 不变。
    pub contrast: f32,
    /// 0..=1。
    pub sepia: f32,
    /// 0..=1。
    pub grayscale: f32,
    bg_pole: Hsl,
    fg_pole: Hsl,
}

impl Default for DarkTheme {
    fn default() -> Self {
        Self::new(
            AlphaColor::from_rgba8(0x18, 0x1a, 0x1b, 0xff),
            AlphaColor::from_rgba8(0xe8, 0xe6, 0xe3, 0xff),
        )
    }
}

impl DarkTheme {
    /// 用自定义的纸面/文字 pole 创建主题。
    pub fn new(background_color: AlphaColor<Srgb>, text_color: AlphaColor<Srgb>) -> Self {
        Self {
            background_color,
            text_color,
            brightness: 1.0,
            contrast: 1.0,
            sepia: 0.0,
            grayscale: 0.0,
            bg_pole: color_to_hsl(background_color),
            fg_pole: color_to_hsl(text_color),
        }
    }
}

/// 把 RGBA 分量按角色映射到暗色主题。
pub(crate) fn modify_color([r, g, b, a]: [f32; 4], role: PaintRole, theme: &DarkTheme) -> [f32; 4] {
    let hsl = rgb_to_hsl(r, g, b);
    let mapped = match role {
        PaintRole::Surface => modify_bg_hsl(hsl, theme.bg_pole),
        PaintRole::Text => modify_fg_hsl(hsl, theme.fg_pole, true),
        // 小面积的深色填充经常是图标或图形，过暗的有彩色不应被当成中性色丢掉色相。
        PaintRole::Ink => modify_fg_hsl(hsl, theme.fg_pole, false),
        // 中性描边走边框带；有彩色描边多半是图表/插画内容，按前景处理。
        PaintRole::Border if hsl.s < 0.24 => modify_border_hsl(hsl, theme.fg_pole, theme.bg_pole),
        PaintRole::Border => modify_fg_hsl(hsl, theme.fg_pole, false),
    };
    let [r, g, b] = apply_filters(hsl_to_rgb(mapped.h, mapped.s, mapped.l), theme);
    [r, g, b, a]
}

fn color_to_hsl(color: AlphaColor<Srgb>) -> Hsl {
    let [r, g, b, _] = color.components;
    rgb_to_hsl(r, g, b)
}

/// 线性重映射：把 `x` 从 `[a, b]` 映到 `[c, d]`。
fn scale(x: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    if (b - a).abs() < f32::EPSILON {
        return c;
    }
    c + (x - a) * (d - c) / (b - a)
}

const MAX_BG_LIGHTNESS: f32 = 0.4;
const MIN_FG_LIGHTNESS: f32 = 0.55;

fn modify_bg_hsl(hsl: Hsl, pole: Hsl) -> Hsl {
    let is_dark = hsl.l < 0.5;
    let is_blue = hsl.h > 200.0 && hsl.h < 280.0;
    let is_neutral = hsl.s < 0.12 || (hsl.l > 0.8 && is_blue);

    if is_dark {
        let l = scale(hsl.l, 0.0, 0.5, 0.0, MAX_BG_LIGHTNESS);
        if is_neutral {
            return Hsl {
                h: pole.h,
                s: pole.s,
                l,
            };
        }
        return Hsl { l, ..hsl };
    }

    let mut l = scale(hsl.l, 0.5, 1.0, MAX_BG_LIGHTNESS, pole.l);
    if is_neutral {
        return Hsl {
            h: pole.h,
            s: pole.s,
            l,
        };
    }

    let mut h = hsl.h;
    let is_yellow = h > 60.0 && h < 180.0;
    if is_yellow {
        if h > 120.0 {
            h = scale(h, 120.0, 180.0, 135.0, 180.0);
        } else {
            h = scale(h, 60.0, 120.0, 60.0, 105.0);
        }
    }

    // 结果落在偏黄区间时压暗，避免暗色页面上出现刺眼的亮黄。
    if h > 40.0 && h < 80.0 {
        l *= 0.75;
    }

    Hsl { h, s: hsl.s, l }
}

fn modify_blue_fg_hue(hue: f32) -> f32 {
    scale(hue, 205.0, 245.0, 205.0, 220.0)
}

fn modify_fg_hsl(hsl: Hsl, pole: Hsl, dark_is_neutral: bool) -> Hsl {
    let is_light = hsl.l > 0.5;
    let is_neutral = (dark_is_neutral && hsl.l < 0.2) || hsl.s < 0.24;
    let is_blue = !is_neutral && hsl.h > 205.0 && hsl.h < 245.0;

    if is_light {
        let l = scale(hsl.l, 0.5, 1.0, MIN_FG_LIGHTNESS, pole.l);
        if is_neutral {
            return Hsl {
                h: pole.h,
                s: pole.s,
                l,
            };
        }
        let h = if is_blue {
            modify_blue_fg_hue(hsl.h)
        } else {
            hsl.h
        };
        return Hsl { h, s: hsl.s, l };
    }

    if is_neutral {
        let l = scale(hsl.l, 0.0, 0.5, pole.l, MIN_FG_LIGHTNESS);
        return Hsl {
            h: pole.h,
            s: pole.s,
            l,
        };
    }

    let h = if is_blue {
        modify_blue_fg_hue(hsl.h)
    } else {
        hsl.h
    };
    let end = if is_blue {
        (MIN_FG_LIGHTNESS + 0.05).min(1.0)
    } else {
        MIN_FG_LIGHTNESS
    };
    let l = scale(hsl.l, 0.0, 0.5, pole.l, end);

    Hsl { h, s: hsl.s, l }
}

fn modify_border_hsl(hsl: Hsl, pole_fg: Hsl, pole_bg: Hsl) -> Hsl {
    let is_dark = hsl.l < 0.5;
    let is_neutral = hsl.l < 0.2 || hsl.s < 0.24;

    let (h, s) = if is_neutral {
        if is_dark {
            (pole_fg.h, pole_fg.s)
        } else {
            (pole_bg.h, pole_bg.s)
        }
    } else {
        (hsl.h, hsl.s)
    };

    Hsl {
        h,
        s,
        l: scale(hsl.l, 0.0, 1.0, 0.5, 0.2),
    }
}

fn apply_filters([mut r, mut g, mut b]: [f32; 3], theme: &DarkTheme) -> [f32; 3] {
    let grayscale = theme.grayscale.clamp(0.0, 1.0);
    if grayscale > 0.0 {
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        r += (y - r) * grayscale;
        g += (y - g) * grayscale;
        b += (y - b) * grayscale;
    }

    let sepia = theme.sepia.clamp(0.0, 1.0);
    if sepia > 0.0 {
        let sr = 0.393 * r + 0.769 * g + 0.189 * b;
        let sg = 0.349 * r + 0.686 * g + 0.168 * b;
        let sb = 0.272 * r + 0.534 * g + 0.131 * b;
        r += (sr - r) * sepia;
        g += (sg - g) * sepia;
        b += (sb - b) * sepia;
    }

    r *= theme.brightness;
    g *= theme.brightness;
    b *= theme.brightness;

    r = (r - 0.5) * theme.contrast + 0.5;
    g = (g - 0.5) * theme.contrast + 0.5;
    b = (b - 0.5) * theme.contrast + 0.5;

    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> Hsl {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < f32::EPSILON {
        return Hsl { h: 0.0, s: 0.0, l };
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let mut h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }

    Hsl { h, s, l }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [l, l, l];
    }

    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;

    [
        (r1 + m).clamp(0.0, 1.0),
        (g1 + m).clamp(0.0, 1.0),
        (b1 + m).clamp(0.0, 1.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(hex: u32) -> AlphaColor<Srgb> {
        AlphaColor::from_rgba8(
            ((hex >> 16) & 0xff) as u8,
            ((hex >> 8) & 0xff) as u8,
            (hex & 0xff) as u8,
            0xff,
        )
    }

    fn map(hex: u32, role: PaintRole) -> [u8; 4] {
        modify_color(rgb(hex).components, role, &DarkTheme::default())
            .map(|c| (c * 255.0 + 0.5) as u8)
    }

    #[test]
    fn background_and_foreground_have_disjoint_bands() {
        // 同一个深色，作为背景保持暗，作为文字变亮。
        let surface = map(0x1a1a1a, PaintRole::Surface);
        let text = map(0x1a1a1a, PaintRole::Text);
        assert!(
            surface[0] < 60,
            "dark surface should stay dark: {surface:?}"
        );
        assert!(text[0] > 150, "dark text should become light: {text:?}");
    }

    #[test]
    fn white_paper_maps_to_the_dark_pole() {
        let out = map(0xffffff, PaintRole::Surface);
        assert!(
            (out[0] as i32 - 0x18).abs() <= 2
                && (out[1] as i32 - 0x1a).abs() <= 2
                && (out[2] as i32 - 0x1b).abs() <= 2,
            "white should map to #181a1b, got {out:?}"
        );
    }

    #[test]
    fn light_text_stays_light() {
        let out = map(0xffffff, PaintRole::Text);
        assert!(out[0] > 200, "light text should not sink: {out:?}");
    }

    #[test]
    fn neutral_colors_adopt_pole_tint() {
        let out = map(0x808080, PaintRole::Surface);
        // 灰度映射后应带一点 pole 的蓝绿色调，而不是纯灰。
        assert_ne!(out[0], out[2]);
    }
}
