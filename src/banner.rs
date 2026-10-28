//! The artwork on the now playing card, in one of three styles: Glass (the
//! cover's colours blurred into a backdrop, a frosted panel over it, and on
//! the panel the sharp cover), Vinyl (the cover as a sleeve with the record
//! sliding out) or Minimal (the cover beside a flat colour). The song's
//! details go next to the cover in each.
//!
//! Everything is drawn with plain pixel maths: rounded corners and shadows
//! come from a signed distance to a rounded rectangle, and text is
//! rasterised glyph by glyph with ab_glyph.

use std::{
    collections::VecDeque,
    io::Cursor,
    sync::{Arc, LazyLock},
    time::Duration,
};

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, VariableFont, point};
use image::{ImageBuffer, Rgb, RgbImage, codecs::jpeg::JpegEncoder, imageops::FilterType};
use librespot_core::SpotifyId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{track::TrackInfo, util::fmt_ms};

pub const WIDTH: u32 = 1200;
pub const HEIGHT: u32 = 560;
pub const FILENAME: &str = "banner.jpg";

/// The frosted panel, inset from the edges.
const PANEL: Rect = Rect {
    x: 44.0,
    y: 44.0,
    w: WIDTH as f32 - 88.0,
    h: HEIGHT as f32 - 88.0,
    radius: 40.0,
};
const COVER: f32 = 320.0;
const COVER_X: f32 = PANEL.x + 40.0;
const COVER_Y: f32 = (HEIGHT as f32 - COVER) / 2.0;
const TEXT_X: f32 = COVER_X + COVER + 48.0;
const TEXT_MAX_WIDTH: f32 = PANEL.x + PANEL.w - 44.0 - TEXT_X;

const BRICOLAGE: &[u8] = include_bytes!("../assets/fonts/BricolageGrotesque.ttf");
const NOTO: &[u8] = include_bytes!("../assets/fonts/NotoSans.ttf");

pub(crate) struct Face {
    pub display: FontVec,
    pub medium: FontVec,
    pub regular: FontVec,
}

pub(crate) struct Fonts {
    pub bricolage: Face,
    pub noto: Face,
}

fn instance(bytes: &[u8], weight: f32, optical_size: Option<f32>) -> FontVec {
    let mut font = FontVec::try_from_vec(bytes.to_vec()).expect("bundled font loads");
    font.set_variation(b"wght", weight);
    if let Some(size) = optical_size {
        font.set_variation(b"opsz", size);
    }
    font
}

pub(crate) static FONTS: LazyLock<Fonts> = LazyLock::new(|| Fonts {
    bricolage: Face {
        display: instance(BRICOLAGE, 760.0, Some(72.0)),
        medium: instance(BRICOLAGE, 600.0, Some(36.0)),
        regular: instance(BRICOLAGE, 420.0, Some(24.0)),
    },
    noto: Face {
        display: instance(NOTO, 750.0, None),
        medium: instance(NOTO, 600.0, None),
        regular: instance(NOTO, 420.0, None),
    },
});

pub(crate) fn covers(font: &FontVec, text: &str) -> bool {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .all(|c| font.glyph_id(c) != GlyphId(0))
}

/// Bricolage where it has every glyph, Noto Sans otherwise, or nothing for
/// scripts neither covers (the card then uses its compact layout).
pub(crate) fn face_for(text: &str) -> Option<&'static Face> {
    let fonts = &*FONTS;
    if covers(&fonts.bricolage.display, text) {
        Some(&fonts.bricolage)
    } else if covers(&fonts.noto.display, text) {
        Some(&fonts.noto)
    } else {
        None
    }
}

pub(crate) fn text_width(font: &FontVec, size: f32, text: &str, tracking: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut previous: Option<GlyphId> = None;
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(prev) = previous {
            width += scaled.kern(prev, id) + tracking;
        }
        width += scaled.h_advance(id);
        previous = Some(id);
    }
    width
}

/// Shortens to fit `max_width`, ending with an ellipsis.
pub(crate) fn ellipsize(font: &FontVec, size: f32, text: &str, max_width: f32) -> String {
    if text_width(font, size, text, 0.0) <= max_width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        // Don't leave "Song -…" or "Song (…".
        let kept: String = chars.iter().collect();
        let kept = kept.trim_end_matches(|c: char| c.is_whitespace() || "-–—,:;(/&".contains(c));
        let candidate = format!("{kept}…");
        if text_width(font, size, &candidate, 0.0) <= max_width {
            return candidate;
        }
    }
    "…".to_string()
}

