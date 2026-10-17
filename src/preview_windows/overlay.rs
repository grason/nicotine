//! Click-through text layer above a DWM thumbnail.
//!
//! DWM draws the mirror above GDI in the owner window, so a name painted
//! in `WM_PAINT` disappears wherever the thumbnail dest overlaps it.
//! Labels and the centered alert icon live in an owned layered popup
//! instead. `WS_EX_TRANSPARENT` lets click and drag hit the owner. The
//! owner is topmost; ownership keeps this window above it without its
//! own `WS_EX_TOPMOST`.
//!
//! `SetLayeredWindowAttributes` and `UpdateLayeredWindow` cannot combine
//! on one HWND, so the opacity slider is baked into the bitmap. GDI
//! `DrawText` leaves alpha at 0: draw antialiased (not ClearType) white
//! on black, take coverage from the color channels, then
//! [`cover_with_halo`].

use std::sync::OnceLock;

use crate::alert_icon::{alert_icon_px, raster_icon, shown_icon, ShownIcon};
use crate::eve_logs::AlertKind;
use crate::preview_common::{
    cover_with_halo, grid_thumb_bounds, overlay_label_bands, thumbnail_dest, GridCell, GridRect,
    DPS_SLOT_PX, HALO_PAD,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, DrawTextW, GetDC,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER,
    ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS,
    DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, HDC, HFONT,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowRect, RegisterClassExW, SetWindowPos,
    ShowWindow, UpdateLayeredWindow, HCURSOR, HICON, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::{
    create_font, px, register_embedded_fonts, PreviewWindowState, BORDER_WIDTH, TITLE_HEIGHT,
};

const OVERLAY_CLASS: &str = "NicotineOverlayWnd\0";

/// Owned popup that carries the preview or grid labels.
pub(super) struct TextOverlay {
    hwnd: HWND,
    key: String,
    shown: bool,
}

impl Drop for TextOverlay {
    fn drop(&mut self) {
        self.destroy();
    }
}

impl TextOverlay {
    pub(super) fn create(owner: HWND) -> Self {
        let class_name: Vec<u16> = OVERLAY_CLASS.encode_utf16().collect();
        let title: Vec<u16> = "Nicotine\0".encode_utf16().collect();
        let Ok(module) = (unsafe { GetModuleHandleW(None) }) else {
            eprintln!("preview text layer was not created: no module handle");
            return Self {
                hwnd: HWND::default(),
                key: String::new(),
                shown: false,
            };
        };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                Some(HINSTANCE(module.0)),
                None,
            )
        };
        match hwnd {
            Ok(hwnd) => Self {
                hwnd,
                key: String::new(),
                shown: false,
            },
            Err(e) => {
                eprintln!("preview text layer was not created: {e}");
                Self {
                    hwnd: HWND::default(),
                    key: String::new(),
                    shown: false,
                }
            }
        }
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub(super) fn destroy(&mut self) {
        if self.hwnd.is_invalid() {
            return;
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        self.hwnd = HWND::default();
        self.shown = false;
    }

    pub(super) fn matches(&self, key: &str) -> bool {
        self.key == key
    }

    /// `ShowWindow(SW_HIDE)` on the owner does not always hide an owned
    /// popup, so hide-active calls this explicitly.
    pub(super) fn set_visible(&mut self, visible: bool) {
        if self.hwnd.is_invalid() || self.shown == visible {
            return;
        }
        self.shown = visible;
        unsafe {
            let cmd = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
            let _ = ShowWindow(self.hwnd, cmd);
        }
    }

    pub(super) fn move_to(&self, owner: HWND) {
        if self.hwnd.is_invalid() {
            return;
        }
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(owner, &mut rect).is_err() {
                return;
            }
            move_overlay_to(self.hwnd, rect.left, rect.top);
        }
    }

    /// Replace the bitmap. The key is stored only after a successful
    /// `UpdateLayeredWindow`, so a failed present is retried next tick.
    pub(super) fn present(&mut self, owner: HWND, pixels: &[u8], w: i32, h: i32, key: String) {
        if self.hwnd.is_invalid() || w <= 0 || h <= 0 {
            return;
        }
        let expected = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if pixels.len() != expected {
            // A size we refused to rasterize. Remember the key so the
            // reconcile tick does not rebuild it forever.
            self.key = key;
            return;
        }
        let mut rect = RECT::default();
        unsafe {
            if GetWindowRect(owner, &mut rect).is_err() {
                return;
            }
            if present_dib(
                self.hwnd,
                POINT {
                    x: rect.left,
                    y: rect.top,
                },
                pixels,
                w,
                h,
            ) {
                self.key = key;
            }
        }
    }
}

