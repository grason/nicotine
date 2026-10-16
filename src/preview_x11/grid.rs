//! Compact-grid display mode: one always-on-top window, one live
//! thumbnail per logged-in client, in cycle order. Cell geometry lives
//! in `preview_common` so the panel caption and the Windows manager
//! share it. This file owns the XComposite sources and the XRender paint.

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Instant;

use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::damage::{ConnectionExt as _, ReportLevel};
use x11rb::protocol::render::{
    ConnectionExt as _, CreatePictureAux as RenderCreatePictureAux, PictOp,
};
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    Rectangle as XRectangle, SubwindowMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use crate::preview_common::{
    fit_contain, grid_layout, grid_thumb_bounds, preview_should_hide, GridCell, GRID_GAP_PX,
    GRID_NAME_FONT_PX, GRID_OUTER_BORDER_PX,
};

use super::render::{
    apply_scale_transform, create_text_pixmap, jetbrains_mono, pick_visual_format, xcolor,
};
use super::{
    opacity_to_cardinal, DragState, MutexExt as _, PreviewManager, CHROME_DARK, NICOTINE_CREAM,
    NICOTINE_GOLD, NICOTINE_RED, WINDOW_SCAN_BACKSTOP,
};

/// Plain copy of one cell's paint inputs.
struct PaintSnap {
    cell: GridCell,
    src_picture: u32,
    source_w: u16,
    source_h: u16,
    hide_thumb: bool,
    text_picture: u32,
    text_w: u16,
    text_h: u16,
    is_active: bool,
    taking_damage: bool,
    label_is_alert: bool,
}

/// One redirected EVE client inside the grid. Drop unredirects the
/// source and frees the XComposite / XDamage / text resources.
struct GridSource {
    conn: Arc<RustConnection>,
    character_name: String,
    source_id: u32,
    damage_id: u32,
    src_pixmap: u32,
    src_picture: u32,
    source_w: u16,
    source_h: u16,
    /// False until the next paint rebinds the source pixmap. Damage
    /// sets this so only the client that drew pays for a round-trip.
    fresh: bool,
    label: String,
    label_is_alert: bool,
    text_pixmap: u32,
    text_picture: u32,
    text_w: u16,
    text_h: u16,
    is_active: bool,
    taking_damage: bool,
    hide_thumb: bool,
}

impl Drop for GridSource {
    fn drop(&mut self) {
        let _ = self.conn.damage_destroy(self.damage_id);
        let _ = self.conn.render_free_picture(self.src_picture);
        let _ = self.conn.render_free_picture(self.text_picture);
        let _ = self.conn.free_pixmap(self.text_pixmap);
        let _ = self.conn.free_pixmap(self.src_pixmap);
        let _ = self
            .conn
            .composite_unredirect_window(self.source_id, Redirect::AUTOMATIC);
    }
}

/// What the last paint drew. Refresh skips the composite when this
/// still matches, so a quiet grid does not repaint at 10 Hz.
#[derive(PartialEq, Eq)]
struct GridChrome {
    entries: Vec<(String, String, bool, bool, bool, bool)>,
    columns: u32,
    cell_w: u32,
    width: u16,
    height: u16,
}

pub(super) struct OwnedGrid {
    conn: Arc<RustConnection>,
    pub(super) window: u32,
    dst_picture: u32,
    sources: Vec<GridSource>,
    cells: Vec<GridCell>,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) x: i16,
    pub(super) y: i16,
    pub(super) drag: DragState,
    pub(super) grabbed: bool,
    chrome: Option<GridChrome>,
}

