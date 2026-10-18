//! Cross-platform helpers for the preview-window managers
//! (`preview_x11.rs` on Linux, `preview_windows.rs` on Windows). Both
//! managers implement the same UX — drag to position, snap to dock,
//! click vs drag distinguished by pixel-distance threshold — so the
//! geometry pieces live here once.

/// Axis-aligned rectangle in root/screen coordinates. `right` and
/// `bottom` are exclusive (right = left + width). Same shape as Win32
/// `RECT`; the Windows backend converts at the call boundary.
#[derive(Debug, Clone, Copy)]
pub struct DragRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Press/drag/release bookkeeping for a draggable preview. `active`
/// is true between mouse-down and mouse-up. `dragged` flips once
/// motion exceeds `DRAG_THRESHOLD_PX` — at release it distinguishes
/// "moved → persist new position" from "clicked → activate source
/// EVE client". `origin_screen` and `origin_window` are captured at
/// press time so the motion delta can drive both threshold detection
/// and position computation.
#[derive(Debug, Default, Clone, Copy)]
pub struct DragState {
    pub active: bool,
    pub dragged: bool,
    pub origin_screen: (i32, i32),
    pub origin_window: (i32, i32),
}

/// Pointer must move at least this many reference pixels between
/// press and release for the gesture to count as a drag. Below the
/// threshold the release activates the source EVE client; above it,
/// the new window position is persisted. Windows wraps this through
/// the DPI helper (`px(DRAG_THRESHOLD_PX)`); Linux uses it raw.
pub const DRAG_THRESHOLD_PX: i32 = 4;

/// Edges within this distance during a drag snap to the matching
/// edge of another preview window. Generous enough that docking
/// feels deliberate, tight enough that you can place freely between
/// docked previews. DPI-scaled on Windows; raw on Linux.
pub const SNAP_THRESHOLD_PX: i32 = 12;

/// Title-strip pixels reserved for one DPS figure (`↓10k+` / `↑10k+`).
/// Tight: just the glyphs, no extra air. 96 DPI; Windows scales via `px()`.
pub const DPS_SLOT_PX: i32 = 48;

/// Compact DPS figure for the title row. Four characters including `k`:
/// `0`–`999` as an integer, `1.0k`–`9.9k` from 1000–9999, then `10k+`.
pub fn format_dps(v: f64) -> String {
    if !v.is_finite() || v < 1.0 {
        return "0".into();
    }
    if v < 1000.0 {
        format!("{}", v.round() as u32)
    } else if v < 10_000.0 {
        let k = v / 1000.0;
        // 9999 rounds to 10.0k (5 chars); treat that as the cap.
        if k >= 9.95 {
            "10k+".into()
        } else {
            format!("{k:.1}k")
        }
    } else {
        "10k+".into()
    }
}

pub fn dps_in_label(incoming: f64) -> String {
    format!("↓{}", format_dps(incoming))
}

pub fn dps_out_label(outgoing: f64) -> String {
    format!("↑{}", format_dps(outgoing))
}

/// Title-strip / list-row label: `"Name - Jita"` when a system is
/// known, otherwise just the character name.
pub fn title_label(character: &str, system: Option<&str>) -> String {
    match system.filter(|s| !s.is_empty()) {
        Some(sys) => format!("{character} - {sys}"),
        None => character.to_string(),
    }
}

/// Thumbnail destination inside a preview window, in window coordinates
/// (right/bottom exclusive). The mirror fills the interior inside
/// `border` on every side. Labels are drawn on top of this rect, so an
/// alert does not change the destination.
pub fn thumbnail_dest(width: i32, height: i32, border: i32) -> (i32, i32, i32, i32) {
    let border = border.max(0);
    let left = border;
    let top = border;
    let right = (width - border).max(left + 1);
    let bottom = (height - border).max(top + 1);
    (left, top, right, bottom)
}

/// 1px black halo baked into label pixmaps so cream, white, and red
/// stay readable on a bright thumbnail. Linux window opacity and the
/// Windows layered bitmap scale the result; this function does not.
pub const HALO_PAD: i32 = 1;

/// Where a preview's name, DPS figures, and alert sit inside `outer`.
/// `top` is the name band. DPS slots share that band's vertical span.
/// `bottom` is the alert band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayBands {
    pub top: GridRect,
    pub dps_in: GridRect,
    pub dps_out: GridRect,
    pub bottom: GridRect,
}

