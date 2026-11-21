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

use crate::hayro_interpret::font::{Glyph, GlyphRun};
use crate::hayro_interpret::hayro_syntax::page::Page;
use crate::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, Paint, SoftMask, StrokeProps, interpret_page,
};
use crate::kurbo::{Affine, BezPath, PathEl, Rect, Shape};
use rustc_hash::FxHashMap;
use vello_cpu::color::{AlphaColor, Srgb};

const MAX_ROLE_EVENTS: usize = 16_384;
const MAX_TEXT_RUNS: usize = 8_192;

/// A drawing role used to map PDF colors to a dark theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PaintRole {
    /// Glyph text and text inside Type3 glyphs.
    Text,
    /// Strokes and borders.
    Border,
    /// Large filled areas such as page backgrounds and panels.
    Surface,
    /// Small filled marks such as icons, bullets, and underlines.
    Ink,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct DrawKey {
    bounds: [i64; 4],
    color: [u8; 4],
    width: i64,
}

impl DrawKey {
    fn new(bounds: Rect, color: [u8; 4], width: f32) -> Self {
        const COORDINATE_SCALE: f64 = 64.0;
        let quantize = |value: f64| (value * COORDINATE_SCALE).round() as i64;
        Self {
            bounds: [
                quantize(bounds.x0),
                quantize(bounds.y0),
                quantize(bounds.x1),
                quantize(bounds.y1),
            ],
            color,
            width: (f64::from(width) * COORDINATE_SCALE).round() as i64,
        }
    }
}

/// Page-level drawing roles inferred from fills, strokes, and text geometry.
#[derive(Clone, Default)]
pub struct DarkRoleAnalysis {
    fills: FxHashMap<DrawKey, PaintRole>,
    strokes: FxHashMap<DrawKey, PaintRole>,
}

impl DarkRoleAnalysis {
    /// Classify a fill using its page context, with a fallback role.
    pub fn classify_fill(
        &self,
        bounds: Rect,
        transform: Affine,
        page_inverse: Affine,
        color: [f32; 4],
        fallback: PaintRole,
    ) -> PaintRole {
        let bounds = (page_inverse * transform).transform_rect_bbox(bounds);
        self.fills
            .get(&DrawKey::new(bounds, rgba8(color), 0.0))
            .copied()
            .unwrap_or(fallback)
    }

    /// Classify a stroke using its page context, with a fallback role.
    pub fn classify_stroke(
        &self,
        bounds: Rect,
        transform: Affine,
        page_inverse: Affine,
        color: [f32; 4],
        line_width: f32,
        fallback: PaintRole,
    ) -> PaintRole {
        let normalized_transform = page_inverse * transform;
        let bounds = normalized_transform.transform_rect_bbox(bounds);
        let width = f64::from(line_width) * max_factor(normalized_transform);
        self.strokes
            .get(&DrawKey::new(bounds, rgba8(color), width as f32))
            .copied()
            .unwrap_or(fallback)
    }
}

/// Analyze page geometry to assign dark-mode drawing roles.
pub fn analyze_dark_roles<'a>(
    page: &'a Page<'a>,
    cache: &InterpreterCache<'a>,
    interpreter_settings: &InterpreterSettings,
    base_transform: Affine,
    target: Rect,
) -> DarkRoleAnalysis {
    let mut context = Context::new(
        base_transform,
        target,
        cache,
        page.xref(),
        interpreter_settings.clone(),
    );
    let mut analyzer = RoleAnalyzer::new(base_transform.inverse(), target);
    interpret_page(page, &mut context, &mut analyzer);
    analyzer.finish()
}

struct FillCandidate {
    key: DrawKey,
    bounds: Rect,
    color: [u8; 4],
    rectangular: bool,
}

struct StrokeCandidate {
    key: DrawKey,
    bounds: Rect,
    width: f64,
    closed: bool,
}

struct TextRun {
    bounds: Rect,
}

