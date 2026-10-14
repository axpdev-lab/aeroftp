// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Tray badge icon generation and management
// Generates tray icons with colored dot badges + overlay icons for AeroCloud sync state

use image::Rgba;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, Ordering};
use tauri::image::Image;
use tauri::AppHandle;
use tracing::{error, info, warn};

/// Tray badge states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrayBadgeState {
    Default = 0, // No badge: idle/synced (Dropbox-style: no badge = all good)
    Syncing = 1, // Blue dot + white sync arrows (sync in progress)
    Error = 2,   // Red dot + white X (error occurred)
    Paused = 3,  // Grey dot + white pause bars (user paused sync)
}

impl TrayBadgeState {
    /// The badge AeroCloud starts with. A configuration left paused starts
    /// paused: no worker runs, and the tray says so instead of looking idle.
    pub fn at_startup(cloud_enabled: bool, cloud_paused: bool) -> Self {
        if cloud_enabled && cloud_paused {
            Self::Paused
        } else {
            Self::Default
        }
    }

    /// Get badge color (RGBA)
    fn badge_color(&self) -> Option<[u8; 4]> {
        match self {
            Self::Default => None,
            Self::Syncing => Some([33, 150, 243, 255]), // #2196F3 - Material Blue 500
            Self::Error => Some([244, 67, 54, 255]),    // #F44336 - Material Red 500
            Self::Paused => Some([158, 158, 158, 255]), // #9E9E9E - Material Grey 500
        }
    }

    /// Get tray tooltip text
    fn tooltip(&self) -> &'static str {
        match self {
            Self::Default => "AeroFTP",
            Self::Syncing => "AeroSync: Syncing...",
            Self::Error => "AeroSync: Sync Error",
            Self::Paused => "AeroSync: Paused",
        }
    }
}

// Current tray state (atomic for lock-free reads)
static CURRENT_TRAY_STATE: AtomicU8 = AtomicU8::new(0);

// Base icon bytes: white monochrome (standard for system tray)
const BASE_ICON_BYTES: &[u8] = include_bytes!("../../icons/AeroFTP_simbol_white_120x120.png");

/// Draw a thick line segment between two points using distance-based rasterization.
/// Pixels within `half_w` distance of the line segment get colored.
fn draw_thick_line(
    rgba: &mut image::RgbaImage,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    half_w: f32,
    color: Rgba<u8>,
) {
    let (w, h) = (rgba.width() as f32, rgba.height() as f32);
    let min_px = (x0.min(x1) - half_w - 1.0).max(0.0) as u32;
    let max_px = (x0.max(x1) + half_w + 1.0).min(w - 1.0) as u32;
    let min_py = (y0.min(y1) - half_w - 1.0).max(0.0) as u32;
    let max_py = (y0.max(y1) + half_w + 1.0).min(h - 1.0) as u32;

    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 0.001 {
        return;
    }

    for py in min_py..=max_py {
        for px in min_px..=max_px {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            let t = ((fx - x0) * dx + (fy - y0) * dy) / len_sq;
            let t = t.clamp(0.0, 1.0);
            let near_x = x0 + t * dx;
            let near_y = y0 + t * dy;
            let dist_sq = (fx - near_x).powi(2) + (fy - near_y).powi(2);
            if dist_sq <= half_w * half_w {
                rgba.put_pixel(px, py, color);
            }
        }
    }
}

/// Draw white pause bars (‖) inside the badge: two vertical bars
fn draw_badge_pause(rgba: &mut image::RgbaImage, cx: f32, cy: f32, r: f32) {
    let white = Rgba([255, 255, 255, 255]);
    let hw = (r * 0.14).max(1.0);
    let bar_half_h = r * 0.35;
    let bar_spacing = r * 0.22;

    // Left bar
    draw_thick_line(
        rgba,
        cx - bar_spacing,
        cy - bar_half_h,
        cx - bar_spacing,
        cy + bar_half_h,
        hw,
        white,
    );
    // Right bar
    draw_thick_line(
        rgba,
        cx + bar_spacing,
        cy - bar_half_h,
        cx + bar_spacing,
        cy + bar_half_h,
        hw,
        white,
    );
}

