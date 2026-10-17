//! Twemoji alert marks, one PNG per [`AlertKind`], plus crossed swords
//! while a client is taking damage.
//!
//! The PNGs are embedded so Linux and Windows composite the same pixels.
//! Output is premultiplied BGRA. The 1px pad stays transparent; callers
//! still size the slot as `size + 2 * HALO_PAD`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use image::imageops::FilterType;
use image::RgbaImage;

use crate::eve_logs::AlertKind;
use crate::preview_common::HALO_PAD;

/// What the preview slot shows. Damage wins over an alert for the
/// duration of the incoming-damage flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShownIcon {
    Alert(AlertKind),
    Damage,
}

/// Icon edge in device pixels. 40% of the shorter interior side, clamped
/// to 16–64 reference pixels (scaled by `dpi_scale`), and never larger
/// than the interior it has to sit in.
pub fn alert_icon_px(shorter_interior: i32, dpi_scale: f32) -> i32 {
    let shorter = shorter_interior.max(1);
    let scale = if dpi_scale.is_finite() && dpi_scale > 0.0 {
        dpi_scale
    } else {
        1.0
    };
    let min_px = (16.0 * scale).round() as i32;
    let max_px = (64.0 * scale).round().max(min_px as f32) as i32;
    let want = ((shorter as f32) * 0.4).round() as i32;
    want.clamp(min_px, max_px).min(shorter).max(1)
}

/// Alert emoji, or crossed swords when `taking_damage` is set.
pub fn shown_icon(taking_damage: bool, alert: Option<AlertKind>) -> Option<ShownIcon> {
    if taking_damage {
        Some(ShownIcon::Damage)
    } else {
        alert.map(ShownIcon::Alert)
    }
}

/// Premultiplied BGRA, side `size + 2 * HALO_PAD`.
pub fn raster_alert_icon(kind: AlertKind, size: i32) -> Vec<u8> {
    raster_cached(ShownIcon::Alert(kind), size)
}

/// Crossed swords. Same bitmap contract as [`raster_alert_icon`].
pub fn raster_damage_icon(size: i32) -> Vec<u8> {
    raster_cached(ShownIcon::Damage, size)
}

pub fn raster_icon(icon: ShownIcon, size: i32) -> Vec<u8> {
    match icon {
        ShownIcon::Alert(kind) => raster_alert_icon(kind, size),
        ShownIcon::Damage => raster_damage_icon(size),
    }
}

type IconCache = HashMap<(ShownIcon, i32), Vec<u8>>;

fn raster_cached(icon: ShownIcon, size: i32) -> Vec<u8> {
    let size = size.max(1);
    let key = (icon, size);
    let cache = icon_cache();
    if let Some(hit) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return hit.clone();
    }
    let buf = raster_uncached(icon, size);
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, buf.clone());
    buf
}

fn icon_cache() -> &'static Mutex<IconCache> {
    static CACHE: OnceLock<Mutex<IconCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn raster_uncached(icon: ShownIcon, size: i32) -> Vec<u8> {
    let src = source_rgba(icon);
    let scaled = image::imageops::resize(&src, size as u32, size as u32, FilterType::Triangle);
    premul_bgra(&scaled, size)
}

fn premul_bgra(img: &RgbaImage, size: i32) -> Vec<u8> {
    let pad = HALO_PAD.max(0);
    let side = size + pad * 2;
    let mut out = vec![0u8; (side as usize) * (side as usize) * 4];
    for y in 0..size {
        for x in 0..size {
            let p = img.get_pixel(x as u32, y as u32);
            let a = u16::from(p[3]);
            let r = (u16::from(p[0]) * a / 255) as u8;
            let g = (u16::from(p[1]) * a / 255) as u8;
            let b = (u16::from(p[2]) * a / 255) as u8;
            let ox = (x + pad) as usize;
            let oy = (y + pad) as usize;
            let i = (oy * side as usize + ox) * 4;
            out[i] = b;
            out[i + 1] = g;
            out[i + 2] = r;
            out[i + 3] = a as u8;
        }
    }
    out
}

fn source_rgba(icon: ShownIcon) -> RgbaImage {
    image::load_from_memory(png_bytes(icon))
        .unwrap_or_else(|err| panic!("alert png {icon:?}: {err}"))
        .to_rgba8()
}

