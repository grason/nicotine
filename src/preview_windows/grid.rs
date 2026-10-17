//! Compact-grid display mode on Windows: one layered topmost window and
//! one DWM thumbnail per logged-in client. Cell geometry is the shared
//! `grid_layout_scaled` helper. The frame is GDI; names sit on a
//! click-through layered popup because DWM paints above GDI.

use anyhow::{Context, Result};

use crate::preview_common::{
    fit_contain, grid_cell_at, grid_layout_scaled, grid_thumb_bounds, preview_should_hide,
    GridLayout, GridRect, GRID_GAP_PX, GRID_OUTER_BORDER_PX,
};
use crate::preview_positions::PreviewPositions;
use crate::windows_manager::id_to_hwnd;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, SetWindowLongPtrW, SetWindowPos, GWLP_USERDATA, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use super::{
    apply_window_opacity, dpi_scale, opacity_to_byte, position_on_screen, positions_locked,
    unpack_xy, DragState, Hthumbnail, PreviewManager, CHROME_DARK, GRID_CLASS, MANAGER_PTR,
    NICOTINE_GOLD, NICOTINE_RED,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;

struct GridThumb {
    name: String,
    source_id: u32,
    thumbnail: Hthumbnail,
}

pub(super) struct OwnedGrid {
    pub(super) hwnd: HWND,
    overlay: super::overlay::TextOverlay,
    thumbs: Vec<GridThumb>,
}

impl Drop for OwnedGrid {
    fn drop(&mut self) {
        self.overlay.destroy();
        unsafe {
            for thumb in self.thumbs.drain(..) {
                let _ = DwmUnregisterThumbnail(thumb.thumbnail);
            }
            let ptr = GetWindowLongPtrW(self.hwnd, GWLP_USERDATA);
            if ptr != 0 {
                drop(Box::from_raw(ptr as *mut GridWindowState));
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct GridWindowState {
    drag: DragState,
    positions: Arc<Mutex<PreviewPositions>>,
    /// Label-layer HWND so a drag can move it without borrowing the manager.
    overlay: HWND,
}

impl PreviewManager {
    pub(super) fn reconcile_grid(&mut self) {
        let windows = {
            let state = self.state.lock().unwrap();
            state.get_ordered_windows()
        };
        if windows.is_empty() {
            self.grid = None;
            self.grid_key.clear();
            return;
        }
        if self.grid.is_none() {
            if let Err(e) = self.create_grid_window() {
                eprintln!("Failed to create grid window: {e}");
                return;
            }
        }
        self.sync_grid_thumbs(&windows);
        self.layout_grid_thumbs(&windows);
        self.invalidate_grid_if_changed(&windows);

        if let Some(grid) = &self.grid {
            unsafe {
                let _ = SetWindowPos(
                    grid.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    fn create_grid_window(&mut self) -> Result<()> {
        let module = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW failed")?;
        let class_name: Vec<u16> = GRID_CLASS.encode_utf16().collect();
        let title: Vec<u16> = "Nicotine\0".encode_utf16().collect();
        let (columns, cell_w, opacity) = {
            let live = self.live.lock().unwrap();
            (
                live.grid_columns,
                live.grid_cell_width,
                live.preview_opacity,
            )
        };
        let count = self
            .state
            .lock()
            .unwrap()
            .get_ordered_windows()
            .len()
            .max(1);
        let layout = grid_layout(count, columns, cell_w);
        let (x, y) = self
            .positions
            .lock()
            .unwrap()
            .grid_position
            .filter(|(x, y)| position_on_screen(*x, *y))
            .unwrap_or((super::px(64), super::px(64)));

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                layout.window_w,
                layout.window_h.max(1),
                None,
                None,
                Some(HINSTANCE(module.0)),
                None,
            )
        }
        .context("CreateWindowExW failed for grid window")?;

        apply_window_opacity(hwnd, opacity);
        let overlay = super::overlay::TextOverlay::create(hwnd);
        let state = Box::new(GridWindowState {
            drag: DragState::default(),
            positions: Arc::clone(&self.positions),
            overlay: overlay.hwnd(),
        });
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        self.grid = Some(OwnedGrid {
            hwnd,
            overlay,
            thumbs: Vec::new(),
        });
        Ok(())
    }

    fn sync_grid_thumbs(&mut self, windows: &[crate::window_manager::EveWindow]) {
        let Some(hwnd) = self.grid.as_ref().map(|g| g.hwnd) else {
            return;
        };
        if let Some(grid) = self.grid.as_mut() {
            let wanted = windows;
            grid.thumbs.retain(|thumb| {
                let keep = wanted
                    .iter()
                    .any(|w| w.title == thumb.name && w.id == thumb.source_id);
                if !keep {
                    unsafe {
                        let _ = DwmUnregisterThumbnail(thumb.thumbnail);
                    }
                }
                keep
            });
        }
        let have: Vec<String> = self
            .grid
            .as_ref()
            .map(|g| g.thumbs.iter().map(|t| t.name.clone()).collect())
            .unwrap_or_default();
        for window in windows {
            if have.iter().any(|n| n == &window.title) {
                continue;
            }
            let thumbnail = unsafe { DwmRegisterThumbnail(hwnd, id_to_hwnd(window.id)) };
            let Ok(thumbnail) = thumbnail else {
                eprintln!("DwmRegisterThumbnail failed for {}", window.title);
                continue;
            };
            if let Some(grid) = self.grid.as_mut() {
                grid.thumbs.push(GridThumb {
                    name: window.title.clone(),
                    source_id: window.id,
                    thumbnail,
                });
            }
        }
        let order: Vec<String> = windows.iter().map(|w| w.title.clone()).collect();
        if let Some(grid) = self.grid.as_mut() {
            grid.thumbs.sort_by_key(|t| {
                order
                    .iter()
                    .position(|n| n == &t.name)
                    .unwrap_or(usize::MAX)
            });
        }
    }

    /// Size the window from the shared layout and point each thumbnail
    /// at its cell. The dest is letterboxed inside the frame. The name
    /// is a layered popup on top of the mirror, because DWM paints
    /// above GDI.
    fn layout_grid_thumbs(&mut self, windows: &[crate::window_manager::EveWindow]) {
        let Some(hwnd) = self.grid.as_ref().map(|g| g.hwnd) else {
            return;
        };
        let (columns, cell_w, hide_active) = {
            let live = self.live.lock().unwrap();
            (
                live.grid_columns,
                live.grid_cell_width,
                live.hide_active_preview,
            )
        };
        let layout = grid_layout(windows.len(), columns, cell_w);
        unsafe {
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let cur_w = rect.right - rect.left;
            let cur_h = rect.bottom - rect.top;
            if cur_w != layout.window_w || cur_h != layout.window_h {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    layout.window_w,
                    layout.window_h.max(1),
                    SWP_NOMOVE | SWP_NOACTIVATE,
                );
            }
        }
        let Some(grid) = self.grid.as_ref() else {
            return;
        };
        for (index, thumb) in grid.thumbs.iter().enumerate() {
            let Some(cell) = layout.cells.get(index) else {
                continue;
            };
            let bounds = grid_thumb_bounds(*cell, layout.cell_inset);
            let (src_w, src_h) = unsafe {
                let mut src_rect = RECT::default();
                if GetWindowRect(id_to_hwnd(thumb.source_id), &mut src_rect).is_ok() {
                    (
                        src_rect.right - src_rect.left,
                        src_rect.bottom - src_rect.top,
                    )
                } else {
                    (0, 0)
                }
            };
            // Zero size fills the content box. A live client is letterboxed.
            let fitted = fit_contain(src_w, src_h, bounds);
            let is_active = thumb.source_id == self.active_id;
            let visible = !preview_should_hide(hide_active, is_active);
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY,
                rcDestination: RECT {
                    left: fitted.x,
                    top: fitted.y,
                    right: fitted.x + fitted.w,
                    bottom: fitted.y + fitted.h,
                },
                rcSource: RECT::default(),
                opacity: 255,
                fVisible: visible.into(),
                fSourceClientAreaOnly: false.into(),
            };
            unsafe {
                let _ = DwmUpdateThumbnailProperties(thumb.thumbnail, &props);
            }
        }
    }

    pub(super) fn invalidate_grid_if_changed(
        &mut self,
        windows: &[crate::window_manager::EveWindow],
    ) {
        let key = self.grid_paint_key(windows);
        if key == self.grid_key {
            return;
        }
        self.grid_key = key;
        if let Some(grid) = &self.grid {
            unsafe {
                let _ = InvalidateRect(Some(grid.hwnd), None, false);
            }
        }
    }

    fn grid_paint_key(&self, windows: &[crate::window_manager::EveWindow]) -> String {
        let (columns, cell_w, hide_active, enabled) = {
            let live = self.live.lock().unwrap();
            let enabled = self.logs_config.lock().unwrap().enabled;
            (
                live.grid_columns,
                live.grid_cell_width,
                live.hide_active_preview,
                enabled,
            )
        };
        let now = std::time::Instant::now();
        let live = self.log_live.lock().unwrap();
        let mut key = format!("{columns}:{cell_w}:{hide_active}:{}", self.active_id);
        for window in windows {
            let alert = if enabled {
                live.alert_for(&window.title, now)
                    .map(|a| a.text.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let taking = enabled && live.taking_damage(&window.title, now);
            key.push_str(&format!(
                "|{}:{alert}:{taking}:{}",
                window.title,
                window.id == self.active_id
            ));
        }
        key
    }

    /// Redraw the label popup when names, alerts, size, or opacity change,
    /// and re-pin it to the grid window every tick. Owned popups do not
    /// follow the owner.
    pub(super) fn refresh_grid_overlay(&mut self) {
        let Some(hwnd) = self.grid.as_ref().map(|g| g.hwnd) else {
            return;
        };
        let opacity = self.config.preview_opacity;
        let byte = opacity_to_byte(opacity);
        let windows = self.state.lock().unwrap().get_ordered_windows();
        let paint_key = self.grid_paint_key(&windows);
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let key = format!("{paint_key}|{opacity}|{w}|{h}");
        let fresh = self
            .grid
            .as_ref()
            .is_some_and(|grid| grid.overlay.matches(&key));
        if fresh {
            if let Some(grid) = self.grid.as_mut() {
                grid.overlay.move_to(hwnd);
                grid.overlay.set_visible(true);
            }
            return;
        }

        let (columns, cell_w, enabled) = {
            let live = self.live.lock().unwrap();
            let enabled = self.logs_config.lock().unwrap().enabled;
            (live.grid_columns, live.grid_cell_width, enabled)
        };
        let layout = grid_layout(windows.len(), columns, cell_w);
        let now = std::time::Instant::now();
        let labels = {
            let log = self.log_live.lock().unwrap();
            let mut labels = Vec::with_capacity(windows.len());
            for (index, window) in windows.iter().enumerate() {
                let Some(cell) = layout.cells.get(index) else {
                    continue;
                };
                let is_active = window.id == self.active_id;
                let alert = if enabled && !is_active {
                    log.alert_for(&window.title, now).map(|a| a.text.clone())
                } else {
                    None
                };
                let is_alert = alert.is_some();
                labels.push(super::overlay::GridLabel {
                    cell: GridRect {
                        x: cell.x,
                        y: cell.y,
                        w: cell.w,
                        h: cell.h,
                    },
                    text: alert.unwrap_or_else(|| window.title.clone()),
                    alert: is_alert,
                });
            }
            labels
        };
        let pixels = super::overlay::grid_pixels(
            w,
            h,
            &labels,
            layout.cell_inset,
            layout.outer_border,
            layout.scrim,
            byte,
        );
        if let Some(grid) = self.grid.as_mut() {
            grid.overlay.present(hwnd, &pixels, w, h, key);
            grid.overlay.set_visible(true);
        }
    }
}

fn grid_layout(count: usize, columns: u32, cell_w: u32) -> GridLayout {
    grid_layout_scaled(
        count,
        columns,
        cell_w,
        GRID_GAP_PX,
        GRID_OUTER_BORDER_PX,
        dpi_scale(),
    )
}

pub(super) unsafe extern "system" fn grid_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GridWindowState;
    let state = if state_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    } else {
        &mut *state_ptr
    };

    match msg {
        WM_PAINT => {
            paint_grid(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (cx, cy) = unpack_xy(lparam);
            let mut pt = POINT { x: cx, y: cy };
            let _ = ClientToScreen(hwnd, &mut pt);
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            state.drag.active = true;
            state.drag.dragged = false;
            state.drag.origin_screen = (pt.x, pt.y);
            state.drag.origin_window = (rect.left, rect.top);
            let _ = SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if state.drag.active && !positions_locked() {
                let (cx, cy) = unpack_xy(lparam);
                let mut pt = POINT { x: cx, y: cy };
                let _ = ClientToScreen(hwnd, &mut pt);
                let dx = pt.x - state.drag.origin_screen.0;
                let dy = pt.y - state.drag.origin_screen.1;
                let threshold = super::px(crate::preview_common::DRAG_THRESHOLD_PX);
                if dx.abs() > threshold || dy.abs() > threshold {
                    state.drag.dragged = true;
                }
                if state.drag.dragged {
                    let new_x = state.drag.origin_window.0 + dx;
                    let new_y = state.drag.origin_window.1 + dy;
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        new_x,
                        new_y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    super::overlay::move_overlay_to(state.overlay, new_x, new_y);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if state.drag.active {
                state.drag.active = false;
                let _ = ReleaseCapture();
                if state.drag.dragged {
                    let mut rect = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut rect);
                    let mut positions = state.positions.lock().unwrap();
                    positions.grid_position = Some((rect.left, rect.top));
                    positions.save();
                } else {
                    let (x, y) = unpack_xy(lparam);
                    activate_grid_cell(x, y);
                }
                state.drag.dragged = false;
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn activate_grid_cell(x: i32, y: i32) {
    let ptr = MANAGER_PTR.load(Ordering::Acquire) as *const PreviewManager;
    if ptr.is_null() {
        return;
    }
    let mgr = unsafe { &*ptr };
    let windows = mgr.state.lock().unwrap().get_ordered_windows();
    let (columns, cell_w) = {
        let live = mgr.live.lock().unwrap();
        (live.grid_columns, live.grid_cell_width)
    };
    let layout = grid_layout(windows.len(), columns, cell_w);
    let Some(index) = grid_cell_at(&layout.cells, x, y) else {
        return;
    };
    let Some(window) = windows.get(index) else {
        return;
    };
    let _ = mgr.wm.activate_window(window.id);
}

unsafe fn paint_grid(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    let ptr = MANAGER_PTR.load(Ordering::Acquire) as *const PreviewManager;
    if ptr.is_null() {
        let _ = EndPaint(hwnd, &ps);
        return;
    }
    let mgr = &*ptr;
    let windows = mgr.state.lock().unwrap().get_ordered_windows();
    let (columns, cell_w, hide_active, enabled) = {
        let live = mgr.live.lock().unwrap();
        let enabled = mgr.logs_config.lock().unwrap().enabled;
        (
            live.grid_columns,
            live.grid_cell_width,
            live.hide_active_preview,
            enabled,
        )
    };
    let layout = grid_layout(windows.len(), columns, cell_w);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let width = client.right.max(layout.window_w);
    let height = client.bottom.max(layout.window_h);

    let gold_brush = CreateSolidBrush(NICOTINE_GOLD);
    let dark_brush = CreateSolidBrush(CHROME_DARK);
    let red_brush = CreateSolidBrush(NICOTINE_RED);
    FillRect(
        hdc,
        &RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        gold_brush,
    );
    let border = layout.outer_border;
    FillRect(
        hdc,
        &RECT {
            left: border,
            top: border,
            right: width - border,
            bottom: height - border,
        },
        dark_brush,
    );

    let now = std::time::Instant::now();
    let log = mgr.log_live.lock().unwrap();

    for (index, window) in windows.iter().enumerate() {
        let Some(cell) = layout.cells.get(index) else {
            continue;
        };
        let is_active = window.id == mgr.active_id;
        let alert = enabled && !is_active && log.alert_for(&window.title, now).is_some();
        let taking = enabled && log.taking_damage(&window.title, now);
        let hide = preview_should_hide(hide_active, is_active);

        if hide {
            FillRect(
                hdc,
                &RECT {
                    left: cell.x,
                    top: cell.y,
                    right: cell.x + cell.w,
                    bottom: cell.y + cell.h,
                },
                dark_brush,
            );
        }

        let frame = if is_active || taking {
            Some(red_brush)
        } else if alert {
            Some(gold_brush)
        } else {
            None
        };
        if let Some(brush) = frame {
            let inset = layout.cell_inset.max(1);
            let rects = [
                RECT {
                    left: cell.x,
                    top: cell.y,
                    right: cell.x + cell.w,
                    bottom: cell.y + inset,
                },
                RECT {
                    left: cell.x,
                    top: cell.y + cell.h - inset,
                    right: cell.x + cell.w,
                    bottom: cell.y + cell.h,
                },
                RECT {
                    left: cell.x,
                    top: cell.y,
                    right: cell.x + inset,
                    bottom: cell.y + cell.h,
                },
                RECT {
                    left: cell.x + cell.w - inset,
                    top: cell.y,
                    right: cell.x + cell.w,
                    bottom: cell.y + cell.h,
                },
            ];
            for rect in &rects {
                FillRect(hdc, rect, brush);
            }
        }
    }

    let _ = DeleteObject(gold_brush.into());
    let _ = DeleteObject(dark_brush.into());
    let _ = DeleteObject(red_brush.into());
    let _ = EndPaint(hwnd, &ps);
}