/// Draw white sync arrows (↻) inside the badge: two curved arrows forming a cycle
fn draw_badge_sync(rgba: &mut image::RgbaImage, cx: f32, cy: f32, r: f32) {
    let white = Rgba([255, 255, 255, 255]);
    let arc_r = r * 0.50;
    let hw = (r * 0.14).max(1.0);
    let steps: u32 = 20;

    // Two arcs (each ~140°) with 40° gaps, forming a clockwise rotation symbol.
    // Image coords: 0°=right, 90°=down, 180°=left, 270°=up
    // Arc 1: right side (320° → 460°/100°), arrowhead at 100°
    // Arc 2: left side (140° → 280°), arrowhead at 280°
    let arc_ranges: [(f32, f32); 2] = [(320.0, 460.0), (140.0, 280.0)];

    for &(start_deg, end_deg) in &arc_ranges {
        // Draw arc as chain of small line segments
        for i in 0..steps {
            let f0 = i as f32 / steps as f32;
            let f1 = (i + 1) as f32 / steps as f32;
            let a0 = (start_deg + (end_deg - start_deg) * f0).to_radians();
            let a1 = (start_deg + (end_deg - start_deg) * f1).to_radians();

            draw_thick_line(
                rgba,
                cx + arc_r * a0.cos(),
                cy + arc_r * a0.sin(),
                cx + arc_r * a1.cos(),
                cy + arc_r * a1.sin(),
                hw,
                white,
            );
        }

        // Arrowhead at the end of the arc
        let end_rad = end_deg.to_radians();
        let tip_x = cx + arc_r * end_rad.cos();
        let tip_y = cy + arc_r * end_rad.sin();

        // Tangent (clockwise direction at angle θ): (-sin(θ), cos(θ))
        let tang_x = -end_rad.sin();
        let tang_y = end_rad.cos();
        // Outward (radial, away from center): (cos(θ), sin(θ))
        let out_x = end_rad.cos();
        let out_y = end_rad.sin();

        let arrow_len = r * 0.28;

        // Prong 1: backward + outward spread
        draw_thick_line(
            rgba,
            tip_x,
            tip_y,
            tip_x - arrow_len * (tang_x + 0.5 * out_x),
            tip_y - arrow_len * (tang_y + 0.5 * out_y),
            hw,
            white,
        );
        // Prong 2: backward + inward spread
        draw_thick_line(
            rgba,
            tip_x,
            tip_y,
            tip_x - arrow_len * (tang_x - 0.5 * out_x),
            tip_y - arrow_len * (tang_y - 0.5 * out_y),
            hw,
            white,
        );
    }
}

/// Draw a white X mark inside the badge
fn draw_badge_x_mark(rgba: &mut image::RgbaImage, cx: f32, cy: f32, r: f32) {
    let white = Rgba([255, 255, 255, 255]);
    let hw = (r * 0.18).max(1.3);
    let arm = r * 0.32;

    // Two diagonal lines crossing at center
    draw_thick_line(rgba, cx - arm, cy - arm, cx + arm, cy + arm, hw, white);
    draw_thick_line(rgba, cx + arm, cy - arm, cx - arm, cy + arm, hw, white);
}

/// Generate a tray icon with a colored badge dot and overlay icon.
/// Returns raw RGBA bytes and dimensions directly (no intermediate PNG encoding).
fn generate_badge_icon(
    base_icon_bytes: &[u8],
    badge_color: [u8; 4],
    state: TrayBadgeState,
) -> Option<(Vec<u8>, u32, u32)> {
    let img = match image::load_from_memory(base_icon_bytes) {
        Ok(img) => img,
        Err(e) => {
            error!("Failed to decode base icon: {}", e);
            return None;
        }
    };

    let mut rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    // Badge parameters: bottom-right, nudged 1px up + 1px right (no white border, like Ubuntu Livepatch)
    let badge_radius = (width as f32 * 0.22).round() as i32; // 22% of width: visible like Ubuntu Livepatch
    let badge_center_x = width as i32 - badge_radius; // 1px more right than original
    let badge_center_y = height as i32 - badge_radius - 2; // 1px more up than original
    let badge_rgba = Rgba(badge_color);

    // Draw solid badge circle (no border: matches Ubuntu Livepatch style)
    let x_min = (badge_center_x - badge_radius).max(0);
    let x_max = (badge_center_x + badge_radius).min(width as i32 - 1);
    let y_min = (badge_center_y - badge_radius).max(0);
    let y_max = (badge_center_y + badge_radius).min(height as i32 - 1);

    for y in y_min..=y_max {
        for x in x_min..=x_max {
            let dx = x - badge_center_x;
            let dy = y - badge_center_y;
            let dist_sq = dx * dx + dy * dy;
            let radius_sq = badge_radius * badge_radius;

            if dist_sq <= radius_sq {
                rgba.put_pixel(x as u32, y as u32, badge_rgba);
            }
        }
    }

    // Draw overlay icon inside the badge circle
    let cx = badge_center_x as f32;
    let cy = badge_center_y as f32;
    let r = badge_radius as f32;

    match state {
        TrayBadgeState::Syncing => draw_badge_sync(&mut rgba, cx, cy, r),
        TrayBadgeState::Error => draw_badge_x_mark(&mut rgba, cx, cy, r),
        TrayBadgeState::Paused => draw_badge_pause(&mut rgba, cx, cy, r),
        TrayBadgeState::Default => {}
    }

    // Return raw RGBA directly: no PNG re-encoding needed
    Some((rgba.into_raw(), width, height))
}