/// Greedy word wrap. A word wider than the line gets a line of its own.
fn wrap(font: &FontVec, size: f32, text: &str, max_width: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if current.is_empty() || text_width(font, size, &candidate, 0.0) <= max_width {
            current = candidate;
        } else {
            lines.push(std::mem::replace(&mut current, word.to_string()));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The title as large as fits on two lines of `max_width`; failing that,
/// two lines at the smallest size with the second one cut short.
fn fit_title(font: &FontVec, text: &str, max_width: f32) -> (f32, Vec<String>) {
    const SIZES: [f32; 3] = [70.0, 60.0, 52.0];
    for size in SIZES {
        let lines = wrap(font, size, text, max_width);
        let fits = lines
            .iter()
            .all(|l| text_width(font, size, l, 0.0) <= max_width);
        if lines.len() <= 2 && fits {
            return (size, lines);
        }
    }
    let size = SIZES[SIZES.len() - 1];
    let lines = wrap(font, size, text, max_width);
    let first = ellipsize(font, size, &lines[0], max_width);
    if lines.len() == 1 || first.ends_with('…') {
        return (size, vec![first]);
    }
    let rest = lines[1..].join(" ");
    (size, vec![first, ellipsize(font, size, &rest, max_width)])
}

/// An image being drawn, in floating point so blends don't band.
pub(crate) struct Canvas {
    pub pixels: Vec<[f32; 3]>,
    pub width: u32,
    pub height: u32,
}

impl Canvas {
    pub fn from_image(img: &RgbImage) -> Self {
        Self {
            pixels: img
                .pixels()
                .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
                .collect(),
            width: img.width(),
            height: img.height(),
        }
    }

    /// `cover` shrunk to a few pixels and blown back up to fill the canvas:
    /// a smooth, cheap blur.
    pub fn blurred(cover: &RgbImage, width: u32, height: u32) -> Self {
        let side = width.max(height);
        let tiny = image::imageops::resize(cover, 10, 10, FilterType::Triangle);
        let wide = image::imageops::resize(&tiny, side, side, FilterType::CatmullRom);
        let crop = image::imageops::crop_imm(
            &wide,
            (side - width) / 2,
            (side - height) / 2,
            width,
            height,
        )
        .to_image();
        Self::from_image(&crop)
    }

    pub fn blend(&mut self, x: i32, y: i32, color: [f32; 3], alpha: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 || alpha <= 0.0 {
            return;
        }
        let alpha = alpha.min(1.0);
        let px = &mut self.pixels[(y as u32 * self.width + x as u32) as usize];
        for (c, target) in px.iter_mut().zip(color) {
            *c = *c * (1.0 - alpha) + target * alpha;
        }
    }

    /// Fills a rounded rectangle, anti-aliased, optionally feathered into a shadow.
    pub fn rounded_rect(&mut self, rect: Rect, color: [f32; 3], alpha: f32, feather: f32) {
        let Rect { x, y, w, h, radius } = rect;
        let pad = feather.ceil() as i32 + 1;
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in (y as i32 - pad)..=((y + h) as i32 + pad) {
            for px in (x as i32 - pad)..=((x + w) as i32 + pad) {
                let d = rounded_rect_distance(
                    px as f32 + 0.5 - cx,
                    py as f32 + 0.5 - cy,
                    w / 2.0,
                    h / 2.0,
                    radius,
                );
                let coverage = if feather > 0.0 {
                    1.0 - smoothstep(-feather, feather, d)
                } else {
                    (0.5 - d).clamp(0.0, 1.0)
                };
                self.blend(px, py, color, alpha * coverage);
            }
        }
    }

    /// A frosted glass panel: a light, slightly brighter-at-the-top fill
    /// with a thin bright rim, over whatever is already drawn.
    pub fn glass(&mut self, rect: Rect) {
        let Rect { x, y, w, h, radius } = rect;
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in (y as i32 - 1)..=((y + h) as i32 + 1) {
            let t = ((py as f32 - y) / h).clamp(0.0, 1.0);
            let fill = 0.15 - 0.06 * t;
            for px in (x as i32 - 1)..=((x + w) as i32 + 1) {
                let d = rounded_rect_distance(
                    px as f32 + 0.5 - cx,
                    py as f32 + 0.5 - cy,
                    w / 2.0,
                    h / 2.0,
                    radius,
                );
                let coverage = (0.5 - d).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                // The rim: the outermost two pixels, brighter.
                let rim = if d > -2.5 { 0.22 } else { 0.0 };
                self.blend(px, py, WHITE, (fill + rim) * coverage);
            }
        }
    }

    /// Draws `img` at (x, y) clipped to rounded corners.
    pub fn image_rounded(&mut self, img: &RgbImage, x: f32, y: f32, radius: f32) {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for (ix, iy, p) in img.enumerate_pixels() {
            let (px, py) = (x as i32 + ix as i32, y as i32 + iy as i32);
            let d = rounded_rect_distance(
                px as f32 + 0.5 - cx,
                py as f32 + 0.5 - cy,
                w / 2.0,
                h / 2.0,
                radius,
            );
            let coverage = (0.5 - d).clamp(0.0, 1.0);
            self.blend(px, py, [p[0] as f32, p[1] as f32, p[2] as f32], coverage);
        }
    }

    /// Draws one line of text with its baseline at `baseline`.
    pub fn text(&mut self, pen: &Pen, text: &str, x: f32, baseline: f32) {
        let Pen {
            font,
            size,
            color,
            alpha,
            tracking,
        } = *pen;
        let scaled = font.as_scaled(PxScale::from(size));
        let mut caret = x;
        let mut previous: Option<GlyphId> = None;
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(prev) = previous {
                caret += scaled.kern(prev, id) + tracking;
            }
            let glyph = id.with_scale_and_position(size, point(caret, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    self.blend(
                        bounds.min.x as i32 + gx as i32,
                        bounds.min.y as i32 + gy as i32,
                        color,
                        coverage * alpha,
                    );
                });
            }
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
    }

    pub fn into_jpeg(self) -> Vec<u8> {
        let img: RgbImage = ImageBuffer::from_fn(self.width, self.height, |x, y| {
            let p = self.pixels[(y * self.width + x) as usize];
            Rgb([
                p[0].round().clamp(0.0, 255.0) as u8,
                p[1].round().clamp(0.0, 255.0) as u8,
                p[2].round().clamp(0.0, 255.0) as u8,
            ])
        });
        let mut out = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(Cursor::new(&mut out), 90);
        encoder.encode_image(&img).expect("in-memory JPEG encode");
        out
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
}

/// How a line of text is drawn.
#[derive(Clone, Copy)]
pub(crate) struct Pen<'a> {
    pub font: &'a FontVec,
    pub size: f32,
    pub color: [f32; 3],
    pub alpha: f32,
    /// Extra space between letters, in pixels.
    pub tracking: f32,
}

pub(crate) const WHITE: [f32; 3] = [255.0, 255.0, 255.0];

impl<'a> Pen<'a> {
    pub fn white(font: &'a FontVec, size: f32, alpha: f32) -> Self {
        Self {
            font,
            size,
            color: WHITE,
            alpha,
            tracking: 0.0,
        }
    }
}

fn rounded_rect_distance(px: f32, py: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = px.abs() - (half_w - radius);
    let qy = py.abs() - (half_h - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The text that goes on a banner.
pub struct BannerText {
    pub title: String,
    pub artists: String,
    pub album: String,
    pub year: Option<i32>,
    pub duration_ms: u32,
    pub explicit: bool,
}

impl From<&TrackInfo> for BannerText {
    fn from(t: &TrackInfo) -> Self {
        Self {
            title: t.title.clone(),
            artists: t.artists_joined(),
            album: t.album.clone(),
            year: t.year,
            duration_ms: t.duration_ms,
            explicit: t.explicit,
        }
    }
}

/// How the panel's artwork looks, picked in /settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// The cover blurred into a backdrop, with a frosted panel over it.
    #[default]
    Glass,
    /// The cover as a record sleeve, with the record sliding out of it.
    Vinyl,
    /// The cover full height, beside a flat colour taken from it.
    Minimal,
}

impl Style {
    pub const ALL: [Style; 3] = [Style::Glass, Style::Vinyl, Style::Minimal];

    pub fn key(self) -> &'static str {
        match self {
            Style::Glass => "glass",
            Style::Vinyl => "vinyl",
            Style::Minimal => "minimal",
        }
    }

    pub fn from_key(key: &str) -> Option<Style> {
        Style::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// Renders the banner as a JPEG, or None if the text needs a script the
/// bundled fonts can't draw.
pub fn render(cover: &RgbImage, info: &BannerText, style: Style) -> Option<Vec<u8>> {
    let title_face = face_for(&info.title)?;
    let artist_face = face_for(&info.artists)?;
    let (mut canvas, place) = match style {
        Style::Glass => (
            glass(cover),
            Place {
                x: TEXT_X,
                max_width: TEXT_MAX_WIDTH,
                chip_alpha: 0.2,
            },
        ),
        Style::Vinyl => (
            vinyl(cover),
            Place {
                x: VINYL_TEXT_X,
                max_width: WIDTH as f32 - 64.0 - VINYL_TEXT_X,
                chip_alpha: 0.14,
            },
        ),
        Style::Minimal => (
            minimal(cover),
            Place {
                x: MINIMAL_TEXT_X,
                max_width: WIDTH as f32 - 64.0 - MINIMAL_TEXT_X,
                chip_alpha: 0.16,
            },
        ),
    };
    details(&mut canvas, info, title_face, artist_face, &place);
    Some(canvas.into_jpeg())
}

/// Glass: the cover blurred into a vivid, darkened backdrop, a frosted
/// panel floating over it, and the sharp cover on the panel.
fn glass(cover: &RgbImage) -> Canvas {
    // Darkened enough for white text, most at the edges.
    let mut canvas = Canvas::blurred(cover, WIDTH, HEIGHT);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (u, v) = (
                x as f32 / WIDTH as f32 - 0.5,
                y as f32 / HEIGHT as f32 - 0.5,
            );
            let shade = 0.66 - 0.28 * (u * u + v * v).sqrt();
            let px = &mut canvas.pixels[(y * WIDTH + x) as usize];
            let luma = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
            for c in px.iter_mut() {
                let vivid = luma + (*c - luma) * 1.35;
                *c = (vivid * shade + 4.0).clamp(0.0, 255.0);
            }
        }
    }

    // The panel, floating a little above the backdrop.
    let shadow = Rect {
        y: PANEL.y + 18.0,
        ..PANEL
    };
    canvas.rounded_rect(shadow, [0.0; 3], 0.22, 30.0);
    canvas.glass(PANEL);

    // Cover with a soft drop shadow.
    let cover_shadow = Rect {
        x: COVER_X,
        y: COVER_Y + 14.0,
        w: COVER,
        h: COVER,
        radius: 24.0,
    };
    canvas.rounded_rect(cover_shadow, [0.0; 3], 0.5, 24.0);
    let sharp = image::imageops::resize(cover, COVER as u32, COVER as u32, FilterType::Lanczos3);
    canvas.image_rounded(&sharp, COVER_X, COVER_Y, 22.0);
    canvas
}

const SLEEVE: f32 = 360.0;
const SLEEVE_X: f32 = 60.0;
const SLEEVE_Y: f32 = (HEIGHT as f32 - SLEEVE) / 2.0;
const RECORD_R: f32 = 172.0;
/// The record's centre, out to the right of the sleeve's.
const RECORD_X: f32 = SLEEVE_X + SLEEVE / 2.0 + 168.0;
const VINYL_TEXT_X: f32 = RECORD_X + RECORD_R + 56.0;

/// Vinyl: a dark room in the cover's colours, the cover as a sleeve, and
/// the record sliding out of it.
fn vinyl(cover: &RgbImage) -> Canvas {
    let cy = HEIGHT as f32 / 2.0;
    let mut canvas = Canvas::blurred(cover, WIDTH, HEIGHT);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            // A little light falls on the record; the rest is dark.
            let (dx, dy) = (
                (x as f32 - RECORD_X) / WIDTH as f32,
                (y as f32 - cy) / HEIGHT as f32,
            );
            let shade = (0.46 - 0.42 * (dx * dx + dy * dy).sqrt()).clamp(0.17, 0.46);
            let px = &mut canvas.pixels[(y * WIDTH + x) as usize];
            let luma = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
            for c in px.iter_mut() {
                let muted = luma + (*c - luma) * 0.85;
                *c = (muted * shade + 5.0).clamp(0.0, 255.0);
            }
        }
    }
    let record_shadow = Rect {
        x: RECORD_X - RECORD_R,
        y: cy - RECORD_R + 12.0,
        w: RECORD_R * 2.0,
        h: RECORD_R * 2.0,
        radius: RECORD_R,
    };
    canvas.rounded_rect(record_shadow, [0.0; 3], 0.55, 22.0);
    record(&mut canvas, cover, RECORD_X, cy, RECORD_R);

    let sleeve_shadow = Rect {
        x: SLEEVE_X + 6.0,
        y: SLEEVE_Y + 16.0,
        w: SLEEVE,
        h: SLEEVE,
        radius: 10.0,
    };
    canvas.rounded_rect(sleeve_shadow, [0.0; 3], 0.6, 26.0);
    let sharp = image::imageops::resize(cover, SLEEVE as u32, SLEEVE as u32, FilterType::Lanczos3);
    canvas.image_rounded(&sharp, SLEEVE_X, SLEEVE_Y, 6.0);
    canvas
}

