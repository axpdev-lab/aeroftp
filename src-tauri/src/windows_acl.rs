// Windows-specific utilities: ACL permissions + reserved filename validation
// Equivalent to Unix chmod 0o600/0o700
#![allow(dead_code)]

/// Restrict file/directory access to the current user using icacls.
/// Silently ignores errors (best-effort hardening).
#[cfg(windows)]
pub fn restrict_to_owner(path: &std::path::Path) {
    let path_str = path.to_string_lossy();
    // Remove inherited permissions and grant full control only to current user
    let username = std::env::var("USERNAME").unwrap_or_else(|_| "".to_string());
    if username.is_empty() {
        return;
    }
    // Reset ACLs: remove inheritance, remove all explicit, then grant current user full control
    let _ = std::process::Command::new("icacls")
        .args([&*path_str, "/inheritance:r", "/grant:r", &format!("{}:F", username), "/T", "/Q"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output();
}

/// No-op on non-Windows platforms
#[cfg(not(windows))]
pub fn restrict_to_owner(_path: &std::path::Path) {}

/// Check if a filename is reserved on Windows (CON, PRN, AUX, NUL, COM1-9, LPT1-9).
/// Returns Some(reserved_name) if reserved, None if safe.
pub fn check_windows_reserved(filename: &str) -> Option<&'static str> {
    let stem = filename.split('.').next().unwrap_or(filename);
    let upper = stem.to_uppercase();
    match upper.as_str() {
        "CON" => Some("CON"),
        "PRN" => Some("PRN"),
        "AUX" => Some("AUX"),
        "NUL" => Some("NUL"),
        "COM1" => Some("COM1"), "COM2" => Some("COM2"), "COM3" => Some("COM3"),
        "COM4" => Some("COM4"), "COM5" => Some("COM5"), "COM6" => Some("COM6"),
        "COM7" => Some("COM7"), "COM8" => Some("COM8"), "COM9" => Some("COM9"),
        "LPT1" => Some("LPT1"), "LPT2" => Some("LPT2"), "LPT3" => Some("LPT3"),
        "LPT4" => Some("LPT4"), "LPT5" => Some("LPT5"), "LPT6" => Some("LPT6"),
        "LPT7" => Some("LPT7"), "LPT8" => Some("LPT8"), "LPT9" => Some("LPT9"),
        _ => None,
    }
}