/// Update the system tray icon badge based on sync state
///
/// # Arguments
/// * `app` - Tauri app handle
/// * `state` - The new badge state to display
pub fn update_tray_badge(app: &AppHandle, state: TrayBadgeState) {
    // Skip if state hasn't changed
    let current = CURRENT_TRAY_STATE.load(Ordering::SeqCst);
    if current == state as u8 {
        return;
    }

    info!("Updating tray badge: {:?}", state);

    // Generate the icon RGBA up front. This is pure CPU work (image decode +
    // badge overlay) with no GTK involvement, so it is safe on any thread.
    let icon_data: Option<(Vec<u8>, u32, u32)> = if let Some(badge_color) = state.badge_color() {
        generate_badge_icon(BASE_ICON_BYTES, badge_color, state)
    } else {
        // Default state: decode base icon directly to RGBA
        match image::load_from_memory(BASE_ICON_BYTES) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                Some((rgba.into_raw(), width, height))
            }
            Err(e) => {
                error!("Failed to decode base icon: {}", e);
                None
            }
        }
    };

    let Some((rgba_bytes, width, height)) = icon_data else {
        error!("Failed to generate tray icon for state {:?}", state);
        return;
    };

    // The actual tray mutations (`tray_by_id`, `set_icon`, `set_tooltip`) touch
    // GTK / the StatusNotifierItem over GDBus on Linux. They MUST run on the GTK
    // main thread. This function is called from background workers (e.g.
    // `background_sync_worker`, which runs on a tokio worker thread); calling the
    // mutations directly from there races the GLib main loop and corrupts the
    // GLib heap, which later aborts a GDBus worker with
    // "malloc(): unaligned fastbin chunk detected" (notably on suspend/resume).
    // Marshal every mutation onto the main thread to keep GLib single-threaded.
    let app_handle = app.clone();
    let marshal = app.run_on_main_thread(move || {
        let tray = match app_handle.tray_by_id("main") {
            Some(t) => t,
            None => {
                warn!("Tray icon 'main' not found, cannot update badge");
                return;
            }
        };

        let icon = Image::new_owned(rgba_bytes, width, height);
        if let Err(e) = tray.set_icon(Some(icon)) {
            error!("Failed to set tray icon: {}", e);
            return;
        }

        if let Err(e) = tray.set_tooltip(Some(state.tooltip())) {
            warn!("Failed to set tray tooltip: {}", e);
        }

        // Only record the applied state once the mutation actually ran on the
        // main thread, so a failed marshal does not skip the next update.
        CURRENT_TRAY_STATE.store(state as u8, Ordering::SeqCst);
        info!("Tray badge updated to {:?}", state);
    });

    if let Err(e) = marshal {
        error!(
            "Failed to marshal tray badge update onto main thread: {}",
            e
        );
    }
}

#[cfg(target_os = "linux")]
thread_local! {
    static TRAY_CLOUD_ITEMS: std::cell::RefCell<Option<[tauri::menu::MenuItem<tauri::Wry>; 3]>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(not(target_os = "linux"))]
thread_local! {
    static TRAY_CLOUD_ITEMS: std::cell::RefCell<
        Option<[tauri::menu::IconMenuItem<tauri::Wry>; 3]>,
    > = const { std::cell::RefCell::new(None) };
}

/// Keep the three AeroCloud rows so a later enable or disable can update them.
/// Called from setup on the GTK main thread. The items are not `Send`.
#[cfg(target_os = "linux")]
pub fn remember_tray_cloud_items(
    sync: tauri::menu::MenuItem<tauri::Wry>,
    pause: tauri::menu::MenuItem<tauri::Wry>,
    folder: tauri::menu::MenuItem<tauri::Wry>,
) {
    TRAY_CLOUD_ITEMS.with(|slot| *slot.borrow_mut() = Some([sync, pause, folder]));
}