/// Label rectangles inside a window or cell, inset by `frame`. Sizes are
/// already in device pixels. A band taller than the interior is clipped
/// so the name and the alert never overlap.
pub fn overlay_label_bands(
    outer: GridRect,
    frame: i32,
    name_h: i32,
    alert_h: i32,
    dps_slot: i32,
) -> OverlayBands {
    let frame = frame.max(0);
    let name_h = name_h.max(0);
    let alert_h = alert_h.max(0);
    let dps_slot = dps_slot.max(0);
    let inner_w = (outer.w - frame * 2).max(0);
    let inner_h = (outer.h - frame * 2).max(0);
    let top_h = name_h.min(inner_h);
    let bottom_h = alert_h.min(inner_h.saturating_sub(top_h));
    let top = GridRect {
        x: outer.x + frame,
        y: outer.y + frame,
        w: inner_w,
        h: top_h,
    };
    let slot = dps_slot.min(inner_w);
    OverlayBands {
        top,
        dps_in: GridRect {
            x: top.x,
            y: top.y,
            w: slot,
            h: top_h,
        },
        dps_out: GridRect {
            x: top.x + inner_w - slot,
            y: top.y,
            w: slot,
            h: top_h,
        },
        bottom: GridRect {
            x: outer.x + frame,
            y: outer.y + outer.h - frame - bottom_h,
            w: inner_w,
            h: bottom_h,
        },
    }
}

/// Bottom text rows of one compact-grid cell. `name` is the lower row.
/// `system` is the row directly above it when a solar system is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridNameRows {
    pub system: Option<GridRect>,
    pub name: GridRect,
}

/// Place the character name along the bottom of a compact-grid cell.
/// `row_h` is one text row (`GRID_SCRIM_PX`, already scaled). When
/// `has_system` is set, a second row of the same height sits directly
/// above the name. The name keeps its row; a cell with no room above
/// it drops the system row.
pub fn grid_name_rows(outer: GridRect, frame: i32, row_h: i32, has_system: bool) -> GridNameRows {
    let frame = frame.max(0);
    let row_h = row_h.max(0);
    let inner_w = (outer.w - frame * 2).max(0);
    let inner_h = (outer.h - frame * 2).max(0);
    let name_h = row_h.min(inner_h);
    let name = GridRect {
        x: outer.x + frame,
        y: outer.y + outer.h - frame - name_h,
        w: inner_w,
        h: name_h,
    };
    // A short leftover would slice the system glyphs. Drop the row
    // unless a full second band fits above the name.
    let room = inner_h.saturating_sub(name_h);
    let system = if has_system && row_h > 0 && room >= row_h {
        Some(GridRect {
            x: name.x,
            y: name.y - row_h,
            w: inner_w,
            h: row_h,
        })
    } else {
        None
    };
    GridNameRows { system, name }
}

/// Premultiplied BGRA (little-endian) for a coverage mask, with a black
/// halo `pad` pixels wide. The halo is the Chebyshev dilation of
/// `coverage`, and the glyph is composited over it, so partial coverage
/// stays the glyph color while empty neighbors become opaque black.
/// The buffer is `(width + 2*pad)` by `(height + 2*pad)`. `pad == 0`
/// is a straight premultiply — the glyph is not outlined.
pub fn cover_with_halo(
    coverage: &[u8],
    width: i32,
    height: i32,
    color: (u8, u8, u8),
    pad: i32,
) -> Vec<u8> {
    let width = width.max(0);
    let height = height.max(0);
    let pad = pad.max(0);
    let out_w = width + pad * 2;
    let out_h = height + pad * 2;
    if out_w <= 0 || out_h <= 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    let sample = |x: i32, y: i32| -> u8 {
        if x < 0 || y < 0 || x >= width || y >= height {
            return 0;
        }
        let idx = (y as usize) * (width as usize) + (x as usize);
        coverage.get(idx).copied().unwrap_or(0)
    };
    for oy in 0..out_h {
        for ox in 0..out_w {
            let sx = ox - pad;
            let sy = oy - pad;
            let g = sample(sx, sy) as u32;
            // Neighbors only. Including the pixel itself would darken
            // antialiased edges even when pad is 0.
            let mut halo = 0u8;
            for dy in -pad..=pad {
                for dx in -pad..=pad {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    halo = halo.max(sample(sx + dx, sy + dy));
                }
            }
            let halo = halo as u32;
            let a = g + halo * (255 - g) / 255;
            let idx = ((oy * out_w + ox) as usize) * 4;
            out[idx] = (color.2 as u32 * g / 255) as u8;
            out[idx + 1] = (color.1 as u32 * g / 255) as u8;
            out[idx + 2] = (color.0 as u32 * g / 255) as u8;
            out[idx + 3] = a as u8;
        }
    }
    out
}