impl Drop for OwnedGrid {
    fn drop(&mut self) {
        if self.grabbed {
            let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
        }
        // Sources unredirect on drop. Do that before the window goes
        // away so a failed unredirect still has a live connection use.
        self.sources.drain(..);
        let _ = self.conn.render_free_picture(self.dst_picture);
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

impl PreviewManager {
    pub(super) fn reconcile_grid(&mut self) -> Result<()> {
        if self.last_window_scan.elapsed() >= WINDOW_SCAN_BACKSTOP {
            self.needs_window_scan = true;
        }
        if self.needs_window_scan {
            let found = self.find_eve_windows()?;
            self.grid_ids.clear();
            for (id, title) in found {
                self.grid_ids.insert(title, id);
            }
            self.needs_window_scan = false;
            self.last_window_scan = Instant::now();
        }

        let ordered: Vec<String> = self
            .state
            .lock_recover()
            .get_ordered_windows()
            .into_iter()
            .map(|w| w.title)
            .collect();
        let desired: Vec<(u32, String)> = ordered
            .into_iter()
            .filter_map(|name| self.grid_ids.get(&name).copied().map(|id| (id, name)))
            .collect();

        if desired.is_empty() {
            self.grid_window = None;
            return Ok(());
        }
        if self.grid_window.is_none() {
            self.create_grid_window(desired.len())?;
        }
        self.sync_grid_sources(&desired)?;
        self.refresh_grid_chrome()?;
        Ok(())
    }

    /// Damage on a grid source: rebind that client's pixmap and repaint
    /// every cell. Other clients keep the pixmap they already hold.
    pub(super) fn paint_grid_damage(&mut self, damage: u32) -> Result<()> {
        let hit = self.grid_window.as_mut().is_some_and(|grid| {
            grid.sources
                .iter_mut()
                .find(|s| s.damage_id == damage)
                .is_some_and(|s| {
                    s.fresh = false;
                    true
                })
        });
        if hit {
            self.paint_grid()?;
        }
        Ok(())
    }

    pub(super) fn grid_has_damage(&self, damage: u32) -> bool {
        self.grid_window
            .as_ref()
            .is_some_and(|g| g.sources.iter().any(|s| s.damage_id == damage))
    }

    fn create_grid_window(&mut self, count: usize) -> Result<()> {
        let (columns, cell_w, opacity) = {
            let live = self.live.lock_recover();
            (
                live.grid_columns,
                live.grid_cell_width,
                live.preview_opacity,
            )
        };
        let layout = grid_layout(count, columns, cell_w, GRID_GAP_PX, GRID_OUTER_BORDER_PX);
        let width = layout.window_w.max(1) as u16;
        let height = layout.window_h.max(1) as u16;

        let (init_x, init_y) = self
            .positions
            .lock_recover()
            .grid_position
            .filter(|(x, y)| self.position_on_screen(*x, *y))
            .unwrap_or((64, 64));

        let conn = &self.conn;
        let screen = &conn.setup().roots[self.screen_num];
        let our_window = conn.generate_id()?;

        // Same override-redirect surface as the list window: no WM
        // decoration, no auto-clear (we paint the whole frame).
        let win_aux = CreateWindowAux::new()
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE,
            )
            .border_pixel(0)
            .override_redirect(1);
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            our_window,
            screen.root,
            init_x as i16,
            init_y as i16,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &win_aux,
        )?;
        conn.change_property32(
            PropMode::REPLACE,
            our_window,
            self.atoms.net_wm_state,
            AtomEnum::ATOM,
            &[self.atoms.net_wm_state_above],
        )?;
        conn.change_property32(
            PropMode::REPLACE,
            our_window,
            self.atoms.net_wm_window_type,
            AtomEnum::ATOM,
            &[self.atoms.net_wm_window_type_utility],
        )?;
        conn.change_property32(
            PropMode::REPLACE,
            our_window,
            self.atoms.net_wm_window_opacity,
            AtomEnum::CARDINAL,
            &[opacity_to_cardinal(opacity)],
        )?;
        let title = b"Nicotine";
        let wm_name_atom: u32 = AtomEnum::WM_NAME.into();
        let string_atom: u32 = AtomEnum::STRING.into();
        conn.change_property8(
            PropMode::REPLACE,
            our_window,
            wm_name_atom,
            string_atom,
            title,
        )?;
        conn.change_property8(
            PropMode::REPLACE,
            our_window,
            self.atoms.net_wm_name,
            self.atoms.utf8_string,
            title,
        )?;
        conn.map_window(our_window)?;

        let pic_aux = RenderCreatePictureAux::new().subwindowmode(SubwindowMode::INCLUDE_INFERIORS);
        let dst_picture = conn.generate_id()?;
        conn.render_create_picture(dst_picture, our_window, self.visual_format, &pic_aux)?;
        conn.sync()?;

        self.grid_window = Some(OwnedGrid {
            conn: Arc::clone(&self.conn),
            window: our_window,
            dst_picture,
            sources: Vec::new(),
            cells: layout.cells,
            width,
            height,
            x: init_x as i16,
            y: init_y as i16,
            drag: DragState::default(),
            grabbed: false,
            chrome: None,
        });
        Ok(())
    }

