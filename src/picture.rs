//! The stats picture: a Wrapped-style card for /stats and the monthly recap,
//! drawn with the same pieces as the artwork banner. Your top song's cover
//! sets the backdrop and the colour of the chart.

use ab_glyph::{Font, GlyphId};
use image::{RgbImage, imageops::FilterType};

use crate::{
    banner::{self, Canvas, FONTS, Face, Pen, Rect, WHITE},
    color,
};

pub const WIDTH: u32 = 1200;
pub const HEIGHT: u32 = 675;
pub const FILENAME: &str = "stats.jpg";

const LEFT: f32 = 64.0;
const LEFT_WIDTH: f32 = 580.0;
const RIGHT: f32 = 708.0;
const RIGHT_END: f32 = 1136.0;
const CHART_TOP: f32 = 372.0;
const CHART_BOTTOM: f32 = 590.0;

pub struct Song {
    pub title: String,
    pub artist: String,
    /// "23 plays"
    pub plays: String,
    pub cover: Option<RgbImage>,
}

pub struct Artist {
    pub name: String,
    pub detail: String,
}

pub struct Bar {
    pub value: u64,
    /// Written under the bar, when there's room for it.
    pub label: Option<String>,
}

pub struct StatsPicture {
    /// "OCTOBER SO FAR"
    pub kicker: String,
    /// "31h 12m"
    pub headline: String,
    /// "of music · 412 plays · 96 songs · 41 artists"
    pub subline: String,
    pub chips: Vec<String>,
    pub bars: Vec<Bar>,
    pub songs: Vec<Song>,
    pub artists: Vec<Artist>,
}

/// The face that can draw `text`, and the text without anything neither
/// bundled font has (rather than empty boxes).
fn printable(text: &str) -> (&'static Face, String) {
    if let Some(face) = banner::face_for(text) {
        return (face, text.to_string());
    }
    let face = &FONTS.noto;
    let kept: String = text
        .chars()
        .filter(|c| c.is_whitespace() || face.display.glyph_id(*c) != GlyphId(0))
        .collect();
    (face, kept.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn rgb(color: u32) -> [f32; 3] {
    [
        ((color >> 16) & 0xFF) as f32,
        ((color >> 8) & 0xFF) as f32,
        (color & 0xFF) as f32,
    ]
}

/// Lifts a dark colour so it reads on the dark backdrop.
fn readable(color: [f32; 3]) -> [f32; 3] {
    let luma = 0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2];
    if luma >= 120.0 {
        return color;
    }
    let lift = ((120.0 - luma) / 120.0).min(0.7);
    color.map(|c| c + (255.0 - c) * lift)
}

fn kicker_pen(alpha: f32) -> Pen<'static> {
    Pen {
        tracking: 2.0,
        ..Pen::white(&FONTS.bricolage.medium, 20.0, alpha)
    }
}

/// The biggest size (of `sizes`) at which `text` fits in `width`.
fn fitting(face: &'static Face, text: &str, width: f32, sizes: &[f32]) -> (f32, String) {
    for &size in sizes {
        if banner::text_width(&face.display, size, text, 0.0) <= width {
            return (size, text.to_string());
        }
    }
    let size = sizes[sizes.len() - 1];
    (size, banner::ellipsize(&face.display, size, text, width))
}

pub fn render(picture: &StatsPicture) -> Vec<u8> {
    let backdrop = picture.songs.iter().find_map(|s| s.cover.as_ref());
    let mut canvas = match backdrop {
        Some(cover) => Canvas::blurred(cover, WIDTH, HEIGHT),
        None => Canvas::from_image(&RgbImage::from_fn(WIDTH, HEIGHT, |x, y| {
            // Loski's amber, deep and dark, when there's no cover to use.
            let t = (x as f32 / WIDTH as f32 + y as f32 / HEIGHT as f32) / 2.0;
            image::Rgb([
                (120.0 - 70.0 * t) as u8,
                (64.0 - 40.0 * t) as u8,
                (40.0 + 10.0 * t) as u8,
            ])
        })),
    };
    for (i, px) in canvas.pixels.iter_mut().enumerate() {
        let y = (i as u32 / WIDTH) as f32 / HEIGHT as f32;
        let shade = 0.34 - 0.08 * y;
        for c in px.iter_mut() {
            *c = *c * shade + 8.0;
        }
    }
    let accent = readable(backdrop.map_or(rgb(0xF2A33A), |c| rgb(color::dominant_of(c))));

    draw_summary(&mut canvas, picture);
    draw_chart(&mut canvas, &picture.bars, accent);
    draw_lists(&mut canvas, picture, accent);

    let mark = kicker_pen(0.4);
    let mark_width = banner::text_width(mark.font, mark.size, "LOSKI", mark.tracking);
    canvas.text(&mark, "LOSKI", RIGHT_END - mark_width, 646.0);
    canvas.into_jpeg()
}