fn png_bytes(icon: ShownIcon) -> &'static [u8] {
    match icon {
        ShownIcon::Alert(AlertKind::FleetInvite) => {
            include_bytes!("../assets/alerts/fleet_invite.png")
        }
        ShownIcon::Alert(AlertKind::FollowWarp) => {
            include_bytes!("../assets/alerts/follow_warp.png")
        }
        ShownIcon::Alert(AlertKind::Regroup) => include_bytes!("../assets/alerts/regroup.png"),
        ShownIcon::Alert(AlertKind::Compression) => {
            include_bytes!("../assets/alerts/compression.png")
        }
        ShownIcon::Alert(AlertKind::Decloak) => include_bytes!("../assets/alerts/decloak.png"),
        ShownIcon::Alert(AlertKind::CrystalBroke) => {
            include_bytes!("../assets/alerts/crystal_broke.png")
        }
        ShownIcon::Alert(AlertKind::ConvoRequest) => {
            include_bytes!("../assets/alerts/convo_request.png")
        }
        ShownIcon::Alert(AlertKind::Scramble) => include_bytes!("../assets/alerts/scramble.png"),
        ShownIcon::Alert(AlertKind::Disrupt) => include_bytes!("../assets/alerts/disrupt.png"),
        ShownIcon::Alert(AlertKind::Jam) => include_bytes!("../assets/alerts/jam.png"),
        ShownIcon::Alert(AlertKind::Bubble) => include_bytes!("../assets/alerts/bubble.png"),
        ShownIcon::Alert(AlertKind::OutOfCharges) => {
            include_bytes!("../assets/alerts/out_of_charges.png")
        }
        ShownIcon::Alert(AlertKind::CargoFull) => {
            include_bytes!("../assets/alerts/cargo_full.png")
        }
        ShownIcon::Alert(AlertKind::MiningRange) => {
            include_bytes!("../assets/alerts/mining_range.png")
        }
        ShownIcon::Damage => include_bytes!("../assets/alerts/damage.png"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [AlertKind; 14] = [
        AlertKind::FleetInvite,
        AlertKind::FollowWarp,
        AlertKind::Regroup,
        AlertKind::Compression,
        AlertKind::Decloak,
        AlertKind::CrystalBroke,
        AlertKind::ConvoRequest,
        AlertKind::Scramble,
        AlertKind::Disrupt,
        AlertKind::Jam,
        AlertKind::Bubble,
        AlertKind::OutOfCharges,
        AlertKind::CargoFull,
        AlertKind::MiningRange,
    ];

    #[test]
    fn icon_size_tracks_the_thumbnail() {
        assert_eq!(alert_icon_px(200, 1.0), 64);
        assert_eq!(alert_icon_px(100, 1.0), 40);
        assert_eq!(alert_icon_px(20, 1.0), 16);
        assert_eq!(alert_icon_px(10, 1.0), 10);
        assert_eq!(alert_icon_px(100, 2.0), 40);
    }

    #[test]
    fn each_icon_has_ink_and_is_distinct() {
        let mut seen = Vec::new();
        let mut check = |icon: ShownIcon, raster: Vec<u8>| {
            let side = 32 + HALO_PAD * 2;
            assert_eq!(raster.len(), (side as usize) * (side as usize) * 4);
            let ink = raster.chunks(4).filter(|p| p[3] > 0).count();
            assert!(ink > 20, "{icon:?} ink {ink}");
            let hash = fnv(&raster);
            assert!(!seen.contains(&hash), "{icon:?} matches another icon");
            seen.push(hash);
        };
        for kind in KINDS {
            check(ShownIcon::Alert(kind), raster_alert_icon(kind, 32));
        }
        check(ShownIcon::Damage, raster_damage_icon(32));
    }

    #[test]
    fn damage_wins_over_an_alert() {
        assert_eq!(
            shown_icon(true, Some(AlertKind::Scramble)),
            Some(ShownIcon::Damage)
        );
        assert_eq!(
            shown_icon(false, Some(AlertKind::Jam)),
            Some(ShownIcon::Alert(AlertKind::Jam))
        );
        assert_eq!(shown_icon(false, None), None);
    }

    fn fnv(bytes: &[u8]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }
}