/// Adjust a proposed (x, y) so the dragged window's edges snap to
/// nearby preview windows' edges when within `snap_threshold` pixels.
/// Each axis snaps independently so you can dock right-against-left
/// while also aligning tops. The first match per axis wins so densely
/// packed snap targets don't oscillate.
///
/// `snap_threshold` is a parameter rather than a constant because
/// Windows applies DPI scaling before calling.
pub fn snap_position(
    proposed_x: i32,
    proposed_y: i32,
    width: i32,
    height: i32,
    others: &[DragRect],
    snap_threshold: i32,
) -> (i32, i32) {
    let mut x = proposed_x;
    let mut y = proposed_y;
    let mut x_snapped = false;
    let mut y_snapped = false;
    let drag_right = proposed_x + width;
    let drag_bottom = proposed_y + height;
    let snap = snap_threshold;

    for other in others {
        // Edge-to-edge docking on the X axis (right of dragged ↔ left
        // of other, then left of dragged ↔ right of other).
        if !x_snapped && (drag_right - other.left).abs() <= snap {
            x = other.left - width;
            x_snapped = true;
        }
        if !x_snapped && (proposed_x - other.right).abs() <= snap {
            x = other.right;
            x_snapped = true;
        }
        // Same for the Y axis.
        if !y_snapped && (drag_bottom - other.top).abs() <= snap {
            y = other.top - height;
            y_snapped = true;
        }
        if !y_snapped && (proposed_y - other.bottom).abs() <= snap {
            y = other.bottom;
            y_snapped = true;
        }
        // Parallel-edge alignment so docked previews share a baseline.
        if !y_snapped && (proposed_y - other.top).abs() <= snap {
            y = other.top;
            y_snapped = true;
        }
        if !y_snapped && (drag_bottom - other.bottom).abs() <= snap {
            y = other.bottom - height;
            y_snapped = true;
        }
        if !x_snapped && (proposed_x - other.left).abs() <= snap {
            x = other.left;
            x_snapped = true;
        }
        if !x_snapped && (drag_right - other.right).abs() <= snap {
            x = other.right - width;
            x_snapped = true;
        }
    }

    (x, y)
}

/// Whether a preview window should be hidden right now, given the
/// "hide active client's preview" setting and whether this preview
/// mirrors the currently-active EVE client.
///
/// The cycle behavior the user sees falls straight out of this: when
/// they switch from client A to client B, A's `is_active` goes false
/// (its preview reappears) and B's goes true (its preview hides). Each
/// manager just re-evaluates this per preview whenever the active client
/// changes — there's no separate "show the one we cycled away from" path.
pub fn preview_should_hide(hide_active_setting: bool, is_active: bool) -> bool {
    hide_active_setting && is_active
}

/// Compact-grid columns. The panel slider uses the same range.
pub const GRID_COLUMNS_MIN: u32 = 1;
pub const GRID_COLUMNS_MAX: u32 = 5;
pub const GRID_COLUMNS_DEFAULT: u32 = 2;

/// Compact-grid cell width in reference pixels (96 DPI). Height is 16:9.
pub const GRID_CELL_WIDTH_MIN: u32 = 96;
pub const GRID_CELL_WIDTH_MAX: u32 = 240;
pub const GRID_CELL_WIDTH_DEFAULT: u32 = 160;

/// Gap between cells, and the gold frame around the whole grid window.
/// Reference pixels; Windows scales them with the cell size.
pub const GRID_GAP_PX: i32 = 1;
pub const GRID_OUTER_BORDER_PX: i32 = 2;

/// Height of one text row on a compact-grid cell. The character name
/// uses the bottom row. A known solar system uses a second row of the
/// same height directly above it. The thumbnail fills the cell; this
/// only places the text.
pub const GRID_SCRIM_PX: i32 = 16;
/// State border drawn inside a cell. The thumbnail stays inside it so
/// the red/gold frame is not covered by the mirror.
pub const GRID_CELL_BORDER_PX: i32 = 2;

/// Character-name size on a grid cell. Reference pixels.
pub const GRID_NAME_FONT_PX: f32 = 12.0;

/// One cell in window coordinates. `x`/`y` are the top-left; `w`/`h`
/// are the full cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCell {
    pub index: usize,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Axis-aligned box. `w` and `h` are sizes, not exclusive edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Result of [`grid_layout`]. Pixel fields are already scaled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridLayout {
    pub window_w: i32,
    pub window_h: i32,
    pub cell_w: i32,
    pub cell_h: i32,
    pub columns: u32,
    pub gap: i32,
    pub outer_border: i32,
    pub scrim: i32,
    pub cell_inset: i32,
    pub cells: Vec<GridCell>,
}

/// Lay out `count` cells. `columns` clamps to 1–5 and `cell_w` to
/// 96–240 before scaling, so a hand-edited config cannot produce a
/// zero-size window. `scale` is 1.0 on Linux and the system DPI scale
/// on Windows (1.0 at 96 DPI). Width stays `columns` cells wide even
/// when the last row is short, so the window does not jump narrower
/// as clients log out. `count == 0` is an empty layout (no window).
pub fn grid_layout(
    count: usize,
    columns: u32,
    cell_w: u32,
    gap: i32,
    outer_border: i32,
) -> GridLayout {
    grid_layout_scaled(count, columns, cell_w, gap, outer_border, 1.0)
}