struct RoleAnalyzer {
    page_inverse: Affine,
    page_bounds: Rect,
    fills: Vec<FillCandidate>,
    strokes: Vec<StrokeCandidate>,
    text_runs: Vec<TextRun>,
    type3_depth: usize,
}

impl RoleAnalyzer {
    fn new(page_inverse: Affine, target: Rect) -> Self {
        Self {
            page_inverse,
            page_bounds: page_inverse.transform_rect_bbox(target),
            fills: Vec::new(),
            strokes: Vec::new(),
            text_runs: Vec::new(),
            type3_depth: 0,
        }
    }

    fn record_fill(&mut self, path: &BezPath, props: &DrawProps<'_>) {
        if self.type3_depth > 0 || props.soft_mask.is_some() || self.fills.len() >= MAX_ROLE_EVENTS
        {
            return;
        }
        let Paint::Color(color) = &props.paint else {
            return;
        };
        let color = rgba8(color.to_rgba().components());
        let transform = self.page_inverse * props.transform;
        let bounds = transform.transform_rect_bbox(path.bounding_box());
        self.fills.push(FillCandidate {
            key: DrawKey::new(bounds, color, 0.0),
            bounds,
            color,
            rectangular: is_axis_aligned_rect(path),
        });
    }

    fn record_stroke(&mut self, path: &BezPath, props: &DrawProps<'_>, stroke: &StrokeProps) {
        if self.type3_depth > 0
            || props.soft_mask.is_some()
            || self.strokes.len() >= MAX_ROLE_EVENTS
        {
            return;
        }
        let Paint::Color(color) = &props.paint else {
            return;
        };
        let color = rgba8(color.to_rgba().components());
        let transform = self.page_inverse * props.transform;
        let bounds = transform.transform_rect_bbox(path.bounding_box());
        let width = f64::from(stroke.line_width) * max_factor(transform);
        self.strokes.push(StrokeCandidate {
            key: DrawKey::new(bounds, color, width as f32),
            bounds,
            width,
            closed: path
                .elements()
                .last()
                .is_some_and(|element| matches!(element, PathEl::ClosePath)),
        });
    }

    fn record_text_run(&mut self, glyph_run: &GlyphRun<'_, '_>, transform: Affine) {
        if self.text_runs.len() >= MAX_TEXT_RUNS {
            return;
        }
        let transform = self.page_inverse * transform;
        let bounds = glyph_run
            .glyphs()
            .iter()
            .filter_map(|positioned| {
                let Glyph::Outline(glyph) = &**positioned else {
                    return None;
                };
                Some(
                    (transform * positioned.transform())
                        .transform_rect_bbox(glyph.outline().bounding_box()),
                )
            })
            .reduce(|left, right| left.union(right));
        if let Some(bounds) = bounds.filter(|bounds| bounds.width() > 0.0 && bounds.height() > 0.0)
        {
            self.text_runs.push(TextRun { bounds });
        }
    }

    fn finish(mut self) -> DarkRoleAnalysis {
        self.text_runs
            .sort_by(|left, right| left.bounds.y0.total_cmp(&right.bounds.y0));

        let page_area = (self.page_bounds.width() * self.page_bounds.height()).max(1.0);
        let mut surface_fills = vec![false; self.fills.len()];
        for (index, fill) in self.fills.iter().enumerate() {
            let area_ratio = fill.bounds.width() * fill.bounds.height() / page_area;
            surface_fills[index] =
                (fill.rectangular && area_ratio >= 0.08) || self.contains_text(fill.bounds);
        }

        self.mark_adjacent_fill_groups(&mut surface_fills, page_area, true);
        self.mark_adjacent_fill_groups(&mut surface_fills, page_area, false);

        let surface_bounds: Vec<Rect> = self
            .fills
            .iter()
            .zip(&surface_fills)
            .filter_map(|(fill, surface)| (*surface && fill.rectangular).then_some(fill.bounds))
            .collect();
        let surface_edges = SurfaceEdgeIndex::new(&surface_bounds);
        let fills = self
            .fills
            .into_iter()
            .zip(surface_fills)
            .map(|(fill, is_surface)| {
                (
                    fill.key,
                    if is_surface {
                        PaintRole::Surface
                    } else {
                        PaintRole::Ink
                    },
                )
            })
            .collect();
        let strokes = self
            .strokes
            .into_iter()
            .map(|stroke| {
                let role =
                    if surface_bounds.iter().any(|surface| {
                        stroke.closed
                            && rects_near(stroke.bounds, *surface, stroke.width * 0.5 + 1.0)
                    }) || surface_edges.matches(stroke.bounds, stroke.width, stroke.closed)
                    {
                        PaintRole::Border
                    } else {
                        PaintRole::Ink
                    };
                (stroke.key, role)
            })
            .collect();

        DarkRoleAnalysis { fills, strokes }
    }

