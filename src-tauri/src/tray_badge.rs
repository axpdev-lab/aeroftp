// Tray badge icon generation and management
// Generates tray icons with colored dot badges + overlay icons for AeroCloud sync state

use std::sync::atomic::{AtomicU8, Ordering};
use image::Rgba;
use tauri::image::Image;
use tauri::AppHandle;
use tracing::{info, warn, error};

/// Tray badge states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrayBadgeState {
    Default = 0,   // No badge — idle/synced (Dropbox-style: no badge = all good)
    Syncing = 1,   // Blue dot + white sync arrows (sync in progress)
    Error = 2,     // Red dot + white X (error occurred)
    Paused = 3,    // Grey dot + white pause bars (user paused sync)
}

impl TrayBadgeState {
    /// Parse state from string (for Tauri command)
    pub fn from_str(s: &str) -> Self {
        match s {
            "syncing" => Self::Syncing,
            "error" => Self::Error,
            "paused" => Self::Paused,
            "default" | "synced" => Self::Default, // "synced" mapped to Default (Dropbox-style)
            other => {
                warn!("Unrecognized tray badge state: {:?}, defaulting to Default", other);
                Self::Default
            }
        }
    }

    /// Get badge color (RGBA)
    fn badge_color(&self) -> Option<[u8; 4]> {
        match self {
            Self::Default => None,
            Self::Syncing => Some([33, 150, 243, 255]),   // #2196F3 - Material Blue 500
            Self::Error => Some([244, 67, 54, 255]),      // #F44336 - Material Red 500
            Self::Paused => Some([158, 158, 158, 255]),   // #9E9E9E - Material Grey 500
        }
    }

    /// Get tray tooltip text
    fn tooltip(&self) -> &'static str {
        match self {
            Self::Default => "AeroFTP",
            Self::Syncing => "AeroSync — Syncing...",
            Self::Error => "AeroSync — Sync Error",
            Self::Paused => "AeroSync — Paused",
        }
    }
}

// Current tray state (atomic for lock-free reads)
static CURRENT_TRAY_STATE: AtomicU8 = AtomicU8::new(0);

// Base icon bytes — white monochrome (standard for system tray)
const BASE_ICON_BYTES: &[u8] = include_bytes!("../../icons/AeroFTP_simbol_white_120x120.png");

/// Draw a thick line segment between two points using distance-based rasterization.
/// Pixels within `half_w` distance of the line segment get colored.
fn draw_thick_line(
    rgba: &mut image::RgbaImage,
    x0: f32, y0: f32,
    x1: f32, y1: f32,
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

/// Draw white pause bars (‖) inside the badge — two vertical bars
fn draw_badge_pause(rgba: &mut image::RgbaImage, cx: f32, cy: f32, r: f32) {
    let white = Rgba([255, 255, 255, 255]);
    let hw = (r * 0.14).max(1.0);
    let bar_half_h = r * 0.35;
    let bar_spacing = r * 0.22;

    // Left bar
    draw_thick_line(
        rgba,
        cx - bar_spacing, cy - bar_half_h,
        cx - bar_spacing, cy + bar_half_h,
        hw, white,
    );
    // Right bar
    draw_thick_line(
        rgba,
        cx + bar_spacing, cy - bar_half_h,
        cx + bar_spacing, cy + bar_half_h,
        hw, white,
    );
}

/// Draw white sync arrows (↻) inside the badge — two curved arrows forming a cycle
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

    // Badge parameters — bottom-right, nudged 1px up + 1px right (no white border, like Ubuntu Livepatch)
    let badge_radius = (width as f32 * 0.22).round() as i32; // 22% of width — visible like Ubuntu Livepatch
    let badge_center_x = width as i32 - badge_radius;         // 1px more right than original
    let badge_center_y = height as i32 - badge_radius - 2;    // 1px more up than original
    let badge_rgba = Rgba(badge_color);

    // Draw solid badge circle (no border — matches Ubuntu Livepatch style)
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

    // Return raw RGBA directly — no PNG re-encoding needed
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

    let tray = match app.tray_by_id("main") {
        Some(t) => t,
        None => {
            warn!("Tray icon 'main' not found, cannot update badge");
            return;
        }
    };

    // Generate icon — either with badge overlay or plain base icon
    let icon_result = if let Some(badge_color) = state.badge_color() {
        generate_badge_icon(BASE_ICON_BYTES, badge_color, state)
            .map(|(rgba_bytes, width, height)| Image::new_owned(rgba_bytes, width, height))
    } else {
        // Default state: decode base icon directly to RGBA
        match image::load_from_memory(BASE_ICON_BYTES) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                Some(Image::new_owned(rgba.into_raw(), width, height))
            }
            Err(e) => {
                error!("Failed to decode base icon: {}", e);
                None
            }
        }
    };

    match icon_result {
        Some(icon) => {
            if let Err(e) = tray.set_icon(Some(icon)) {
                error!("Failed to set tray icon: {}", e);
                return;
            }
        }
        None => {
            error!("Failed to generate tray icon for state {:?}", state);
            return;
        }
    }

    if let Err(e) = tray.set_tooltip(Some(state.tooltip())) {
        warn!("Failed to set tray tooltip: {}", e);
    }

    CURRENT_TRAY_STATE.store(state as u8, Ordering::SeqCst);

    info!("Tray badge updated to {:?}", state);
}