/// Drag path. Position only — the bitmap does not change while the
/// window moves. Owned popups do not follow their owner.
pub(super) fn move_overlay_to(overlay: HWND, x: i32, y: i32) {
    if overlay.is_invalid() {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            overlay,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub(super) fn register_class(module: windows::Win32::Foundation::HMODULE, cursor: HCURSOR) {
    let class_name: Vec<u16> = OVERLAY_CLASS.encode_utf16().collect();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: Default::default(),
        lpfnWndProc: Some(overlay_wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module.into(),
        hIcon: HICON::default(),
        hCursor: cursor,
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        hIconSm: HICON::default(),
    };
    unsafe {
        let _ = RegisterClassExW(&wc);
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub(super) struct Glyph {
    pub text: String,
    pub color: (u8, u8, u8),
    pub rect: RECT,
    pub font: HFONT,
    pub flags: DRAW_TEXT_FORMAT,
}

pub(super) fn colorref_rgb(color: COLORREF) -> (u8, u8, u8) {
    let v = color.0;
    (
        (v & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        ((v >> 16) & 0xff) as u8,
    )
}

/// One premultiplied icon already including its halo.
pub(super) struct Sprite<'a> {
    pub pixels: &'a [u8],
    pub w: i32,
    pub h: i32,
    pub x: i32,
    pub y: i32,
}

/// Premultiplied BGRA for `glyphs` and `sprites`, with the frame rects
/// in `clear` punched back to transparent so a halo cannot cover the
/// state border. Sprites are blended before the opacity scale and
/// before the punch. `opacity` is the layered-window byte (255 = opaque).
pub(super) fn compose(
    width: i32,
    height: i32,
    glyphs: &[Glyph],
    sprites: &[Sprite<'_>],
    opacity: u8,
    clear: &[RECT],
) -> Vec<u8> {
    if width <= 0 || height <= 0 {
        return Vec::new();
    }
    let Some(len) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
    else {
        return Vec::new();
    };
    // A 20-client grid at high DPI stays well under this. Larger than
    // that, skip the bitmap rather than allocating a desktop-sized DIB.
    if len > 64 * 1024 * 1024 {
        return Vec::new();
    }
    let mut dest = vec![0u8; len];
    for glyph in glyphs {
        if glyph.text.is_empty() {
            continue;
        }
        let gw = glyph.rect.right - glyph.rect.left;
        let gh = glyph.rect.bottom - glyph.rect.top;
        if gw <= 0 || gh <= 0 {
            continue;
        }
        let coverage = draw_coverage(&glyph.text, glyph.font, gw, gh, glyph.flags);
        if coverage.len() != (gw as usize) * (gh as usize) {
            continue;
        }
        let halo = cover_with_halo(&coverage, gw, gh, glyph.color, HALO_PAD);
        let hw = gw + HALO_PAD * 2;
        let hh = gh + HALO_PAD * 2;
        blit_over(
            &mut dest,
            width,
            height,
            Blit {
                pixels: &halo,
                w: hw,
                h: hh,
                x: glyph.rect.left - HALO_PAD,
                y: glyph.rect.top - HALO_PAD,
            },
        );
    }
    for sprite in sprites {
        blit_over(
            &mut dest,
            width,
            height,
            Blit {
                pixels: sprite.pixels,
                w: sprite.w,
                h: sprite.h,
                x: sprite.x,
                y: sprite.y,
            },
        );
    }
    if opacity < 255 {
        for channel in &mut dest {
            *channel = (*channel as u16 * opacity as u16 / 255) as u8;
        }
    }
    for rect in clear {
        clear_rect(&mut dest, width, height, *rect);
    }
    dest
}

pub(super) fn preview_pixels(
    state: &PreviewWindowState,
    width: i32,
    height: i32,
    opacity: u8,
) -> Vec<u8> {
    let border = px(BORDER_WIDTH);
    let bands = overlay_label_bands(
        GridRect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        },
        border,
        px(TITLE_HEIGHT),
        0,
        px(DPS_SLOT_PX),
    );
    let font = overlay_body_font();
    let white = (255, 255, 255);
    let pad = px(6);
    let mut glyphs = Vec::new();
    push_glyph(
        &mut glyphs,
        &state.title_label,
        white,
        rect_of(bands.top),
        font,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
    );
    if state.showing_dps {
        let mut incoming = rect_of(bands.dps_in);
        incoming.left += pad;
        push_glyph(
            &mut glyphs,
            &state.dps_in_label,
            white,
            incoming,
            font,
            DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        let mut outgoing = rect_of(bands.dps_out);
        outgoing.right -= pad;
        push_glyph(
            &mut glyphs,
            &state.dps_out_label,
            white,
            outgoing,
            font,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
    }
    let (left, top, right, bottom) = thumbnail_dest(width, height, border);
    let mut rasters = Vec::new();
    if let Some(icon) = shown_icon(state.taking_damage, state.alert_kind) {
        if let Some(drawn) = centered_icon(
            icon,
            GridRect {
                x: left,
                y: top,
                w: right - left,
                h: bottom - top,
            },
        ) {
            rasters.push(drawn);
        }
    }
    let sprites = sprites_of(&rasters);
    let clear = frame_rects(width, height, border);
    compose(width, height, &glyphs, &sprites, opacity, &clear)
}

/// One grid cell's name plus the alert icon, if any. The name stays cream.
/// `taking` draws crossed swords in the icon slot for the damage flash.
pub(super) struct GridLabel {
    pub cell: GridRect,
    pub text: String,
    pub kind: Option<AlertKind>,
    pub taking: bool,
}

/// Cream names along the bottom of each cell, and an icon centered
/// on the thumbnail. The outer border and each cell frame are punched
/// clear so a text halo stays off the GDI state border underneath.
pub(super) fn grid_pixels(
    width: i32,
    height: i32,
    labels: &[GridLabel],
    inset: i32,
    outer_border: i32,
    scrim: i32,
    opacity: u8,
) -> Vec<u8> {
    let font = grid_font();
    let cream = colorref_rgb(super::NICOTINE_CREAM);
    let mut glyphs = Vec::new();
    let mut rasters = Vec::new();
    let mut clear = frame_rects(width, height, outer_border);
    for label in labels {
        let band = overlay_label_bands(label.cell, inset, scrim, scrim, 0).bottom;
        push_glyph(
            &mut glyphs,
            &label.text,
            cream,
            rect_of(band),
            font,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        if let Some(icon) = shown_icon(label.taking, label.kind) {
            let bounds = grid_thumb_bounds(
                GridCell {
                    index: 0,
                    x: label.cell.x,
                    y: label.cell.y,
                    w: label.cell.w,
                    h: label.cell.h,
                },
                inset,
            );
            if let Some(drawn) = centered_icon(icon, bounds) {
                rasters.push(drawn);
            }
        }
        clear.extend(cell_frame_rects(label.cell, inset));
    }
    let sprites = sprites_of(&rasters);
    compose(width, height, &glyphs, &sprites, opacity, &clear)
}

/// Icon centered in `bounds`. The bitmap includes a 1px transparent pad.
fn centered_icon(icon: ShownIcon, bounds: GridRect) -> Option<(Vec<u8>, i32, i32, i32)> {
    if bounds.w <= 0 || bounds.h <= 0 {
        return None;
    }
    let icon_px = alert_icon_px(bounds.w.min(bounds.h), super::dpi_scale());
    let side = icon_px + HALO_PAD * 2;
    let raster = raster_icon(icon, icon_px);
    let x = bounds.x + (bounds.w - side) / 2;
    let y = bounds.y + (bounds.h - side) / 2;
    Some((raster, side, x, y))
}

fn sprites_of(rasters: &[(Vec<u8>, i32, i32, i32)]) -> Vec<Sprite<'_>> {
    rasters
        .iter()
        .map(|(pixels, side, x, y)| Sprite {
            pixels,
            w: *side,
            h: *side,
            x: *x,
            y: *y,
        })
        .collect()
}

pub(super) fn grid_font() -> HFONT {
    static SLOT: OnceLock<isize> = OnceLock::new();
    let raw = *SLOT.get_or_init(|| {
        register_embedded_fonts();
        let height = px(-(crate::preview_common::GRID_NAME_FONT_PX as i32));
        unsafe { create_font("JetBrains Mono", height, ANTIALIASED_QUALITY).0 as isize }
    });
    HFONT(raw as *mut _)
}

fn overlay_body_font() -> HFONT {
    static SLOT: OnceLock<isize> = OnceLock::new();
    let raw = *SLOT.get_or_init(|| {
        register_embedded_fonts();
        unsafe { create_font("JetBrains Mono", px(-14), ANTIALIASED_QUALITY).0 as isize }
    });
    HFONT(raw as *mut _)
}

fn push_glyph(
    glyphs: &mut Vec<Glyph>,
    text: &str,
    color: (u8, u8, u8),
    rect: RECT,
    font: HFONT,
    flags: DRAW_TEXT_FORMAT,
) {
    if text.is_empty() || rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    glyphs.push(Glyph {
        text: text.to_string(),
        color,
        rect,
        font,
        flags,
    });
}

fn rect_of(band: GridRect) -> RECT {
    RECT {
        left: band.x,
        top: band.y,
        right: band.x + band.w,
        bottom: band.y + band.h,
    }
}

fn cell_frame_rects(cell: GridRect, inset: i32) -> [RECT; 4] {
    let inset = inset.max(0);
    let x = cell.x;
    let y = cell.y;
    let right = cell.x + cell.w;
    let bottom = cell.y + cell.h;
    [
        RECT {
            left: x,
            top: y,
            right,
            bottom: y + inset,
        },
        RECT {
            left: x,
            top: bottom - inset,
            right,
            bottom,
        },
        RECT {
            left: x,
            top: y,
            right: x + inset,
            bottom,
        },
        RECT {
            left: right - inset,
            top: y,
            right,
            bottom,
        },
    ]
}

fn frame_rects(width: i32, height: i32, border: i32) -> Vec<RECT> {
    let border = border.max(0);
    vec![
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: border,
        },
        RECT {
            left: 0,
            top: height - border,
            right: width,
            bottom: height,
        },
        RECT {
            left: 0,
            top: 0,
            right: border,
            bottom: height,
        },
        RECT {
            left: width - border,
            top: 0,
            right: width,
            bottom: height,
        },
    ]
}

fn draw_coverage(
    text: &str,
    font: HFONT,
    width: i32,
    height: i32,
    flags: DRAW_TEXT_FORMAT,
) -> Vec<u8> {
    if width <= 0 || height <= 0 || text.is_empty() {
        return Vec::new();
    }
    unsafe {
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let Ok(bitmap) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return Vec::new();
        };
        if bits.is_null() {
            let _ = DeleteObject(bitmap.into());
            return Vec::new();
        }
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            let _ = DeleteObject(bitmap.into());
            return Vec::new();
        }
        let old_bmp = SelectObject(dc, bitmap.into());
        let old_font = SelectObject(dc, font.into());
        // Antialiased white on a zeroed (black) DIB. Coverage is the
        // brightest channel. ClearType would fringe in color and the
        // halo would pick up the wrong shape.
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let _ = DrawTextW(dc, &mut buf, &mut rect, flags);
        let nbytes = (width as usize) * (height as usize) * 4;
        let raw = std::slice::from_raw_parts(bits as *const u8, nbytes);
        let mut coverage = Vec::with_capacity((width as usize) * (height as usize));
        for px in raw.as_chunks::<4>().0 {
            coverage.push(px[0].max(px[1]).max(px[2]));
        }
        let _ = SelectObject(dc, old_font);
        let _ = SelectObject(dc, old_bmp);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        coverage
    }
}

struct Blit<'a> {
    pixels: &'a [u8],
    w: i32,
    h: i32,
    x: i32,
    y: i32,
}

fn blit_over(dest: &mut [u8], dest_w: i32, dest_h: i32, src: Blit<'_>) {
    for sy in 0..src.h {
        let y = src.y + sy;
        if y < 0 || y >= dest_h {
            continue;
        }
        for sx in 0..src.w {
            let x = src.x + sx;
            if x < 0 || x >= dest_w {
                continue;
            }
            let si = ((sy * src.w + sx) as usize) * 4;
            let di = ((y * dest_w + x) as usize) * 4;
            if si + 3 >= src.pixels.len() || di + 3 >= dest.len() {
                continue;
            }
            let sa = src.pixels[si + 3] as u32;
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dest[di..di + 4].copy_from_slice(&src.pixels[si..si + 4]);
                continue;
            }
            let inv = 255 - sa;
            for channel in 0..4 {
                let blended =
                    src.pixels[si + channel] as u32 + dest[di + channel] as u32 * inv / 255;
                dest[di + channel] = blended as u8;
            }
        }
    }
}

/// Alpha-blend a top-down premultiplied BGRA bitmap onto `hdc`.
/// Used by the client list, which is GDI rather than a layered popup.
pub(super) unsafe fn blit_premul(hdc: HDC, x: i32, y: i32, pixels: &[u8], w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let Some(need) = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
    else {
        return;
    };
    if pixels.len() < need {
        return;
    }
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let Ok(bitmap) = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        return;
    };
    if bits.is_null() {
        let _ = DeleteObject(bitmap.into());
        return;
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, need);
    let mem = CreateCompatibleDC(Some(hdc));
    if mem.is_invalid() {
        let _ = DeleteObject(bitmap.into());
        return;
    }
    let old = SelectObject(mem, bitmap.into());
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _drawn = AlphaBlend(hdc, x, y, w, h, mem, 0, 0, w, h, blend).as_bool();
    let _ = SelectObject(mem, old);
    let _ = DeleteObject(bitmap.into());
    let _ = DeleteDC(mem);
}