#[cfg(not(target_os = "linux"))]
pub fn remember_tray_cloud_items(
    sync: tauri::menu::IconMenuItem<tauri::Wry>,
    pause: tauri::menu::IconMenuItem<tauri::Wry>,
    folder: tauri::menu::IconMenuItem<tauri::Wry>,
) {
    TRAY_CLOUD_ITEMS.with(|slot| *slot.borrow_mut() = Some([sync, pause, folder]));
}

fn apply_stored_cloud_items(enabled: bool) {
    TRAY_CLOUD_ITEMS.with(|slot| {
        let stored = slot.borrow();
        let Some(items) = stored.as_ref() else {
            return;
        };
        for item in items {
            if let Err(err) = item.set_enabled(enabled) {
                warn!("Failed to update a tray AeroCloud row: {err}");
            }
        }
    });
}

/// Enable or disable Sync Now, Pause Sync and Open Cloud Folder.
///
/// AeroCloud on or off is what these rows follow. The tray badge already
/// moves, on the GTK main thread, while a sync runs. These rows use that
/// same marshal and update the live items. The menu is not rebuilt.
pub fn set_tray_cloud_actions_enabled(app: &AppHandle, enabled: bool) {
    let marshal = app.run_on_main_thread(move || {
        #[cfg(target_os = "linux")]
        set_promoted_cloud_items_sensitive(enabled);
        apply_stored_cloud_items(enabled);
        info!("Tray AeroCloud rows enabled={enabled}");
    });
    if let Err(e) = marshal {
        error!("Failed to marshal tray menu update onto main thread: {e}");
    }
}

/// Marks on the tray menu. Light monochrome, same family as the white tray
/// glyph, so they read on a dark panel menu the way the MEGA tray icons do:
/// a small mark beside each entry.
///
/// On Linux the tray row stays a plain item until
/// `install_linux_tray_menu_icons` turns it into a `GtkImageMenuItem`.
/// The image is a theme icon name. A pixbuf published as `icon-data` makes
/// GNOME Shell's menu loader fail, and the row then paints with no label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayMenuGlyph {
    Sync,
    Pause,
    Folder,
    Update,
    Show,
    Quit,
}

/// A 16×16 menu icon for `glyph`.
///
/// Linux attaches a theme icon name instead. This bitmap is the Windows
/// and macOS menu image.
#[cfg(not(target_os = "linux"))]
pub fn tray_menu_icon(glyph: TrayMenuGlyph) -> Image<'static> {
    let mut rgba = image::RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0]));
    draw_tray_menu_glyph(&mut rgba, glyph);
    let (w, h) = rgba.dimensions();
    Image::new_owned(rgba.into_raw(), w, h)
}

#[cfg(any(not(target_os = "linux"), test))]
fn menu_ink() -> Rgba<u8> {
    Rgba([232, 234, 237, 255])
}