/// A vinyl record centred on (cx, cy): fine grooves, two streaks where they
/// catch the light, a smooth rim, and the cover as its label.
fn record(canvas: &mut Canvas, cover: &RgbImage, cx: f32, cy: f32, r: f32) {
    let label_r = (r * 0.34).round();
    let label_side = (label_r * 2.0) as u32;
    let label = image::imageops::resize(cover, label_side, label_side, FilterType::Lanczos3);
    // Where the light comes from, just under the horizon on the right.
    const LIGHT: f32 = 0.42;
    for py in (cy - r - 1.0) as i32..=(cy + r + 1.0) as i32 {
        for px in (cx - r - 1.0) as i32..=(cx + r + 1.0) as i32 {
            let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt();
            let coverage = (r - d + 0.5).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let color = if d < label_r {
                if d < 4.5 {
                    // The spindle hole.
                    [10.0; 3]
                } else {
                    let p = label.get_pixel(
                        ((dx + label_r) as u32).min(label_side - 1),
                        ((dy + label_r) as u32).min(label_side - 1),
                    );
                    // The label's edge sinks into the vinyl a little.
                    let edge = 1.0 - 0.35 * smoothstep(label_r - 3.0, label_r, d);
                    [p[0] as f32 * edge, p[1] as f32 * edge, p[2] as f32 * edge]
                }
            } else {
                let angle = dy.atan2(dx);
                let across = (d - label_r) / (r - label_r);
                let groove = 0.5 + 0.5 * (d * 1.7).sin();
                let streak = (2.0 * (angle - LIGHT)).cos().max(0.0).powi(10);
                // Smooth vinyl, without grooves, round the label and at the rim.
                let smooth = d < label_r + 9.0 || d > r - 6.0;
                let grooves = if smooth { 0.0 } else { 6.0 * groove };
                let shine = 62.0 * streak * (0.3 + 0.7 * across);
                let rim = if d > r - 2.0 { 18.0 } else { 0.0 };
                let v = 15.0 + grooves + shine + rim;
                [v, v, v * 1.05]
            };
            canvas.blend(px, py, color, coverage);
        }
    }
}