fn clear_rect(dest: &mut [u8], dest_w: i32, dest_h: i32, rect: RECT) {
    let x0 = rect.left.max(0);
    let y0 = rect.top.max(0);
    let x1 = rect.right.min(dest_w);
    let y1 = rect.bottom.min(dest_h);
    for y in y0..y1 {
        for x in x0..x1 {
            let di = ((y * dest_w + x) as usize) * 4;
            if di + 4 <= dest.len() {
                dest[di..di + 4].fill(0);
            }
        }
    }
}

unsafe fn present_dib(hwnd: HWND, origin: POINT, pixels: &[u8], w: i32, h: i32) -> bool {
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let Ok(bitmap) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        return false;
    };
    if bits.is_null() {
        let _ = DeleteObject(bitmap.into());
        return false;
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
    let screen = GetDC(None);
    if screen.is_invalid() {
        let _ = DeleteObject(bitmap.into());
        return false;
    }
    let mem = CreateCompatibleDC(Some(screen));
    if mem.is_invalid() {
        let _ = ReleaseDC(None, screen);
        let _ = DeleteObject(bitmap.into());
        return false;
    }
    let old = SelectObject(mem, bitmap.into());
    let size = SIZE { cx: w, cy: h };
    let src_origin = POINT { x: 0, y: 0 };
    let blend = windows::Win32::Graphics::Gdi::BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let dst = std::ptr::from_ref(&origin);
    let sz = std::ptr::from_ref(&size);
    let src = std::ptr::from_ref(&src_origin);
    let blend_ptr = std::ptr::from_ref(&blend);
    let ok = UpdateLayeredWindow(
        hwnd,
        Some(screen),
        Some(dst),
        Some(sz),
        Some(mem),
        Some(src),
        COLORREF(0),
        Some(blend_ptr),
        ULW_ALPHA,
    )
    .is_ok();
    let _ = SelectObject(mem, old);
    let _ = DeleteObject(bitmap.into());
    let _ = DeleteDC(mem);
    let _ = ReleaseDC(None, screen);
    ok
}