#[cfg(any(not(target_os = "linux"), test))]
fn draw_tray_menu_glyph(rgba: &mut image::RgbaImage, glyph: TrayMenuGlyph) {
    let ink = menu_ink();
    let stroke = 1.05;
    match glyph {
        TrayMenuGlyph::Pause => {
            draw_thick_line(rgba, 5.5, 3.5, 5.5, 12.5, stroke, ink);
            draw_thick_line(rgba, 10.5, 3.5, 10.5, 12.5, stroke, ink);
        }
        TrayMenuGlyph::Folder => {
            draw_thick_line(rgba, 3.0, 6.0, 3.0, 4.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 4.0, 7.0, 4.0, stroke, ink);
            draw_thick_line(rgba, 7.0, 4.0, 8.0, 6.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 6.0, 13.0, 6.0, stroke, ink);
            draw_thick_line(rgba, 13.0, 6.0, 13.0, 13.0, stroke, ink);
            draw_thick_line(rgba, 13.0, 13.0, 3.0, 13.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 13.0, 3.0, 6.0, stroke, ink);
        }
        TrayMenuGlyph::Show => {
            draw_thick_line(rgba, 3.0, 3.0, 13.0, 3.0, stroke, ink);
            draw_thick_line(rgba, 13.0, 3.0, 13.0, 10.0, stroke, ink);
            draw_thick_line(rgba, 13.0, 10.0, 3.0, 10.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 10.0, 3.0, 3.0, stroke, ink);
            draw_thick_line(rgba, 8.0, 10.0, 8.0, 13.0, stroke, ink);
            draw_thick_line(rgba, 5.0, 13.0, 11.0, 13.0, stroke, ink);
        }
        TrayMenuGlyph::Quit => {
            draw_thick_line(rgba, 3.0, 3.0, 3.0, 13.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 3.0, 7.0, 3.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 13.0, 7.0, 13.0, stroke, ink);
            draw_thick_line(rgba, 6.0, 8.0, 13.0, 8.0, stroke, ink);
            draw_thick_line(rgba, 13.0, 8.0, 10.0, 5.0, stroke, ink);
            draw_thick_line(rgba, 13.0, 8.0, 10.0, 11.0, stroke, ink);
        }
        TrayMenuGlyph::Update => {
            draw_thick_line(rgba, 8.0, 2.0, 8.0, 9.0, stroke, ink);
            draw_thick_line(rgba, 8.0, 9.0, 5.0, 6.0, stroke, ink);
            draw_thick_line(rgba, 8.0, 9.0, 11.0, 6.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 12.0, 13.0, 12.0, stroke, ink);
            draw_thick_line(rgba, 3.0, 12.0, 3.0, 14.0, stroke, ink);
            draw_thick_line(rgba, 13.0, 12.0, 13.0, 14.0, stroke, ink);
        }
        TrayMenuGlyph::Sync => {
            let (cx, cy, arc_r) = (8.0, 8.0, 4.6);
            let steps = 12u32;
            for &(start_deg, end_deg) in &[(300.0_f32, 430.0), (120.0, 250.0)] {
                for i in 0..steps {
                    let f0 = i as f32 / steps as f32;
                    let f1 = (i + 1) as f32 / steps as f32;
                    let a0 = (start_deg + (end_deg - start_deg) * f0).to_radians();
                    let a1 = (start_deg + (end_deg - start_deg) * f1).to_radians();
                    draw_thick_line(
                        rgba,
                        cx + arc_r * a0.cos(),
                        cy + arc_r * a0.sin(),
                        cx + arc_r * a1.cos(),
                        cy + arc_r * a1.sin(),
                        0.9,
                        ink,
                    );
                }
                let end = end_deg.to_radians();
                let tip_x = cx + arc_r * end.cos();
                let tip_y = cy + arc_r * end.sin();
                let (tang_x, tang_y) = (-end.sin(), end.cos());
                let (out_x, out_y) = (end.cos(), end.sin());
                let len = 2.4;
                draw_thick_line(
                    rgba,
                    tip_x,
                    tip_y,
                    tip_x - tang_x * len + out_x * 1.4,
                    tip_y - tang_y * len + out_y * 1.4,
                    0.9,
                    ink,
                );
                draw_thick_line(
                    rgba,
                    tip_x,
                    tip_y,
                    tip_x - tang_x * len - out_x * 1.4,
                    tip_y - tang_y * len - out_y * 1.4,
                    0.9,
                    ink,
                );
            }
        }
    }
}

/// Which tray mark belongs on `label`.
///
/// `Check for Updates` and `Quit` also exist on other menus. They take an
/// icon only inside the tray popup, which already contains `Sync Now` or
/// `Show AeroFTP` by the time those two rows are inserted.
#[cfg(any(target_os = "linux", test))]
fn tray_menu_glyph_for_label(
    label: &str,
    menu_contains_sync_now: bool,
    menu_contains_show: bool,
) -> Option<TrayMenuGlyph> {
    let in_tray_popup = menu_contains_sync_now || menu_contains_show;
    match label {
        "Sync Now" => Some(TrayMenuGlyph::Sync),
        "Pause Sync" => Some(TrayMenuGlyph::Pause),
        "Open Cloud Folder" => Some(TrayMenuGlyph::Folder),
        "Show AeroFTP" => Some(TrayMenuGlyph::Show),
        "Check for Updates" if in_tray_popup => Some(TrayMenuGlyph::Update),
        "Quit" if in_tray_popup => Some(TrayMenuGlyph::Quit),
        _ => None,
    }
}