/// [`grid_layout`] with an explicit DPI scale. Scale is applied after
/// the reference-pixel clamp.
pub fn grid_layout_scaled(
    count: usize,
    columns: u32,
    cell_w: u32,
    gap: i32,
    outer_border: i32,
    scale: f32,
) -> GridLayout {
    let columns = columns.clamp(GRID_COLUMNS_MIN, GRID_COLUMNS_MAX);
    let cell_w_ref = cell_w.clamp(GRID_CELL_WIDTH_MIN, GRID_CELL_WIDTH_MAX);
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let cell_w_px = scale_px(cell_w_ref as i32, scale).max(1);
    let cell_h_px = (cell_w_px * 9 / 16).max(1);
    let gap_px = scale_px(gap.max(0), scale).max(0);
    let border_px = scale_px(outer_border.max(0), scale).max(0);
    let scrim = scale_px(GRID_SCRIM_PX, scale).clamp(1, cell_h_px);
    let cell_inset = scale_px(GRID_CELL_BORDER_PX, scale).clamp(0, cell_w_px / 4);

    if count == 0 {
        return GridLayout {
            window_w: 0,
            window_h: 0,
            cell_w: cell_w_px,
            cell_h: cell_h_px,
            columns,
            gap: gap_px,
            outer_border: border_px,
            scrim,
            cell_inset,
            cells: Vec::new(),
        };
    }

    let cols = columns as usize;
    let rows = count.div_ceil(cols);
    let window_w = border_px * 2 + cols as i32 * cell_w_px + (cols as i32 - 1) * gap_px;
    let window_h = border_px * 2 + rows as i32 * cell_h_px + (rows as i32 - 1) * gap_px;

    let mut cells = Vec::with_capacity(count);
    for index in 0..count {
        let col = (index % cols) as i32;
        let row = (index / cols) as i32;
        cells.push(GridCell {
            index,
            x: border_px + col * (cell_w_px + gap_px),
            y: border_px + row * (cell_h_px + gap_px),
            w: cell_w_px,
            h: cell_h_px,
        });
    }

    GridLayout {
        window_w,
        window_h,
        cell_w: cell_w_px,
        cell_h: cell_h_px,
        columns,
        gap: gap_px,
        outer_border: border_px,
        scrim,
        cell_inset,
        cells,
    }
}

fn scale_px(n: i32, scale: f32) -> i32 {
    (n as f32 * scale).round() as i32
}

/// Rectangle the thumbnail is allowed to occupy: inside the state
/// border on every side. The name is drawn on top of this box.
pub fn grid_thumb_bounds(cell: GridCell, inset: i32) -> GridRect {
    let inset = inset.max(0);
    let x = cell.x + inset;
    let y = cell.y + inset;
    let w = (cell.w - inset * 2).max(1);
    let h = (cell.h - inset * 2).max(1);
    GridRect { x, y, w, h }
}

/// Letterbox `src_w`×`src_h` inside `bounds`. A zero source fills the
/// bounds. The result is centered and never larger than `bounds`.
pub fn fit_contain(src_w: i32, src_h: i32, bounds: GridRect) -> GridRect {
    if src_w <= 0 || src_h <= 0 || bounds.w <= 0 || bounds.h <= 0 {
        return bounds;
    }
    let src_aspect = src_w as f64 / src_h as f64;
    let box_aspect = bounds.w as f64 / bounds.h as f64;
    let (dw, dh) = if src_aspect > box_aspect {
        let dw = bounds.w;
        let dh = ((bounds.w as f64) / src_aspect).round() as i32;
        (dw, dh.max(1))
    } else {
        let dh = bounds.h;
        let dw = ((bounds.h as f64) * src_aspect).round() as i32;
        (dw.max(1), dh)
    };
    GridRect {
        x: bounds.x + (bounds.w - dw) / 2,
        y: bounds.y + (bounds.h - dh) / 2,
        w: dw.min(bounds.w),
        h: dh.min(bounds.h),
    }
}