    fn contains_text(&self, fill: Rect) -> bool {
        let first = self
            .text_runs
            .partition_point(|text| text.bounds.y0 < fill.y0 - 2.0);
        self.text_runs[first..]
            .iter()
            .take_while(|text| text.bounds.y0 <= fill.y1 + 2.0)
            .any(|text| text_is_covered(fill, text.bounds))
    }

    fn mark_adjacent_fill_groups(&self, roles: &mut [bool], page_area: f64, vertical: bool) {
        let mut groups: FxHashMap<([u8; 4], i64, i64), Vec<usize>> = FxHashMap::default();
        for (index, fill) in self.fills.iter().enumerate() {
            if !fill.rectangular {
                continue;
            }
            let span = if vertical {
                (quantize(fill.bounds.x0), quantize(fill.bounds.x1))
            } else {
                (quantize(fill.bounds.y0), quantize(fill.bounds.y1))
            };
            groups
                .entry((fill.color, span.0, span.1))
                .or_default()
                .push(index);
        }

        for indices in groups.values_mut() {
            indices.sort_by(|left, right| {
                let left = self.fills[*left].bounds;
                let right = self.fills[*right].bounds;
                if vertical {
                    left.y0.total_cmp(&right.y0)
                } else {
                    left.x0.total_cmp(&right.x0)
                }
            });
            let mut chain = Vec::new();
            let mut chain_bounds = Rect::ZERO;
            let mut chain_end = 0.0;
            for index in indices.iter().copied() {
                let bounds = self.fills[index].bounds;
                let (start, end) = if vertical {
                    (bounds.y0, bounds.y1)
                } else {
                    (bounds.x0, bounds.x1)
                };
                if !chain.is_empty() && start > chain_end + 1.0 {
                    self.mark_surface_chain(&chain, chain_bounds, roles, page_area, vertical);
                    chain.clear();
                }
                if chain.is_empty() {
                    chain_bounds = bounds;
                    chain_end = end;
                } else {
                    chain_bounds = chain_bounds.union(bounds);
                    chain_end = chain_end.max(end);
                }
                chain.push(index);
            }
            self.mark_surface_chain(&chain, chain_bounds, roles, page_area, vertical);
        }
    }

    fn mark_surface_chain(
        &self,
        chain: &[usize],
        bounds: Rect,
        roles: &mut [bool],
        page_area: f64,
        vertical: bool,
    ) {
        let span_ratio = if vertical {
            bounds.width() / self.page_bounds.width().max(1.0)
        } else {
            bounds.height() / self.page_bounds.height().max(1.0)
        };
        let area_ratio = bounds.width() * bounds.height() / page_area;
        if chain.len() >= 2 && span_ratio >= 0.25 && area_ratio >= 0.005 {
            for index in chain {
                roles[*index] = true;
            }
        }
    }
}