/// Install the Linux tray-icon swap before the tray menu is built.
///
/// Ayatana parses the `GtkMenu` once, inside `app_indicator_set_menu`.
/// `libdbusmenu` exports an icon only from a `GtkImageMenuItem`, and a
/// pixbuf becomes `icon-data`. GNOME Shell 50 then fails that image
/// (`GdkPixbuf.PixbufError`) and the row draws blank. A theme `icon-name`
/// is the path the shell paints, on the right of the label, without
/// decoding a bitmap. The replacement still emits `activate` on the
/// original item, which forwards `tray_sync_now` and the other ids.
#[cfg(target_os = "linux")]
pub fn install_linux_tray_menu_icons() {
    use std::sync::Once;

    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let signal_id = gtk::glib::gobject_ffi::g_signal_lookup(
            c"insert".as_ptr(),
            gtk::ffi::gtk_menu_shell_get_type(),
        );
        if signal_id == 0 {
            warn!("GtkMenuShell::insert unavailable; tray menu stays without icons");
            return;
        }
        gtk::glib::gobject_ffi::g_signal_add_emission_hook(
            signal_id,
            0,
            Some(on_menu_shell_insert),
            std::ptr::null_mut(),
            None,
        );
        info!("Linux tray menu icon hook installed");
    });
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn on_menu_shell_insert(
    _hint: *mut gtk::glib::gobject_ffi::GSignalInvocationHint,
    n_values: libc::c_uint,
    values: *const gtk::glib::gobject_ffi::GValue,
    _data: gtk::glib::ffi::gpointer,
) -> gtk::glib::ffi::gboolean {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        promote_inserted_tray_item(values, n_values);
    }));
    gtk::glib::ffi::GTRUE
}

#[cfg(target_os = "linux")]
fn promote_inserted_tray_item(
    values: *const gtk::glib::gobject_ffi::GValue,
    n_values: libc::c_uint,
) {
    if n_values < 3 || values.is_null() {
        return;
    }
    // `insert` of the replacement row re-enters this hook. The flag skips
    // that nested emission; the image-item check below is the second guard.
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let _clear = ClearBusy(&BUSY);

    use gtk::glib::translate::FromGlibPtrNone;
    use gtk::prelude::*;

    unsafe {
        let shell_ptr = gtk::glib::gobject_ffi::g_value_get_object(values);
        let child_ptr = gtk::glib::gobject_ffi::g_value_get_object(values.add(1));
        if shell_ptr.is_null() || child_ptr.is_null() {
            return;
        }
        if gtk::glib::gobject_ffi::g_type_check_instance_is_a(
            child_ptr as *mut _,
            gtk::ffi::gtk_image_menu_item_get_type(),
        ) != gtk::glib::ffi::GFALSE
        {
            return;
        }
        let shell = gtk::Widget::from_glib_none(shell_ptr as *mut gtk::ffi::GtkWidget);
        let Ok(menu) = shell.downcast::<gtk::Menu>() else {
            return;
        };
        let child = gtk::Widget::from_glib_none(child_ptr as *mut gtk::ffi::GtkWidget);
        let Ok(item) = child.downcast::<gtk::MenuItem>() else {
            return;
        };
        let Some(label) = menu_item_label(&item) else {
            return;
        };
        let Some(glyph) = tray_menu_glyph_for_label(
            &label,
            menu_contains_label(&menu, "Sync Now"),
            menu_contains_label(&menu, "Show AeroFTP"),
        ) else {
            return;
        };
        let Some(position) = menu
            .children()
            .iter()
            .position(|candidate| candidate == item.upcast_ref::<gtk::Widget>())
        else {
            return;
        };

        let c_label = match std::ffi::CString::new(label.clone()) {
            Ok(value) => value,
            Err(_) => return,
        };
        let raw = gtk::ffi::gtk_image_menu_item_new_with_label(c_label.as_ptr());
        if raw.is_null() {
            return;
        }
        // Theme icon name, not a pixbuf. `icon-data` is what the shell
        // rejects, and a rejected image leaves the whole row blank.
        let image =
            gtk::Image::from_icon_name(Some(tray_menu_icon_name(glyph)), gtk::IconSize::Menu);
        gtk::ffi::gtk_image_menu_item_set_image(
            raw as *mut gtk::ffi::GtkImageMenuItem,
            image.upcast_ref::<gtk::Widget>().as_ptr() as *mut _,
        );
        gtk::ffi::gtk_image_menu_item_set_always_show_image(
            raw as *mut gtk::ffi::GtkImageMenuItem,
            gtk::glib::ffi::GTRUE,
        );
        // from_glib_none sinks the floating image-menu item. A failed
        // downcast drops that reference and frees it.
        let Ok(new_item) = gtk::Widget::from_glib_none(raw).downcast::<gtk::MenuItem>() else {
            return;
        };
        new_item.set_sensitive(item.is_sensitive());
        remember_promoted_cloud_item(&label, &new_item);
        let source = item.clone();
        new_item.connect_activate(move |_| {
            source.emit_activate();
        });
        menu.insert(&new_item, position as i32);
        menu.remove(&item);
        new_item.show_all();
        info!("tray menu icon attached: {label}");
    }
}