/// Cell index under a window-local point, or `None` when the point is
/// on the frame or in a gap.
pub fn grid_cell_at(cells: &[GridCell], x: i32, y: i32) -> Option<usize> {
    cells.iter().find_map(|cell| {
        let inside = x >= cell.x && y >= cell.y && x < cell.x + cell.w && y < cell.y + cell.h;
        inside.then_some(cell.index)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference geometry used across the snap tests. The dragged window
    // is 100×100. With snap_threshold = 12 (the production value), edges
    // within 12px of a neighbor's edge should snap; edges 13px+ away
    // should not. `neighbor` sits far enough away that placing it at
    // various offsets exercises one snap condition at a time.
    const DRAG_W: i32 = 100;
    const DRAG_H: i32 = 100;
    const SNAP: i32 = 12;

    fn neighbor(left: i32, top: i32) -> DragRect {
        DragRect {
            left,
            top,
            right: left + 100,
            bottom: top + 100,
        }
    }

    #[test]
    fn title_label_without_system_is_name_only() {
        assert_eq!(title_label("Alpha", None), "Alpha");
        assert_eq!(title_label("Alpha", Some("")), "Alpha");
    }

    #[test]
    fn title_label_with_system() {
        assert_eq!(title_label("Alpha", Some("Jita")), "Alpha - Jita");
    }

    #[test]
    fn thumbnail_dest_is_the_interior_inside_the_frame() {
        // 320×180 preview, 3px border — same numbers the Windows manager
        // uses at 96 DPI. An alert does not change the rect.
        let (l, t, r, b) = thumbnail_dest(320, 180, 3);
        assert_eq!((l, t, r, b), (3, 3, 317, 177));
    }

    #[test]
    fn overlay_label_bands_preview_window() {
        let bands = overlay_label_bands(
            GridRect {
                x: 0,
                y: 0,
                w: 320,
                h: 180,
            },
            3,
            24,
            18,
            48,
        );
        assert_eq!(
            bands.top,
            GridRect {
                x: 3,
                y: 3,
                w: 314,
                h: 24
            }
        );
        assert_eq!(
            bands.dps_in,
            GridRect {
                x: 3,
                y: 3,
                w: 48,
                h: 24
            }
        );
        assert_eq!(
            bands.dps_out,
            GridRect {
                x: 269,
                y: 3,
                w: 48,
                h: 24
            }
        );
        assert_eq!(
            bands.bottom,
            GridRect {
                x: 3,
                y: 159,
                w: 314,
                h: 18
            }
        );
    }

    #[test]
    fn overlay_label_bands_grid_cell_bottom() {
        let bands = overlay_label_bands(
            GridRect {
                x: 2,
                y: 2,
                w: 160,
                h: 90,
            },
            2,
            16,
            16,
            0,
        );
        assert_eq!(
            bands.bottom,
            GridRect {
                x: 4,
                y: 74,
                w: 156,
                h: 16
            }
        );
    }

    #[test]
    fn cover_with_halo_paints_black_ring_and_premultiplies() {
        let px = cover_with_halo(&[255], 1, 1, (252, 250, 242), 1);
        assert_eq!(px.len(), 9 * 4);
        assert_eq!(&px[4 * 4..5 * 4], &[242, 250, 252, 255]);
        for i in 0..9 {
            if i == 4 {
                continue;
            }
            assert_eq!(&px[i * 4..i * 4 + 4], &[0, 0, 0, 255], "neighbor {i}");
        }
    }

    #[test]
    fn cover_with_halo_pad_zero_is_premultiply() {
        let px = cover_with_halo(&[128], 1, 1, (255, 0, 0), 0);
        assert_eq!(px, vec![0, 0, 128, 128]);
    }

    #[test]
    fn format_dps_thresholds() {
        assert_eq!(format_dps(0.4), "0");
        assert_eq!(format_dps(42.2), "42");
        assert_eq!(format_dps(999.4), "999");
        assert_eq!(format_dps(1234.0), "1.2k");
        assert_eq!(format_dps(9800.0), "9.8k");
        assert_eq!(format_dps(10_000.0), "10k+");
        assert_eq!(format_dps(45_000.0), "10k+");
    }

    #[test]
    fn dps_labels() {
        assert_eq!(dps_in_label(1200.0), "↓1.2k");
        assert_eq!(dps_out_label(840.0), "↑840");
    }

    #[test]
    fn no_others_returns_proposed_unchanged() {
        let (x, y) = snap_position(100, 100, DRAG_W, DRAG_H, &[], SNAP);
        assert_eq!((x, y), (100, 100));
    }

    #[test]
    fn empty_others_at_extreme_proposed_still_identity() {
        let (x, y) = snap_position(-50_000, 32_000, DRAG_W, DRAG_H, &[], SNAP);
        assert_eq!((x, y), (-50_000, 32_000));
    }

    #[test]
    fn no_snap_when_all_edges_beyond_threshold() {
        // Neighbor at (213, 213) — closest edge (left=213) is 13px from
        // drag's right edge (200). Past the inclusive threshold of 12.
        let others = [neighbor(213, 213)];
        let (x, y) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!((x, y), (100, 100));
    }

    #[test]
    fn dock_drag_right_to_other_left() {
        // drag right (200) is 10px from other.left (210). Snaps so drag's
        // right edge equals other's left edge.
        let others = [neighbor(210, 100)];
        let (x, _) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(x, 110); // x + DRAG_W == other.left
    }

    #[test]
    fn dock_drag_left_to_other_right() {
        // drag left (100) is 10px from other.right (90). Snaps so drag's
        // left equals other's right.
        let others = [DragRect {
            left: -10,
            top: 100,
            right: 90,
            bottom: 200,
        }];
        let (x, _) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(x, 90);
    }

    #[test]
    fn dock_drag_bottom_to_other_top() {
        let others = [neighbor(100, 210)]; // other.top = 210, drag.bottom = 200
        let (_, y) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(y, 110); // y + DRAG_H == other.top
    }

    #[test]
    fn dock_drag_top_to_other_bottom() {
        let others = [DragRect {
            left: 100,
            top: -10,
            right: 200,
            bottom: 90,
        }];
        let (_, y) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(y, 90);
    }

    #[test]
    fn align_top_to_top() {
        // Y offset by 5 — within snap range but neither edge-to-edge
        // condition matches (other is far below drag's bottom and far
        // above drag's top). Top-alignment kicks in.
        let others = [neighbor(400, 105)];
        let (_, y) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(y, 105); // proposed_y snaps to other.top
    }

    #[test]
    fn align_bottom_to_bottom() {
        // drag.bottom = 200, other.bottom = 205. Difference 5 ≤ snap.
        // No edge-to-edge match (other is far away in X), so
        // bottom-alignment wins. New y = other.bottom - drag_height.
        let others = [DragRect {
            left: 400,
            top: 105,
            right: 500,
            bottom: 205,
        }];
        let (_, y) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(y, 105);
    }

    #[test]
    fn align_left_to_left() {
        // X offset of 5 with neighbor far below — only alignment paths
        // can match. proposed_x snaps to other.left.
        let others = [neighbor(105, 400)];
        let (x, _) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(x, 105);
    }

    #[test]
    fn align_right_to_right() {
        // drag.right = 200, other.right = 195. Difference 5 ≤ snap.
        let others = [DragRect {
            left: 95,
            top: 400,
            right: 195,
            bottom: 500,
        }];
        let (x, _) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(x, 95); // x = other.right - drag_width
    }

    #[test]
    fn x_and_y_snap_independently_in_one_call() {
        // Neighbor positioned to dock right-to-left on X AND align tops
        // on Y. Both axes should snap from the same neighbor.
        let others = [neighbor(210, 105)];
        let (x, y) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!((x, y), (110, 105));
    }

    #[test]
    fn x_axis_inclusive_at_threshold() {
        // Distance == snap should still snap (the check is `<= snap`).
        // drag.right = 200, other.left = 212 → distance 12.
        let others = [neighbor(212, 100)];
        let (x, _) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(x, 112); // snapped: x + width = other.left
    }

    #[test]
    fn x_axis_no_snap_one_past_threshold() {
        // Distance == snap + 1 should not snap.
        let others = [neighbor(213, 100)];
        let (x, _) = snap_position(100, 100, DRAG_W, DRAG_H, &others, SNAP);
        assert_eq!(x, 100); // unchanged
    }

    #[test]
    fn first_x_match_wins_among_multiple_neighbors() {
        // Both neighbors are within snap range on X. A is processed
        // first and snaps the X axis; B should be ignored for X even
        // though its edge would also qualify. If first-match-wins
        // breaks, the test catches B's snap result (x=112) instead of
        // A's (x=110).
        let a = neighbor(210, 400); // far enough on Y to not snap Y
        let b = neighbor(212, 400);
        let (x, _) = snap_position(100, 100, DRAG_W, DRAG_H, &[a, b], SNAP);
        assert_eq!(x, 110);
    }

    #[test]
    fn first_y_match_wins_among_multiple_neighbors() {
        let a = neighbor(400, 210);
        let b = neighbor(400, 212);
        let (_, y) = snap_position(100, 100, DRAG_W, DRAG_H, &[a, b], SNAP);
        assert_eq!(y, 110);
    }

    #[test]
    fn x_snaps_from_first_other_y_from_second() {
        // First neighbor is positioned to only snap X; second is
        // positioned to only snap Y. Combined call should pick up
        // both snaps cumulatively.
        let x_only = neighbor(210, 400);
        let y_only = neighbor(400, 210);
        let (x, y) = snap_position(100, 100, DRAG_W, DRAG_H, &[x_only, y_only], SNAP);
        assert_eq!((x, y), (110, 110));
    }

    #[test]
    fn edge_to_edge_preferred_over_alignment_within_same_other() {
        // Geometry where BOTH y-axis snap rules would match from the
        // same neighbor; the loop body checks edge-to-edge first, so
        // it wins.
        //
        // drag (100, 105), 100×100 → drag.top = 105, drag.bottom = 205.
        // other.top = 200, other.bottom = 210.
        //   edge-to-edge bottom↔top: |205 - 200| = 5 ≤ snap → y = 200 - 100 = 100.
        //   alignment bottom↔bottom: |205 - 210| = 5 ≤ snap → y = 210 - 100 = 110.
        //   alignment top↔top: |105 - 200| = 95 → no.
        //   edge top↔bottom: |105 - 210| = 105 → no.
        // Outcome 100 (edge wins) is distinguishable from both
        // 105 (no snap) and 110 (alignment wins).
        let other = DragRect {
            left: 400, // far on X so X axis stays unchanged
            top: 200,
            right: 500,
            bottom: 210,
        };
        let (x, y) = snap_position(100, 105, DRAG_W, DRAG_H, &[other], SNAP);
        assert_eq!((x, y), (100, 100));
    }

    #[test]
    fn works_with_negative_coordinates() {
        // Secondary monitors on multi-display setups commonly map to
        // negative root coordinates. Snap should work the same regardless
        // of sign.
        let others = [DragRect {
            left: -90,
            top: -90,
            right: -10, // drag.left (-100) is 10px from this. Snap.
            bottom: 10,
        }];
        let (x, _) = snap_position(-100, -100, DRAG_W, DRAG_H, &others, SNAP);
        // drag.left (-100) vs other.right (-10): diff 90 → no edge dock.
        // drag.right (0) vs other.left (-90): diff 90 → no edge dock.
        // drag.left vs other.left (-90): diff 10 → alignment-left match.
        // Result: x = other.left = -90.
        assert_eq!(x, -90);
    }

    #[test]
    fn zero_snap_threshold_disables_all_snapping() {
        // With snap_threshold = 0, only exactly-coincident edges match
        // (diff.abs() <= 0 means diff == 0). The test uses a clearly
        // non-coincident neighbor; no snap should occur.
        let others = [neighbor(210, 100)];
        let (x, y) = snap_position(100, 100, DRAG_W, DRAG_H, &others, 0);
        assert_eq!((x, y), (100, 100));
    }

    #[test]
    fn preview_hidden_only_when_setting_on_and_active() {
        // Setting off: nothing is ever hidden, active or not.
        assert!(!preview_should_hide(false, false));
        assert!(!preview_should_hide(false, true));
        // Setting on: only the active client's preview hides.
        assert!(!preview_should_hide(true, false));
        assert!(preview_should_hide(true, true));
    }

    #[test]
    fn cycling_hides_new_active_and_reveals_previous() {
        // Two previews, A and B; the setting is on. Model the active
        // client by which `is_active` flag is set, then "cycle" by moving
        // the active flag from A to B and re-evaluating both — exactly
        // what the managers do on a focus change.
        let on = true;

        // A is the active client: A hidden, B visible.
        let (a_active, b_active) = (true, false);
        assert!(preview_should_hide(on, a_active));
        assert!(!preview_should_hide(on, b_active));

        // Cycle to B: A is no longer active (reappears), B now hides.
        let (a_active, b_active) = (false, true);
        assert!(!preview_should_hide(on, a_active));
        assert!(preview_should_hide(on, b_active));
    }

    #[test]
    fn grid_default_twenty_clients_is_a_side_dock() {
        // 2×160 cells, 16:9, 1px gaps, 2px frame.
        // width  = 4 + 2*160 + 1 = 325
        // height = 4 + 10*90 + 9 = 913
        let layout = grid_layout(20, 2, 160, GRID_GAP_PX, GRID_OUTER_BORDER_PX);
        assert_eq!(layout.columns, 2);
        assert_eq!((layout.cell_w, layout.cell_h), (160, 90));
        assert_eq!((layout.window_w, layout.window_h), (325, 913));
        assert_eq!(layout.cells.len(), 20);
        assert_eq!(
            (layout.cells[0].x, layout.cells[0].y),
            (GRID_OUTER_BORDER_PX, GRID_OUTER_BORDER_PX)
        );
        assert_eq!((layout.cells[1].x, layout.cells[1].y), (163, 2));
        // Next row starts under the first cell, past one gap.
        assert_eq!((layout.cells[2].x, layout.cells[2].y), (2, 93));
    }

    #[test]
    fn grid_partial_last_row_stays_left_aligned() {
        let layout = grid_layout(19, 2, 160, 1, 2);
        assert_eq!(layout.cells.len(), 19);
        // 10 rows either way — the short last row does not shrink the window.
        assert_eq!(layout.window_h, grid_layout(20, 2, 160, 1, 2).window_h);
        let last = layout.cells[18];
        assert_eq!(last.index, 18);
        assert_eq!(last.x, 2);
        assert_eq!(last.y, 2 + 9 * (90 + 1));
    }

    #[test]
    fn grid_one_client_keeps_the_column_width() {
        let layout = grid_layout(1, 2, 160, 1, 2);
        assert_eq!(layout.window_w, 325);
        assert_eq!(layout.window_h, 2 * 2 + 90);
        assert_eq!(layout.cells.len(), 1);
    }

    #[test]
    fn grid_zero_clients_is_empty() {
        let layout = grid_layout(0, 2, 160, 1, 2);
        assert_eq!((layout.window_w, layout.window_h), (0, 0));
        assert!(layout.cells.is_empty());
    }

    #[test]
    fn grid_clamps_columns_and_cell_width() {
        let wide = grid_layout(1, 9, 999, 1, 2);
        assert_eq!(wide.columns, GRID_COLUMNS_MAX);
        assert_eq!(wide.cell_w, GRID_CELL_WIDTH_MAX as i32);
        let tiny = grid_layout(1, 0, 10, 1, 2);
        assert_eq!(tiny.columns, GRID_COLUMNS_MIN);
        assert_eq!(tiny.cell_w, GRID_CELL_WIDTH_MIN as i32);
    }

    #[test]
    fn grid_scale_two_doubles_the_default_cell() {
        let layout = grid_layout_scaled(1, 2, 160, 1, 2, 2.0);
        assert_eq!((layout.cell_w, layout.cell_h), (320, 180));
        assert_eq!(layout.gap, 2);
        assert_eq!(layout.outer_border, 4);
        assert_eq!(layout.scrim, 32);
    }

    #[test]
    fn grid_hit_test_ignores_frame_and_gap() {
        let layout = grid_layout(4, 2, 160, 1, 2);
        assert_eq!(grid_cell_at(&layout.cells, 2, 2), Some(0));
        assert_eq!(grid_cell_at(&layout.cells, 161, 2), Some(0));
        assert_eq!(grid_cell_at(&layout.cells, 162, 2), None);
        assert_eq!(grid_cell_at(&layout.cells, 163, 2), Some(1));
        assert_eq!(grid_cell_at(&layout.cells, 0, 0), None);
        assert_eq!(grid_cell_at(&layout.cells, 2, 93), Some(2));
    }

    #[test]
    fn grid_name_rows_without_system_is_the_bottom_band() {
        let rows = grid_name_rows(
            GridRect {
                x: 2,
                y: 2,
                w: 160,
                h: 90,
            },
            2,
            16,
            false,
        );
        assert_eq!(rows.system, None);
        assert_eq!(
            rows.name,
            GridRect {
                x: 4,
                y: 74,
                w: 156,
                h: 16
            }
        );
    }

    #[test]
    fn grid_name_rows_puts_the_system_above_the_name() {
        let rows = grid_name_rows(
            GridRect {
                x: 2,
                y: 2,
                w: 160,
                h: 90,
            },
            2,
            16,
            true,
        );
        assert_eq!(
            rows.name,
            GridRect {
                x: 4,
                y: 74,
                w: 156,
                h: 16
            }
        );
        assert_eq!(
            rows.system,
            Some(GridRect {
                x: 4,
                y: 58,
                w: 156,
                h: 16
            })
        );
    }

    #[test]
    fn grid_name_rows_drops_a_system_row_that_would_be_short() {
        // 10px left above a 16px name: not enough for a full system row.
        let rows = grid_name_rows(
            GridRect {
                x: 0,
                y: 0,
                w: 40,
                h: 30,
            },
            2,
            16,
            true,
        );
        assert_eq!(rows.system, None);
        assert_eq!(rows.name.h, 16);
        assert_eq!(rows.name.y, 12);
    }

    #[test]
    fn grid_name_rows_drops_the_system_when_only_the_name_fits() {
        // Frame 2px plus one 16px row: nothing left above the name.
        let rows = grid_name_rows(
            GridRect {
                x: 0,
                y: 0,
                w: 40,
                h: 20,
            },
            2,
            16,
            true,
        );
        assert_eq!(rows.system, None);
        assert_eq!(
            rows.name,
            GridRect {
                x: 2,
                y: 2,
                w: 36,
                h: 16
            }
        );
    }

    #[test]
    fn grid_thumb_bounds_fill_inside_the_frame() {
        let cell = GridCell {
            index: 0,
            x: 2,
            y: 2,
            w: 160,
            h: 90,
        };
        let bounds = grid_thumb_bounds(cell, 2);
        assert_eq!(
            bounds,
            GridRect {
                x: 4,
                y: 4,
                w: 156,
                h: 86
            }
        );
    }

    #[test]
    fn fit_contain_letterboxes_wide_and_tall_sources() {
        let bounds = GridRect {
            x: 10,
            y: 20,
            w: 160,
            h: 74,
        };
        // 16:9 into a wider box: fit the height, center horizontally.
        let wide = fit_contain(1920, 1080, bounds);
        assert_eq!(wide.h, 74);
        assert_eq!(wide.w, 132);
        assert_eq!(wide.x, 24);
        assert_eq!(wide.y, 20);

        // Wider than the box: fit the width, center vertically.
        let cinematic = fit_contain(320, 100, bounds);
        assert_eq!(cinematic.w, 160);
        assert_eq!(cinematic.h, 50);
        assert_eq!(cinematic.x, 10);
        assert_eq!(cinematic.y, 32);

        assert_eq!(fit_contain(0, 0, bounds), bounds);
    }
}
