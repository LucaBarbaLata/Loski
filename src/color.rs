//! Picks an embed colour from the album cover, like Spotify's now-playing
//! background.

use std::{collections::HashMap, sync::LazyLock, time::Duration};

use parking_lot::Mutex;

pub const SPOTIFY_GREEN: u32 = 0x1ED760;

static CACHE: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);

pub async fn cover_color(http: &reqwest::Client, url: &str) -> u32 {
    if let Some(color) = CACHE.lock().get(url) {
        return *color;
    }
    let color = fetch(http, url).await.unwrap_or(SPOTIFY_GREEN);
    let mut cache = CACHE.lock();
    if cache.len() > 512 {
        cache.clear();
    }
    cache.insert(url.to_string(), color);
    color
}

async fn fetch(http: &reqwest::Client, url: &str) -> Option<u32> {
    let bytes = http
        .get(url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;
    tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).ok()?.to_rgb8();
        let small = image::imageops::thumbnail(&img, 32, 32);
        Some(dominant(small.pixels().map(|p| p.0)))
    })
    .await
    .ok()?
}

/// The colour that stands out in an image already in memory.
pub fn dominant_of(img: &image::RgbImage) -> u32 {
    let small = image::imageops::thumbnail(img, 32, 32);
    dominant(small.pixels().map(|p| p.0))
}

/// Weights each pixel by how vivid it is, buckets by hue, and averages the
/// heaviest bucket. Greyscale covers fall back to their average colour.
fn dominant(pixels: impl Iterator<Item = [u8; 3]>) -> u32 {
    const BINS: usize = 12;
    let mut bins = [[0f64; 4]; BINS];
    let mut all = [0f64; 4];

    for [r, g, b] in pixels {
        let (rf, gf, bf) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        let max = rf.max(gf).max(bf);
        let min = rf.min(gf).min(bf);
        let delta = max - min;
        let saturation = if max > 0.0 { delta / max } else { 0.0 };
        all[0] += r as f64;
        all[1] += g as f64;
        all[2] += b as f64;
        all[3] += 1.0;
        if delta < 1e-6 {
            continue;
        }
        let hue = if max == rf {
            ((gf - bf) / delta).rem_euclid(6.0)
        } else if max == gf {
            (bf - rf) / delta + 2.0
        } else {
            (rf - gf) / delta + 4.0
        } / 6.0;
        // Favour vivid mid-to-bright colours over muddy or washed-out ones.
        let weight = saturation * saturation * max;
        let bin = ((hue * BINS as f64) as usize).min(BINS - 1);
        bins[bin][0] += r as f64 * weight;
        bins[bin][1] += g as f64 * weight;
        bins[bin][2] += b as f64 * weight;
        bins[bin][3] += weight;
    }

    let best = bins
        .iter()
        .max_by(|a, b| a[3].total_cmp(&b[3]))
        .filter(|b| b[3] > 0.6)
        .unwrap_or(&all);
    if best[3] <= 0.0 {
        return SPOTIFY_GREEN;
    }
    let mut rgb = [best[0] / best[3], best[1] / best[3], best[2] / best[3]];

    // Keep it readable as a thin embed stripe on Discord's dark theme.
    let lum = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    if lum < 60.0 {
        let lift = 60.0 / lum.max(1.0);
        rgb.iter_mut().for_each(|c| *c = (*c * lift).min(255.0));
    }
    ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_vivid_colour_over_grey() {
        let pixels = std::iter::repeat_n([128, 128, 128], 900)
            .chain(std::iter::repeat_n([220, 30, 30], 124));
        let c = dominant(pixels);
        assert!(c >> 16 > 150, "expected red, got {c:06x}");
    }

    #[test]
    fn greyscale_falls_back_to_average() {
        let c = dominant(std::iter::repeat_n([100, 100, 100], 100));
        assert_eq!(c, 0x646464);
    }
}