/// Sync Now, Pause Sync and Open Cloud Folder follow AeroCloud. The other
/// tray rows stay available when it is off.
#[cfg(any(target_os = "linux", test))]
fn is_tray_cloud_action(label: &str) -> bool {
    matches!(label, "Sync Now" | "Pause Sync" | "Open Cloud Folder")
}

#[cfg(target_os = "linux")]
thread_local! {
    static PROMOTED_CLOUD_ITEMS: std::cell::RefCell<Vec<gtk::MenuItem>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The visible row is the image item the hook inserted. `libdbusmenu` watches
/// that widget's `sensitive` property and publishes it as the menu `enabled`
/// flag, the same way a badge repaint publishes a new icon.
#[cfg(target_os = "linux")]
fn remember_promoted_cloud_item(label: &str, item: &gtk::MenuItem) {
    if !is_tray_cloud_action(label) {
        return;
    }
    PROMOTED_CLOUD_ITEMS.with(|slots| slots.borrow_mut().push(item.clone()));
}

#[cfg(target_os = "linux")]
fn set_promoted_cloud_items_sensitive(enabled: bool) {
    use gtk::prelude::*;
    PROMOTED_CLOUD_ITEMS.with(|slots| {
        for item in slots.borrow().iter() {
            item.set_sensitive(enabled);
        }
    });
}

#[cfg(target_os = "linux")]
fn menu_item_label(item: &gtk::MenuItem) -> Option<String> {
    use gtk::prelude::*;
    item.child()?
        .downcast::<gtk::Label>()
        .ok()
        .map(|label| label.text().to_string())
}

#[cfg(target_os = "linux")]
fn menu_contains_label(menu: &gtk::Menu, expected: &str) -> bool {
    use gtk::prelude::*;
    menu.children().iter().any(|child| {
        child
            .clone()
            .downcast::<gtk::MenuItem>()
            .ok()
            .and_then(|item| menu_item_label(&item))
            .is_some_and(|text| text == expected)
    })
}

/// Freedesktop name the shell already has. Symbolic, so it follows the menu color.
#[cfg(target_os = "linux")]
fn tray_menu_icon_name(glyph: TrayMenuGlyph) -> &'static str {
    match glyph {
        TrayMenuGlyph::Sync => "view-refresh-symbolic",
        TrayMenuGlyph::Pause => "media-playback-pause-symbolic",
        TrayMenuGlyph::Folder => "folder-symbolic",
        TrayMenuGlyph::Update => "software-update-available-symbolic",
        TrayMenuGlyph::Show => "window-restore-symbolic",
        TrayMenuGlyph::Quit => "system-shutdown-symbolic",
    }
}