const MINIMAL_TEXT_X: f32 = HEIGHT as f32 + 64.0;

/// Minimal: the cover full height on the left, and a flat colour taken
/// from it on the right.
fn minimal(cover: &RgbImage) -> Canvas {
    let tone = flat_tone(cover);
    let mut canvas = Canvas {
        pixels: Vec::with_capacity((WIDTH * HEIGHT) as usize),
        width: WIDTH,
        height: HEIGHT,
    };
    for y in 0..HEIGHT {
        // A touch lighter at the top, so the colour isn't dead flat.
        let lift = 1.0 + 0.12 * (1.0 - y as f32 / HEIGHT as f32);
        let row = tone.map(|c| (c * lift).min(255.0));
        canvas.pixels.extend((0..WIDTH).map(|_| row));
    }
    let side = HEIGHT;
    let sharp = image::imageops::resize(cover, side, side, FilterType::Lanczos3);
    for (x, y, p) in sharp.enumerate_pixels() {
        canvas.pixels[(y * WIDTH + x) as usize] = [p[0] as f32, p[1] as f32, p[2] as f32];
    }
    canvas
}

/// The cover's average colour, a little richer and dark enough for white
/// text on it.
fn flat_tone(cover: &RgbImage) -> [f32; 3] {
    let count = (cover.width() * cover.height()).max(1) as f32;
    let mut sum = [0.0f32; 3];
    for p in cover.pixels() {
        for (s, c) in sum.iter_mut().zip(p.0) {
            *s += c as f32;
        }
    }
    let mean = sum.map(|s| s / count);
    let luma = 0.2126 * mean[0] + 0.7152 * mean[1] + 0.0722 * mean[2];
    let rich = mean.map(|c| (luma + (c - luma) * 1.3).max(0.0));
    let scale = if luma > 44.0 { 44.0 / luma } else { 1.0 };
    rich.map(|c| (c * scale).clamp(0.0, 255.0))
}