fn draw_summary(canvas: &mut Canvas, picture: &StatsPicture) {
    canvas.text(&kicker_pen(0.72), &picture.kicker, LEFT, 92.0);

    let (size, headline) = fitting(
        &FONTS.bricolage,
        &picture.headline,
        LEFT_WIDTH,
        &[128.0, 108.0, 92.0],
    );
    canvas.text(
        &Pen::white(&FONTS.bricolage.display, size, 1.0),
        &headline,
        LEFT - 4.0,
        212.0,
    );

    let sub_pen = Pen::white(&FONTS.bricolage.regular, 26.0, 0.86);
    let subline = banner::ellipsize(sub_pen.font, sub_pen.size, &picture.subline, LEFT_WIDTH);
    canvas.text(&sub_pen, &subline, LEFT, 262.0);

    // Chips, as many as fit on one row.
    let chip_pen = Pen {
        tracking: 0.3,
        ..Pen::white(&FONTS.bricolage.medium, 20.0, 0.95)
    };
    let (height, mut x, y) = (40.0, LEFT, 292.0);
    for chip in &picture.chips {
        let width =
            banner::text_width(chip_pen.font, chip_pen.size, chip, chip_pen.tracking) + 32.0;
        if x + width > LEFT + LEFT_WIDTH {
            break;
        }
        let pill = Rect {
            x,
            y,
            w: width,
            h: height,
            radius: height / 2.0,
        };
        canvas.rounded_rect(pill, WHITE, 0.15, 0.0);
        canvas.text(&chip_pen, chip, x + 16.0, y + height * 0.68);
        x += width + 10.0;
    }
}

fn draw_chart(canvas: &mut Canvas, bars: &[Bar], accent: [f32; 3]) {
    let n = bars.len().max(1) as f32;
    let gap = match bars.len() {
        0..=7 => 18.0,
        8..=12 => 12.0,
        _ => 5.0,
    };
    let width = ((LEFT_WIDTH - gap * (n - 1.0)) / n).min(72.0);
    let max = bars.iter().map(|b| b.value).max().unwrap_or(0).max(1);
    let best = bars
        .iter()
        .enumerate()
        .filter(|(_, b)| b.value > 0)
        .max_by_key(|(_, b)| b.value)
        .map(|(i, _)| i);
    let label_pen = Pen::white(&FONTS.bricolage.regular, 18.0, 0.62);
    let full = CHART_BOTTOM - CHART_TOP;

    for (i, bar) in bars.iter().enumerate() {
        let x = LEFT + i as f32 * (width + gap);
        let (height, color, alpha) = match bar.value {
            0 => (5.0, accent, 0.28),
            v => (
                (v as f32 / max as f32 * full).max(10.0),
                if Some(i) == best { WHITE } else { accent },
                0.95,
            ),
        };
        let column = Rect {
            x,
            y: CHART_BOTTOM - height,
            w: width,
            h: height,
            radius: (width / 2.0).min(9.0),
        };
        canvas.rounded_rect(column, color, alpha, 0.0);
        if let Some(label) = &bar.label {
            let label_width = banner::text_width(label_pen.font, label_pen.size, label, 0.0);
            let lx =
                (x + width / 2.0 - label_width / 2.0).clamp(LEFT, LEFT + LEFT_WIDTH - label_width);
            canvas.text(&label_pen, label, lx, CHART_BOTTOM + 32.0);
        }
    }
}