#[cfg(target_os = "linux")]
struct ClearBusy(&'static AtomicBool);

#[cfg(target_os = "linux")]
impl Drop for ClearBusy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::TrayBadgeState;

    /// The grey Paused badge was designed with the others and never shown: the
    /// only way to it was `update_tray_badge_cmd`, which nothing called, and
    /// pausing AeroCloud reset the tray to the idle icon. A paused setup now
    /// starts on the Paused badge, and `pause_aerocloud` switches to it.
    #[test]
    fn a_paused_aerocloud_starts_on_the_paused_badge() {
        assert_eq!(
            TrayBadgeState::at_startup(true, true),
            TrayBadgeState::Paused
        );
        assert_eq!(
            TrayBadgeState::at_startup(true, false),
            TrayBadgeState::Default
        );
        assert_eq!(
            TrayBadgeState::at_startup(false, true),
            TrayBadgeState::Default
        );
    }

    /// The text of the top-level function starting at `signature`, up to its
    /// closing brace at column 0. Line endings are normalised first: with no
    /// `.gitattributes` in this repository a Windows checkout stores CRLF, and
    /// the first run of this test there (main d5f694ae2, Delta Sync MSVC unit
    /// tests) failed to find the closing brace.
    fn function_text(src: &str, signature: &str) -> Option<String> {
        let src = src.replace("\r\n", "\n");
        let start = src.find(signature)?;
        let end = start + src[start..].find("\n}\n")?;
        Some(src[start..end].to_string())
    }

    /// Resuming has to take the Paused badge down itself: with
    /// `sync_on_startup` off the restarted worker waits for a trigger before
    /// its first cycle, so nothing else repaints the tray and it stayed grey.
    #[test]
    fn resuming_aerocloud_clears_the_paused_badge() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
        let body = function_text(src, "async fn resume_aerocloud(")
            .expect("resume_aerocloud is defined and ends with a closing brace at column 0");
        assert!(
            body.contains(
                "tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Default);"
            ),
            "resume_aerocloud leaves the tray on the Paused badge"
        );
    }

    #[test]
    fn tray_menu_glyphs_paint_a_visible_mark() {
        for glyph in [
            super::TrayMenuGlyph::Sync,
            super::TrayMenuGlyph::Pause,
            super::TrayMenuGlyph::Folder,
            super::TrayMenuGlyph::Update,
            super::TrayMenuGlyph::Show,
            super::TrayMenuGlyph::Quit,
        ] {
            let mut rgba = image::RgbaImage::from_pixel(16, 16, image::Rgba([0, 0, 0, 0]));
            super::draw_tray_menu_glyph(&mut rgba, glyph);
            let painted = rgba.pixels().filter(|px| px[3] > 0).count();
            assert!(painted >= 8, "tray glyph painted only {painted} pixels");
        }
    }

    /// `Check for Updates` and `Quit` live in the application menu too.
    /// Icons attach only when the popup is the tray menu.
    #[test]
    fn tray_menu_icons_attach_only_to_the_tray_popup() {
        assert_eq!(
            super::tray_menu_glyph_for_label("Sync Now", false, false),
            Some(super::TrayMenuGlyph::Sync)
        );
        assert_eq!(
            super::tray_menu_glyph_for_label("Pause Sync", false, false),
            Some(super::TrayMenuGlyph::Pause)
        );
        assert_eq!(
            super::tray_menu_glyph_for_label("Open Cloud Folder", false, false),
            Some(super::TrayMenuGlyph::Folder)
        );
        assert_eq!(
            super::tray_menu_glyph_for_label("Show AeroFTP", false, false),
            Some(super::TrayMenuGlyph::Show)
        );
        assert_eq!(
            super::tray_menu_glyph_for_label("Check for Updates", false, false),
            None
        );
        assert_eq!(super::tray_menu_glyph_for_label("Quit", false, false), None);
        assert_eq!(
            super::tray_menu_glyph_for_label("Check for Updates", true, false),
            Some(super::TrayMenuGlyph::Update)
        );
        assert_eq!(
            super::tray_menu_glyph_for_label("Quit", false, true),
            Some(super::TrayMenuGlyph::Quit)
        );
        assert_eq!(
            super::tray_menu_glyph_for_label("Quit AeroFTP", true, true),
            None
        );
    }

    /// Turning AeroCloud on or off repaints these three rows. The badge path
    /// already moves for a running sync; Quit and the window rows stay put.
    #[test]
    fn tray_menu_cloud_rows_follow_aerocloud() {
        assert!(super::is_tray_cloud_action("Sync Now"));
        assert!(super::is_tray_cloud_action("Pause Sync"));
        assert!(super::is_tray_cloud_action("Open Cloud Folder"));
        assert!(!super::is_tray_cloud_action("Check for Updates"));
        assert!(!super::is_tray_cloud_action("Show AeroFTP"));
        assert!(!super::is_tray_cloud_action("Quit"));

        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
        for signature in [
            "async fn save_cloud_config_cmd(",
            "async fn setup_aerocloud(",
            "async fn disable_aerocloud(",
        ] {
            let body = match function_text(src, signature) {
                Some(body) => body,
                None => panic!("{signature} is missing a column-0 closing brace"),
            };
            assert!(
                body.contains("tray_badge::set_tray_cloud_actions_enabled("),
                "{signature} leaves the tray rows at their startup state"
            );
        }
    }

    #[test]
    fn a_crlf_checkout_still_yields_the_function_text() {
        let src = "fn before() {}\r\n\r\nasync fn resume_aerocloud() {\r\n    repaint();\r\n}\r\nfn after() {}\r\n";
        let body = function_text(src, "async fn resume_aerocloud(")
            .expect("the function end is found in a CRLF checkout");
        assert!(body.contains("repaint();"));
        assert!(!body.contains("after"));
    }
}