/// Where the song's details go: from `x`, at most `max_width` wide, and how
/// strongly the chips stand out from what's behind them.
struct Place {
    x: f32,
    max_width: f32,
    chip_alpha: f32,
}

/// Album and year, the title, the artists and the chips, centred top to
/// bottom.
fn details(
    canvas: &mut Canvas,
    info: &BannerText,
    title_face: &Face,
    artist_face: &Face,
    at: &Place,
) {
    // Album and year sit above the title, small and spaced out.
    let kicker = match (info.album.is_empty(), info.year) {
        (false, Some(year)) => format!("{} · {year}", info.album),
        (false, None) => info.album.clone(),
        (true, Some(year)) => year.to_string(),
        (true, None) => String::new(),
    }
    .to_uppercase();
    let kicker_face = face_for(&kicker).unwrap_or(&FONTS.noto);

    let (title_size, title_lines) = fit_title(&title_face.display, &info.title, at.max_width);
    let kicker_size = 21.0;
    let artist_size = 34.0;
    let chip_size = 21.0;
    let chip_height = 40.0;
    let kicker_pen = Pen {
        tracking: 3.0,
        ..Pen::white(&kicker_face.medium, kicker_size, 0.74)
    };
    let kicker = ellipsize(kicker_pen.font, kicker_size, &kicker, at.max_width - 40.0);
    let artist = ellipsize(
        &artist_face.medium,
        artist_size,
        &info.artists,
        at.max_width,
    );

    let title_line_height = title_size * 1.04;
    let block_height = if kicker.is_empty() {
        0.0
    } else {
        kicker_size + 18.0
    } + title_lines.len() as f32 * title_line_height
        + 14.0
        + artist_size
        + 28.0
        + chip_height;
    let mut y = (HEIGHT as f32 - block_height) / 2.0;

    if !kicker.is_empty() {
        y += kicker_size;
        canvas.text(&kicker_pen, &kicker, at.x, y - kicker_size * 0.15);
        y += 18.0;
    }
    let title_pen = Pen::white(&title_face.display, title_size, 1.0);
    for line in &title_lines {
        y += title_line_height;
        canvas.text(&title_pen, line, at.x, y - title_size * 0.2);
    }
    y += 14.0 + artist_size;
    let artist_pen = Pen::white(&artist_face.medium, artist_size, 0.93);
    canvas.text(&artist_pen, &artist, at.x, y - artist_size * 0.18);

    // Chips: quality, length, explicit.
    y += 28.0;
    let mut chips = vec!["320 kbps".to_string(), fmt_ms(info.duration_ms as u64)];
    if info.explicit {
        chips.push("E".to_string());
    }
    let chip_pen = Pen {
        tracking: 0.4,
        ..Pen::white(&FONTS.bricolage.medium, chip_size, 0.96)
    };
    let mut x = at.x;
    for chip in chips {
        let w = text_width(chip_pen.font, chip_size, &chip, chip_pen.tracking) + 32.0;
        let pill = Rect {
            x,
            y,
            w,
            h: chip_height,
            radius: chip_height / 2.0,
        };
        canvas.rounded_rect(pill, WHITE, at.chip_alpha, 0.0);
        canvas.text(&chip_pen, &chip, x + 16.0, y + chip_height * 0.68);
        x += w + 10.0;
    }
}