    fn sync_grid_sources(&mut self, desired: &[(u32, String)]) -> Result<()> {
        if let Some(grid) = self.grid_window.as_mut() {
            grid.sources.retain(|s| {
                desired
                    .iter()
                    .any(|(id, name)| name == &s.character_name && *id == s.source_id)
            });
        }
        let have: Vec<String> = self
            .grid_window
            .as_ref()
            .map(|g| g.sources.iter().map(|s| s.character_name.clone()).collect())
            .unwrap_or_default();
        for (id, name) in desired {
            if !have.iter().any(|n| n == name) {
                if let Err(e) = self.bind_grid_source(*id, name) {
                    eprintln!("Grid source '{name}' failed: {e}");
                }
            }
        }
        let order: Vec<String> = desired.iter().map(|(_, n)| n.clone()).collect();
        if let Some(grid) = self.grid_window.as_mut() {
            grid.sources.sort_by_key(|s| {
                order
                    .iter()
                    .position(|n| n == &s.character_name)
                    .unwrap_or(usize::MAX)
            });
        }
        Ok(())
    }

    fn bind_grid_source(&mut self, source_id: u32, title: &str) -> Result<()> {
        let window = self
            .grid_window
            .as_ref()
            .map(|g| g.window)
            .context("grid window missing")?;
        let conn = &self.conn;
        let geom = conn.get_geometry(source_id)?.reply()?;
        if geom.width == 0 || geom.height == 0 {
            anyhow::bail!("source window has zero geometry");
        }
        let src_attrs = conn.get_window_attributes(source_id)?.reply()?;
        conn.composite_redirect_window(source_id, Redirect::AUTOMATIC)?;
        let src_pixmap = conn.generate_id()?;
        if conn
            .composite_name_window_pixmap(source_id, src_pixmap)?
            .check()
            .is_err()
        {
            let _ = conn.composite_unredirect_window(source_id, Redirect::AUTOMATIC);
            anyhow::bail!("name_window_pixmap failed");
        }
        let damage_id = conn.generate_id()?;
        conn.damage_create(damage_id, source_id, ReportLevel::NON_EMPTY)?;

        let formats_reply = conn.render_query_pict_formats()?.reply()?;
        let src_format = pick_visual_format(&formats_reply, src_attrs.visual, geom.depth)
            .context("no PictFormat for grid source")?;
        let src_picture = conn.generate_id()?;
        let pic_aux = RenderCreatePictureAux::new().subwindowmode(SubwindowMode::INCLUDE_INFERIORS);
        conn.render_create_picture(src_picture, src_pixmap, src_format, &pic_aux)?;

        let (text_pixmap, text_picture, text_w, text_h) = create_text_pixmap(
            conn,
            window,
            title,
            NICOTINE_CREAM,
            jetbrains_mono(),
            GRID_NAME_FONT_PX,
            self.argb32_format,
        )?;

        let source = GridSource {
            conn: Arc::clone(&self.conn),
            character_name: title.to_string(),
            source_id,
            damage_id,
            src_pixmap,
            src_picture,
            source_w: geom.width,
            source_h: geom.height,
            fresh: false,
            label: title.to_string(),
            label_is_alert: false,
            text_pixmap,
            text_picture,
            text_w,
            text_h,
            is_active: false,
            taking_damage: false,
            hide_thumb: false,
        };
        if let Some(grid) = self.grid_window.as_mut() {
            grid.sources.push(source);
        }
        Ok(())
    }