impl<'a> Device<'a> for RoleAnalyzer {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        match mode {
            DrawMode::Fill(_) => self.record_fill(path, &props),
            DrawMode::Stroke(stroke) => self.record_stroke(path, &props, stroke),
            DrawMode::FillAndStroke(_, stroke) => {
                self.record_fill(path, &props);
                self.record_stroke(path, &props, stroke);
            }
            DrawMode::Invisible => {}
        }
    }

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(
        &mut self,
        _: f32,
        _: Option<SoftMask<'a>>,
        _: BlendMode,
        _: bool,
        _: bool,
    ) {
    }

    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        mode: &DrawMode,
    ) {
        if matches!(mode, DrawMode::Invisible) {
            return;
        }
        self.record_text_run(glyph_run, props.transform);
        for positioned in glyph_run.glyphs() {
            if let Glyph::Type3(glyph) = &**positioned {
                self.type3_depth += 1;
                glyph.interpret(self, props.transform, positioned.transform(), &props.paint);
                self.type3_depth -= 1;
            }
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

fn rgba8(color: [f32; 4]) -> [u8; 4] {
    color.map(|component| (component.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

fn quantize(value: f64) -> i64 {
    (value * 64.0).round() as i64
}

fn max_factor(transform: Affine) -> f64 {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    (a * a + b * b).sqrt().max((c * c + d * d).sqrt())
}

fn is_axis_aligned_rect(path: &BezPath) -> bool {
    let mut points = Vec::with_capacity(4);
    let mut closed = false;
    for element in path.iter() {
        match element {
            PathEl::MoveTo(point) | PathEl::LineTo(point) => {
                points.push((point.x, point.y));
            }
            PathEl::ClosePath => closed = true,
            PathEl::QuadTo(_, _) | PathEl::CurveTo(_, _, _) => return false,
        }
    }
    if !closed || points.len() != 4 {
        return false;
    }

    let bounds = path.bounding_box();
    let tolerance = 1e-8;
    let corners = [
        (bounds.x0, bounds.y0),
        (bounds.x1, bounds.y0),
        (bounds.x1, bounds.y1),
        (bounds.x0, bounds.y1),
    ];
    let has_corners = corners.iter().all(|corner| {
        points.iter().any(|point| {
            (corner.0 - point.0).abs() < tolerance && (corner.1 - point.1).abs() < tolerance
        })
    });
    let has_axis_aligned_edges = points.iter().enumerate().all(|(index, point)| {
        let next = points[(index + 1) % points.len()];
        (point.0 - next.0).abs() < tolerance || (point.1 - next.1).abs() < tolerance
    });
    has_corners && has_axis_aligned_edges
}

fn text_is_covered(fill: Rect, text: Rect) -> bool {
    let fill = fill.inflate(2.0, 2.0);
    let overlap = fill.intersect(text);
    let text_area = text.width() * text.height();
    text_area > 0.0 && overlap.width() * overlap.height() / text_area >= 0.85
}

#[derive(Clone, Copy)]
struct EdgeBand {
    coordinate_min: f64,
    coordinate_max: f64,
    start: f64,
    end: f64,
}

#[derive(Default)]
struct SurfaceEdgeIndex {
    horizontal: FxHashMap<i32, Vec<EdgeBand>>,
    vertical: FxHashMap<i32, Vec<EdgeBand>>,
    rectangles: Vec<Rect>,
}

impl SurfaceEdgeIndex {
    fn new(rectangles: &[Rect]) -> Self {
        let mut index = Self::default();
        for bounds in rectangles {
            index.rectangles.push(*bounds);
            for y in [bounds.y0, bounds.y1] {
                index
                    .horizontal
                    .entry(y.round() as i32)
                    .or_default()
                    .push(EdgeBand {
                        coordinate_min: y,
                        coordinate_max: y,
                        start: bounds.x0,
                        end: bounds.x1,
                    });
            }
            for x in [bounds.x0, bounds.x1] {
                index
                    .vertical
                    .entry(x.round() as i32)
                    .or_default()
                    .push(EdgeBand {
                        coordinate_min: x,
                        coordinate_max: x,
                        start: bounds.y0,
                        end: bounds.y1,
                    });
            }
        }
        Self::merge_bands(&mut index.horizontal);
        Self::merge_bands(&mut index.vertical);
        index
            .rectangles
            .sort_by(|left, right| left.x0.total_cmp(&right.x0));
        index
    }

    fn merge_bands(edges: &mut FxHashMap<i32, Vec<EdgeBand>>) {
        for bands in edges.values_mut() {
            bands.sort_by(|left, right| {
                left.coordinate_min
                    .total_cmp(&right.coordinate_min)
                    .then(left.start.total_cmp(&right.start))
            });
            let mut merged: Vec<EdgeBand> = Vec::with_capacity(bands.len());
            for band in bands.drain(..) {
                if let Some(previous) = merged.last_mut()
                    && band.coordinate_min <= previous.coordinate_max + 1.0
                    && band.start <= previous.end + 1.0
                    && band.end >= previous.start - 1.0
                {
                    previous.coordinate_min = previous.coordinate_min.min(band.coordinate_min);
                    previous.coordinate_max = previous.coordinate_max.max(band.coordinate_max);
                    previous.start = previous.start.min(band.start);
                    previous.end = previous.end.max(band.end);
                } else {
                    merged.push(band);
                }
            }
            *bands = merged;
        }
    }

    fn matches(&self, stroke: Rect, width: f64, closed: bool) -> bool {
        let tolerance = (width * 0.5 + 0.75).max(1.0);
        if closed {
            let first = self
                .rectangles
                .partition_point(|surface| surface.x0 < stroke.x0 - tolerance);
            if self.rectangles[first..]
                .iter()
                .take_while(|surface| surface.x0 <= stroke.x0 + tolerance)
                .any(|surface| rects_near(stroke, *surface, tolerance))
            {
                return true;
            }
        }
        if stroke.height() <= tolerance {
            let y = (stroke.y0 + stroke.y1) * 0.5;
            let first = (y - tolerance).floor() as i32;
            let last = (y + tolerance).ceil() as i32;
            for bucket in first..=last {
                if let Some(edges) = self.horizontal.get(&bucket)
                    && edges.iter().any(|edge| {
                        let overlap =
                            (stroke.x1.min(edge.end) - stroke.x0.max(edge.start)).max(0.0);
                        (y >= edge.coordinate_min - tolerance
                            && y <= edge.coordinate_max + tolerance)
                            && overlap >= stroke.width().min(edge.end - edge.start) * 0.7
                    })
                {
                    return true;
                }
            }
        }

        if stroke.width() <= tolerance {
            let x = (stroke.x0 + stroke.x1) * 0.5;
            let first = (x - tolerance).floor() as i32;
            let last = (x + tolerance).ceil() as i32;
            for bucket in first..=last {
                if let Some(edges) = self.vertical.get(&bucket)
                    && edges.iter().any(|edge| {
                        let overlap =
                            (stroke.y1.min(edge.end) - stroke.y0.max(edge.start)).max(0.0);
                        (x >= edge.coordinate_min - tolerance
                            && x <= edge.coordinate_max + tolerance)
                            && overlap >= stroke.height().min(edge.end - edge.start) * 0.7
                    })
                {
                    return true;
                }
            }
        }

        false
    }
}

fn rects_near(left: Rect, right: Rect, tolerance: f64) -> bool {
    (left.x0 - right.x0).abs() <= tolerance
        && (left.y0 - right.y0).abs() <= tolerance
        && (left.x1 - right.x1).abs() <= tolerance
        && (left.y1 - right.y1).abs() <= tolerance
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

/// Map an RGBA color to a dark theme according to its drawing role.
pub fn modify_color([r, g, b, a]: [f32; 4], role: PaintRole, theme: &DarkTheme) -> [f32; 4] {
    let hsl = rgb_to_hsl(r, g, b);
    let mapped = match role {
        PaintRole::Surface => modify_bg_hsl(hsl, theme.bg_pole),
        PaintRole::Border => modify_border_hsl(hsl, theme.fg_pole, theme.bg_pole),
        PaintRole::Text => modify_fg_hsl(hsl, theme.fg_pole, true),
        // 小面积的深色填充经常是图标或图形，过暗的有彩色不应被当成中性色丢掉色相。
        PaintRole::Ink => modify_fg_hsl(hsl, theme.fg_pole, false),
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