fn draw_lists(canvas: &mut Canvas, picture: &StatsPicture, accent: [f32; 3]) {
    let text_x = RIGHT + 108.0;
    let text_width = RIGHT_END - text_x;
    canvas.text(&kicker_pen(0.72), "TOP SONGS", RIGHT, 92.0);
    for (i, song) in picture.songs.iter().take(5).enumerate() {
        let top = 114.0 + i as f32 * 72.0;
        let rank = Pen::white(&FONTS.bricolage.display, 26.0, 0.45);
        canvas.text(&rank, &(i + 1).to_string(), RIGHT, top + 40.0);

        let cover = Rect {
            x: RIGHT + 36.0,
            y: top + 4.0,
            w: 56.0,
            h: 56.0,
            radius: 10.0,
        };
        match &song.cover {
            Some(img) => {
                let shadow = Rect {
                    y: cover.y + 4.0,
                    ..cover
                };
                canvas.rounded_rect(shadow, [0.0; 3], 0.45, 8.0);
                let small = image::imageops::resize(img, 56, 56, FilterType::Lanczos3);
                canvas.image_rounded(&small, cover.x, cover.y, cover.radius);
            }
            None => canvas.rounded_rect(cover, accent, 0.35, 0.0),
        }

        // A title the fonts can't draw at all gives its line to the artist.
        let (title_face, title) = printable(&song.title);
        let (artist_face, artist) = printable(&song.artist);
        let (face, title, detail) = match (title.is_empty(), artist.is_empty()) {
            (false, false) => (title_face, title, format!("{artist} · {}", song.plays)),
            (false, true) => (title_face, title, song.plays.clone()),
            (true, false) => (artist_face, artist, song.plays.clone()),
            (true, true) => (
                &FONTS.bricolage,
                format!("Song {}", i + 1),
                song.plays.clone(),
            ),
        };
        let title_pen = Pen::white(&face.medium, 24.0, 1.0);
        let title = banner::ellipsize(title_pen.font, title_pen.size, &title, text_width);
        canvas.text(&title_pen, &title, text_x, top + 27.0);
        let (face, detail) = printable(&detail);
        let detail_pen = Pen::white(&face.regular, 19.0, 0.7);
        let detail = banner::ellipsize(detail_pen.font, detail_pen.size, &detail, text_width);
        canvas.text(&detail_pen, &detail, text_x, top + 53.0);
    }

    if picture.artists.is_empty() {
        return;
    }
    canvas.text(&kicker_pen(0.72), "TOP ARTISTS", RIGHT, 506.0);
    for (i, artist) in picture.artists.iter().take(3).enumerate() {
        let baseline = 542.0 + i as f32 * 34.0;
        let rank = Pen::white(&FONTS.bricolage.display, 22.0, 0.45);
        canvas.text(&rank, &(i + 1).to_string(), RIGHT, baseline);

        let (face, name) = printable(&artist.name);
        let name_pen = Pen::white(&face.medium, 22.0, 1.0);
        let name = banner::ellipsize(name_pen.font, name_pen.size, &name, 230.0);
        canvas.text(&name_pen, &name, RIGHT + 36.0, baseline);
        let name_width = banner::text_width(name_pen.font, name_pen.size, &name, 0.0);
        let detail_pen = Pen::white(&FONTS.bricolage.regular, 19.0, 0.7);
        let room = RIGHT_END - (RIGHT + 36.0 + name_width + 14.0);
        let detail = banner::ellipsize(detail_pen.font, detail_pen.size, &artist.detail, room);
        canvas.text(
            &detail_pen,
            &detail,
            RIGHT + 36.0 + name_width + 14.0,
            baseline,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cover(seed: u8) -> RgbImage {
        RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([
                (x as u8).wrapping_mul(3).wrapping_add(seed),
                (y as u8).wrapping_mul(2).wrapping_add(seed / 2),
                120u8.wrapping_add(seed),
            ])
        })
    }

    fn sample(days: usize, with_covers: bool) -> StatsPicture {
        StatsPicture {
            kicker: "OCTOBER SO FAR".into(),
            headline: "31h 12m".into(),
            subline: "of music · 412 plays · 96 songs · 41 artists".into(),
            chips: vec![
                "9-day streak".into(),
                "+4 hr vs Sep 1–21".into(),
                "Night owl".into(),
                "This one does not fit on the row".into(),
            ],
            bars: (0..days)
                .map(|i| Bar {
                    value: [0, 40, 90, 20, 130, 75, 0, 60][i % 8] * 60_000,
                    label: (i == 0 || i + 1 == days).then(|| format!("Oct {}", i + 1)),
                })
                .collect(),
            songs: [
                "The Diary of Jane - Single Version",
                "Lost in You",
                "夜に駆ける",
                "Șapte Nopți (feat. Someone With A Long Name) - Extended Mix",
                "Always",
            ]
            .iter()
            .enumerate()
            .map(|(i, title)| Song {
                title: title.to_string(),
                artist: "Breaking Benjamin".into(),
                plays: format!("{} plays", 23 - i),
                cover: with_covers.then(|| cover(i as u8 * 40)),
            })
            .collect(),
            artists: vec![
                Artist {
                    name: "Breaking Benjamin".into(),
                    detail: "41 plays · 2 hr 3 min".into(),
                },
                Artist {
                    name: "Three Days Grace And A Much Longer Name Than Fits".into(),
                    detail: "18 plays · 58 min".into(),
                },
                Artist {
                    name: "Saliva".into(),
                    detail: "9 plays · 31 min".into(),
                },
            ],
        }
    }

    #[test]
    fn draws_a_picture_of_the_right_size() {
        for (name, picture) in [
            ("month", sample(21, true)),
            ("week", sample(7, false)),
            ("one", sample(1, true)),
        ] {
            let jpeg = render(&picture);
            let img = image::load_from_memory(&jpeg).unwrap();
            assert_eq!((img.width(), img.height()), (WIDTH, HEIGHT));
            if let Ok(dir) = std::env::var("LOSKI_BANNER_OUT") {
                std::fs::write(format!("{dir}/stats-{name}.jpg"), &jpeg).unwrap();
            }
        }
    }

    #[test]
    fn leaves_out_what_the_fonts_cant_draw() {
        let (_, text) = printable("夜に駆ける YOASOBI");
        assert_eq!(text, "YOASOBI");
        assert_eq!(printable("Șapte Nopți").1, "Șapte Nopți");
    }
}