    /// Resize, relabel, and repaint when the set of cells or their
    /// chrome changed. Cell size never depends on alerts.
    pub(super) fn refresh_grid_chrome(&mut self) -> Result<()> {
        let Some((count, window, cur_w, cur_h)) = self
            .grid_window
            .as_ref()
            .map(|g| (g.sources.len(), g.window, g.width, g.height))
        else {
            return Ok(());
        };
        if count == 0 {
            self.grid_window = None;
            return Ok(());
        }
        let (columns, cell_w, hide_active) = {
            let live = self.live.lock_recover();
            (
                live.grid_columns,
                live.grid_cell_width,
                live.hide_active_preview,
            )
        };
        let layout = grid_layout(count, columns, cell_w, GRID_GAP_PX, GRID_OUTER_BORDER_PX);
        let width = layout.window_w.max(1) as u16;
        let height = layout.window_h.max(1) as u16;
        let size_changed = cur_w != width || cur_h != height;
        if size_changed {
            let _ = self.conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .width(width as u32)
                    .height(height as u32),
            );
        }
        if let Some(grid) = self.grid_window.as_mut() {
            grid.width = width;
            grid.height = height;
            grid.cells = layout.cells;
        }

        let names: Vec<String> = self
            .grid_window
            .as_ref()
            .map(|g| g.sources.iter().map(|s| s.character_name.clone()).collect())
            .unwrap_or_default();
        let now = Instant::now();
        let enabled = self.logs_config.lock_recover().enabled;
        for name in &names {
            let (is_active, alert, taking) = {
                let is_active = self
                    .active_character
                    .as_ref()
                    .is_some_and(|active| active == name)
                    || self.grid_window.as_ref().is_some_and(|g| {
                        g.sources
                            .iter()
                            .any(|s| s.character_name == *name && s.source_id == self.active_id)
                    });
                let (alert, taking) = if enabled && !is_active {
                    let live = self.log_live.lock_recover();
                    (
                        live.alert_for(name, now).map(|a| a.text.clone()),
                        live.taking_damage(name, now),
                    )
                } else if enabled {
                    let live = self.log_live.lock_recover();
                    (None, live.taking_damage(name, now))
                } else {
                    (None, false)
                };
                (is_active, alert, taking)
            };
            let label_is_alert = alert.is_some();
            let label = alert.unwrap_or_else(|| name.clone());
            let hide_thumb = preview_should_hide(hide_active, is_active);
            let needs_text = self.grid_window.as_ref().is_some_and(|g| {
                g.sources.iter().any(|s| {
                    s.character_name == *name
                        && (s.label != label || s.label_is_alert != label_is_alert)
                })
            });
            if needs_text {
                self.rebuild_grid_label(name, &label, label_is_alert)?;
            }
            if let Some(grid) = self.grid_window.as_mut() {
                if let Some(src) = grid.sources.iter_mut().find(|s| s.character_name == *name) {
                    src.is_active = is_active;
                    src.taking_damage = taking;
                    src.hide_thumb = hide_thumb;
                }
            }
        }