/// Recently rendered banners, so reposting the panel doesn't redraw.
type Rendered = ((SpotifyId, Style), Arc<Vec<u8>>);
static CACHE: LazyLock<Mutex<VecDeque<Rendered>>> = LazyLock::new(Default::default);
const CACHE_SIZE: usize = 6;

/// Downloads the cover and renders the banner for a song.
pub async fn for_track(
    http: &reqwest::Client,
    track: &TrackInfo,
    style: Style,
) -> Option<Arc<Vec<u8>>> {
    let key = (track.id, style);
    if let Some((_, bytes)) = CACHE.lock().iter().find(|(k, _)| *k == key) {
        return Some(bytes.clone());
    }
    let url = track.cover_url.as_ref()?;
    let bytes = http
        .get(url)
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;
    let info = BannerText::from(track);
    let rendered = tokio::task::spawn_blocking(move || {
        let cover = image::load_from_memory(&bytes).ok()?.to_rgb8();
        render(&cover, &info, style)
    })
    .await
    .ok()??;
    let rendered = Arc::new(rendered);
    let mut cache = CACHE.lock();
    cache.push_front((key, rendered.clone()));
    cache.truncate(CACHE_SIZE);
    Some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cover() -> RgbImage {
        ImageBuffer::from_fn(640, 640, |x, y| {
            let (fx, fy) = (x as f32 / 640.0, y as f32 / 640.0);
            Rgb([
                (40.0 + 200.0 * fx) as u8,
                (20.0 + 90.0 * (1.0 - fy)) as u8,
                (110.0 + 120.0 * fy) as u8,
            ])
        })
    }

    fn info(title: &str) -> BannerText {
        BannerText {
            title: title.into(),
            artists: "M83".into(),
            album: "Hurry Up, We're Dreaming".into(),
            year: Some(2011),
            duration_ms: 243_000,
            explicit: true,
        }
    }

    /// Writes the banners to LOSKI_BANNER_OUT for a look, drawn from the
    /// cover at LOSKI_BANNER_COVER if set.
    #[test]
    fn renders_every_style_at_the_right_size() {
        let long = "Șapte Nopți (feat. Someone Else) - Extended Club Mix Remastered Version 2024";
        let cover = std::env::var("LOSKI_BANNER_COVER")
            .ok()
            .and_then(|path| image::open(path).ok())
            .map_or_else(sample_cover, |cover| cover.to_rgb8());
        for style in Style::ALL {
            for (name, title) in [("short", "Midnight City"), ("long", long)] {
                let jpeg = render(&cover, &info(title), style).expect("renders");
                let img = image::load_from_memory(&jpeg).unwrap();
                assert_eq!((img.width(), img.height()), (WIDTH, HEIGHT));
                if let Ok(dir) = std::env::var("LOSKI_BANNER_OUT") {
                    let path = format!("{dir}/banner-{}-{name}.jpg", style.key());
                    std::fs::write(path, &jpeg).unwrap();
                }
            }
        }
    }

    #[test]
    fn falls_back_for_scripts_without_glyphs() {
        for style in Style::ALL {
            assert!(render(&sample_cover(), &info("夜に駆ける"), style).is_none());
            assert!(render(&sample_cover(), &info("Кино — Группа крови"), style).is_some());
        }
    }

    #[test]
    fn styles_are_saved_by_name() {
        for style in Style::ALL {
            let json = serde_json::to_string(&style).unwrap();
            assert_eq!(json, format!("\"{}\"", style.key()));
            assert_eq!(Style::from_key(style.key()), Some(style));
        }
    }

    #[test]
    fn fits_titles_on_two_lines_at_most() {
        let font = &FONTS.bricolage.display;
        assert_eq!(
            fit_title(font, "Short", TEXT_MAX_WIDTH),
            (70.0, vec!["Short".to_string()])
        );
        let (_, lines) = fit_title(font, &"Word ".repeat(40), TEXT_MAX_WIDTH);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        assert!(
            lines
                .iter()
                .all(|l| text_width(font, 52.0, l, 0.0) <= TEXT_MAX_WIDTH)
        );
        let (_, one_long_word) = fit_title(font, &"W".repeat(80), TEXT_MAX_WIDTH);
        assert_eq!(one_long_word.len(), 1);
        assert!(one_long_word[0].ends_with('…'));
    }

    #[test]
    fn ellipsis_does_not_leave_dangling_punctuation() {
        let font = &FONTS.bricolage.display;
        let cut = ellipsize(font, 52.0, "Song Title - Extended Club Mix Version", 360.0);
        assert!(!cut.contains("-…") && !cut.contains(" …"), "{cut}");
    }
}