        let chrome = self.grid_chrome(columns, cell_w);
        let changed = self
            .grid_window
            .as_ref()
            .is_some_and(|g| g.chrome.as_ref() != chrome.as_ref());
        if changed || size_changed {
            self.paint_grid()?;
            if let Some(grid) = self.grid_window.as_mut() {
                grid.chrome = chrome;
            }
        }
        Ok(())
    }

    fn grid_chrome(&self, columns: u32, cell_w: u32) -> Option<GridChrome> {
        let grid = self.grid_window.as_ref()?;
        Some(GridChrome {
            entries: grid
                .sources
                .iter()
                .map(|s| {
                    (
                        s.character_name.clone(),
                        s.label.clone(),
                        s.is_active,
                        s.label_is_alert,
                        s.taking_damage,
                        s.hide_thumb,
                    )
                })
                .collect(),
            columns,
            cell_w,
            width: grid.width,
            height: grid.height,
        })
    }

    fn rebuild_grid_label(&mut self, name: &str, label: &str, is_alert: bool) -> Result<()> {
        let window = match self.grid_window.as_ref().map(|g| g.window) {
            Some(w) => w,
            None => return Ok(()),
        };
        let color = if is_alert {
            NICOTINE_GOLD
        } else {
            NICOTINE_CREAM
        };
        let (pixmap, picture, w, h) = create_text_pixmap(
            &self.conn,
            window,
            label,
            color,
            jetbrains_mono(),
            GRID_NAME_FONT_PX,
            self.argb32_format,
        )?;
        let old = self.grid_window.as_ref().and_then(|g| {
            g.sources
                .iter()
                .find(|s| s.character_name == name)
                .map(|s| (s.text_picture, s.text_pixmap))
        });
        let Some((old_picture, old_pixmap)) = old else {
            let _ = self.conn.render_free_picture(picture);
            let _ = self.conn.free_pixmap(pixmap);
            return Ok(());
        };
        let _ = self.conn.render_free_picture(old_picture);
        let _ = self.conn.free_pixmap(old_pixmap);
        if let Some(src) = self
            .grid_window
            .as_mut()
            .and_then(|g| g.sources.iter_mut().find(|s| s.character_name == name))
        {
            src.text_pixmap = pixmap;
            src.text_picture = picture;
            src.text_w = w;
            src.text_h = h;
            src.label = label.to_string();
            src.label_is_alert = is_alert;
        }
        Ok(())
    }

    pub(super) fn paint_grid(&mut self) -> Result<()> {
        let stale: Vec<usize> = {
            let Some(grid) = self.grid_window.as_ref() else {
                return Ok(());
            };
            if grid.sources.is_empty() || grid.cells.len() != grid.sources.len() {
                return Ok(());
            }
            grid.sources
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.fresh)
                .map(|(i, _)| i)
                .collect()
        };
        for index in stale {
            self.refresh_grid_pixmap(index)?;
        }

        let (columns, cell_w) = {
            let live = self.live.lock_recover();
            (live.grid_columns, live.grid_cell_width)
        };
        // Snapshot everything the draw needs so the X connection borrow
        // doesn't overlap the grid borrow.
        let Some(snap) = self.grid_window.as_ref().map(|grid| {
            let cells: Vec<PaintSnap> = grid
                .sources
                .iter()
                .zip(grid.cells.iter())
                .map(|(src, cell)| PaintSnap {
                    cell: *cell,
                    src_picture: src.src_picture,
                    source_w: src.source_w,
                    source_h: src.source_h,
                    hide_thumb: src.hide_thumb,
                    text_picture: src.text_picture,
                    text_w: src.text_w,
                    text_h: src.text_h,
                    is_active: src.is_active,
                    taking_damage: src.taking_damage,
                    label_is_alert: src.label_is_alert,
                })
                .collect();
            (grid.dst_picture, grid.width, grid.height, cells)
        }) else {
            return Ok(());
        };
        let (dst, width, height, cells) = snap;
        let layout = grid_layout(
            cells.len(),
            columns,
            cell_w,
            GRID_GAP_PX,
            GRID_OUTER_BORDER_PX,
        );
        let gold = xcolor(NICOTINE_GOLD, 0xffff);
        let dark = xcolor(CHROME_DARK, 0xffff);
        let red = xcolor(NICOTINE_RED, 0xffff);

        let outer = XRectangle {
            x: 0,
            y: 0,
            width,
            height,
        };
        self.conn
            .render_fill_rectangles(PictOp::SRC, dst, gold, &[outer])?;
        let inner = XRectangle {
            x: layout.outer_border as i16,
            y: layout.outer_border as i16,
            width: width.saturating_sub(layout.outer_border as u16 * 2),
            height: height.saturating_sub(layout.outer_border as u16 * 2),
        };
        self.conn
            .render_fill_rectangles(PictOp::SRC, dst, dark, &[inner])?;

        for src in &cells {
            let cell = src.cell;
            let bounds = grid_thumb_bounds(cell, layout.cell_inset, layout.scrim);
            if !src.hide_thumb
                && src.source_w > 0
                && src.source_h > 0
                && bounds.w > 0
                && bounds.h > 0
            {
                let fitted = fit_contain(src.source_w as i32, src.source_h as i32, bounds);
                apply_scale_transform(
                    &self.conn,
                    src.src_picture,
                    src.source_w,
                    src.source_h,
                    fitted.w.max(1) as u16,
                    fitted.h.max(1) as u16,
                )?;
                self.conn.render_composite(
                    PictOp::SRC,
                    src.src_picture,
                    x11rb::NONE,
                    dst,
                    0,
                    0,
                    0,
                    0,
                    fitted.x as i16,
                    fitted.y as i16,
                    fitted.w.max(1) as u16,
                    fitted.h.max(1) as u16,
                )?;
            }

            let scrim_y = cell.y + cell.h - layout.scrim;
            let scrim = XRectangle {
                x: cell.x as i16,
                y: scrim_y as i16,
                width: cell.w.max(0) as u16,
                height: layout.scrim.max(0) as u16,
            };
            self.conn
                .render_fill_rectangles(PictOp::SRC, dst, dark, &[scrim])?;

            let max_w = (cell.w - 4).max(0) as u16;
            let draw_w = src.text_w.min(max_w);
            let tx = if src.text_w > max_w {
                cell.x + 2
            } else {
                cell.x + (cell.w - draw_w as i32) / 2
            };
            let ty = scrim_y + (layout.scrim - src.text_h as i32) / 2;
            if draw_w > 0 && src.text_h > 0 {
                self.conn.render_composite(
                    PictOp::OVER,
                    src.text_picture,
                    x11rb::NONE,
                    dst,
                    0,
                    0,
                    0,
                    0,
                    tx as i16,
                    ty.max(scrim_y) as i16,
                    draw_w,
                    src.text_h.min(layout.scrim.max(0) as u16),
                )?;
            }

            let frame = if src.is_active || src.taking_damage {
                Some(red)
            } else if src.label_is_alert {
                Some(gold)
            } else {
                None
            };
            if let Some(color) = frame {
                let inset = layout.cell_inset.max(1);
                let rects = [
                    XRectangle {
                        x: cell.x as i16,
                        y: cell.y as i16,
                        width: cell.w.max(0) as u16,
                        height: inset as u16,
                    },
                    XRectangle {
                        x: cell.x as i16,
                        y: (cell.y + cell.h - inset) as i16,
                        width: cell.w.max(0) as u16,
                        height: inset as u16,
                    },
                    XRectangle {
                        x: cell.x as i16,
                        y: cell.y as i16,
                        width: inset as u16,
                        height: cell.h.max(0) as u16,
                    },
                    XRectangle {
                        x: (cell.x + cell.w - inset) as i16,
                        y: cell.y as i16,
                        width: inset as u16,
                        height: cell.h.max(0) as u16,
                    },
                ];
                self.conn
                    .render_fill_rectangles(PictOp::SRC, dst, color, &rects)?;
            }
        }
        Ok(())
    }

    /// Rebind one source pixmap. Same reason as the per-window preview:
    /// DXVK presents into a fresh buffer, and the pixmap from redirect
    /// time keeps the old one.
    fn refresh_grid_pixmap(&mut self, index: usize) -> Result<()> {
        let Some((source_id, old_picture, old_pixmap)) = self.grid_window.as_ref().and_then(|g| {
            g.sources
                .get(index)
                .map(|s| (s.source_id, s.src_picture, s.src_pixmap))
        }) else {
            return Ok(());
        };

        let new_pixmap = self.conn.generate_id()?;
        if self
            .conn
            .composite_name_window_pixmap(source_id, new_pixmap)?
            .check()
            .is_err()
        {
            self.needs_window_scan = true;
            return Ok(());
        }
        let new_picture = self.conn.generate_id()?;
        let pic_aux = RenderCreatePictureAux::new().subwindowmode(SubwindowMode::INCLUDE_INFERIORS);
        self.conn
            .render_create_picture(new_picture, new_pixmap, self.visual_format, &pic_aux)?;
        let _ = self.conn.render_free_picture(old_picture);
        let _ = self.conn.free_pixmap(old_pixmap);

        let geom = self
            .conn
            .get_geometry(source_id)
            .ok()
            .and_then(|c| c.reply().ok());
        if let Some(src) = self
            .grid_window
            .as_mut()
            .and_then(|g| g.sources.get_mut(index))
        {
            if let Some(geom) = geom {
                if geom.width > 0 && geom.height > 0 {
                    src.source_w = geom.width;
                    src.source_h = geom.height;
                }
            }
            src.src_pixmap = new_pixmap;
            src.src_picture = new_picture;
            src.fresh = true;
        }
        Ok(())
    }

    /// Click-to-activate for a release that was not a drag. `x`/`y` are
    /// window-local. A miss (frame or gap) does nothing.
    pub(super) fn activate_grid_cell(&self, x: i32, y: i32) -> Result<()> {
        let Some(grid) = self.grid_window.as_ref() else {
            return Ok(());
        };
        let Some(index) = crate::preview_common::grid_cell_at(&grid.cells, x, y) else {
            return Ok(());
        };
        let Some(src) = grid.sources.get(index) else {
            return Ok(());
        };
        self.activate_source(src.source_id)
    }
}
