//! Linux libmtp backend for MTP portable devices (APPENDIX-MTP Phase 2).
//!
//! Compiled only when `build.rs` finds libmtp via pkg-config and sets
//! `cfg(mtp_libmtp)`. Hosts without `libmtp-dev` keep the Null backend and
//! CI stays green.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(all(target_os = "linux", mtp_libmtp))]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex, Once};

use async_trait::async_trait;

use crate::providers::mtp::backend::{
    MtpBackend, MtpDeviceInfo, MtpObject, MtpObjectId, MtpProgress, MtpStorage,
};
use crate::providers::types::ProviderError;

// ─── Minimal FFI surface (libmtp 1.1.x) ─────────────────────────────────────

const LIBMTP_FILES_AND_FOLDERS_ROOT: u32 = 0xffff_ffff;
const LIBMTP_STORAGE_SORTBY_NOTSORTED: c_int = 0;
const LIBMTP_FILETYPE_FOLDER: c_int = 0;
const LIBMTP_FILETYPE_UNKNOWN: c_int = 44; // last enum member in 1.1.21

const LIBMTP_ERROR_NONE: c_int = 0;
const LIBMTP_ERROR_NO_DEVICE_ATTACHED: c_int = 5;
const LIBMTP_ERROR_CONNECTING: c_int = 7;
const LIBMTP_ERROR_STORAGE_FULL: c_int = 6;
#[allow(dead_code)]
const LIBMTP_ERROR_CANCELLED: c_int = 8;

#[repr(C)]
#[derive(Clone, Copy)]
struct LibmtpDeviceEntry {
    vendor: *mut c_char,
    vendor_id: u16,
    product: *mut c_char,
    product_id: u16,
    device_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LibmtpRawDevice {
    device_entry: LibmtpDeviceEntry,
    bus_location: u32,
    devnum: u8,
}

#[repr(C)]
struct LibmtpDeviceStorage {
    id: u32,
    storage_type: u16,
    filesystem_type: u16,
    access_capability: u16,
    max_capacity: u64,
    free_space_in_bytes: u64,
    free_space_in_objects: u64,
    storage_description: *mut c_char,
    volume_identifier: *mut c_char,
    next: *mut LibmtpDeviceStorage,
    prev: *mut LibmtpDeviceStorage,
}

/// Opaque device handle: we only read the `storage` field (offset after known
/// prefix). Layout matches libmtp 1.1.x `LIBMTP_mtpdevice_struct`.
#[repr(C)]
struct LibmtpMtpDevice {
    object_bitsize: u8,
    params: *mut c_void,
    usbinfo: *mut c_void,
    storage: *mut LibmtpDeviceStorage,
    // remainder unused by us
}

#[repr(C)]
struct LibmtpFile {
    item_id: u32,
    parent_id: u32,
    storage_id: u32,
    filename: *mut c_char,
    filesize: u64,
    modificationdate: libc::time_t,
    filetype: c_int,
    next: *mut LibmtpFile,
}

type LibmtpProgressFunc =
    Option<unsafe extern "C" fn(sent: u64, total: u64, data: *const c_void) -> c_int>;

#[link(name = "mtp")]
extern "C" {
    fn LIBMTP_Init();
    fn LIBMTP_Detect_Raw_Devices(devices: *mut *mut LibmtpRawDevice, numdevs: *mut c_int) -> c_int;
    fn LIBMTP_Open_Raw_Device(raw: *mut LibmtpRawDevice) -> *mut LibmtpMtpDevice;
    fn LIBMTP_Open_Raw_Device_Uncached(raw: *mut LibmtpRawDevice) -> *mut LibmtpMtpDevice;
    fn LIBMTP_Release_Device(device: *mut LibmtpMtpDevice);
    fn LIBMTP_Get_Storage(device: *mut LibmtpMtpDevice, sortby: c_int) -> c_int;
    fn LIBMTP_Get_Friendlyname(device: *mut LibmtpMtpDevice) -> *mut c_char;
    fn LIBMTP_Get_Modelname(device: *mut LibmtpMtpDevice) -> *mut c_char;
    fn LIBMTP_Get_Serialnumber(device: *mut LibmtpMtpDevice) -> *mut c_char;
    fn LIBMTP_Get_Manufacturername(device: *mut LibmtpMtpDevice) -> *mut c_char;
    fn LIBMTP_Get_Files_And_Folders(
        device: *mut LibmtpMtpDevice,
        storage: u32,
        parent: u32,
    ) -> *mut LibmtpFile;
    fn LIBMTP_Get_File_To_File(
        device: *mut LibmtpMtpDevice,
        id: u32,
        path: *const c_char,
        callback: LibmtpProgressFunc,
        data: *const c_void,
    ) -> c_int;
    fn LIBMTP_Send_File_From_File(
        device: *mut LibmtpMtpDevice,
        path: *const c_char,
        filedata: *mut LibmtpFile,
        callback: LibmtpProgressFunc,
        data: *const c_void,
    ) -> c_int;
    fn LIBMTP_Delete_Object(device: *mut LibmtpMtpDevice, object_id: u32) -> c_int;
    fn LIBMTP_Create_Folder(
        device: *mut LibmtpMtpDevice,
        name: *mut c_char,
        parent_id: u32,
        storage_id: u32,
    ) -> u32;
    fn LIBMTP_new_file_t() -> *mut LibmtpFile;
    fn LIBMTP_destroy_file_t(file: *mut LibmtpFile);
    fn LIBMTP_FreeMemory(ptr: *mut c_void);
    fn LIBMTP_Clear_Errorstack(device: *mut LibmtpMtpDevice);
    fn LIBMTP_Get_Errorstack(device: *mut LibmtpMtpDevice) -> *mut LibmtpError;
}

#[repr(C)]
struct LibmtpError {
    errornumber: c_int,
    error_text: *mut c_char,
    next: *mut LibmtpError,
}

// ─── Process-wide serialization (libmtp is not thread-safe) ─────────────────

static INIT: Once = Once::new();
/// Global lock: every libmtp call in this process must hold it.
static LIBMTP_LOCK: Mutex<()> = Mutex::new(());

fn ensure_init() {
    INIT.call_once(|| unsafe {
        LIBMTP_Init();
    });
}

fn cstr_opt(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn free_c_string(p: *mut c_char) {
    if !p.is_null() {
        unsafe { LIBMTP_FreeMemory(p as *mut c_void) };
    }
}

fn raw_device_id(raw: &LibmtpRawDevice) -> String {
    format!("usb:{}:{}", raw.bus_location, raw.devnum)
}

fn parse_device_id(id: &str) -> Result<(u32, u8), ProviderError> {
    // Format: usb:{bus}:{devnum}
    let rest = id.strip_prefix("usb:").ok_or_else(|| {
        ProviderError::InvalidConfig(format!(
            "invalid MTP device_id (expected usb:bus:dev): {id}"
        ))
    })?;
    let mut parts = rest.split(':');
    let bus: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ProviderError::InvalidConfig(format!("invalid MTP bus in {id}")))?;
    let dev: u8 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ProviderError::InvalidConfig(format!("invalid MTP devnum in {id}")))?;
    if parts.next().is_some() {
        return Err(ProviderError::InvalidConfig(format!(
            "invalid MTP device_id (extra segments): {id}"
        )));
    }
    Ok((bus, dev))
}

fn map_detect_error(code: c_int) -> ProviderError {
    match code {
        LIBMTP_ERROR_NONE => ProviderError::NetworkError("libmtp detect returned NONE".into()),
        LIBMTP_ERROR_NO_DEVICE_ATTACHED => {
            // Treat as empty list at call site; this is for unexpected paths.
            ProviderError::NotFound("no MTP device attached".into())
        }
        LIBMTP_ERROR_CONNECTING => ProviderError::ConnectionFailed(
            "could not connect to MTP device (busy or permission denied; unmount gvfs/nautilus MTP if open)".into(),
        ),
        other => ProviderError::ConnectionFailed(format!("libmtp detect error code {other}")),
    }
}

fn device_error_message(device: *mut LibmtpMtpDevice) -> String {
    if device.is_null() {
        return "null device".into();
    }
    unsafe {
        let mut cur = LIBMTP_Get_Errorstack(device);
        let mut parts = Vec::new();
        while !cur.is_null() {
            if let Some(t) = cstr_opt((*cur).error_text) {
                parts.push(t);
            }
            cur = (*cur).next;
        }
        LIBMTP_Clear_Errorstack(device);
        if parts.is_empty() {
            "libmtp operation failed".into()
        } else {
            parts.join("; ")
        }
    }
}

fn friendly_busy(msg: &str) -> ProviderError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("busy")
        || lower.contains("claim")
        || lower.contains("access")
        || lower.contains("resource")
    {
        ProviderError::ConnectionFailed(
            "MTP device is busy (another program such as a file manager may have it open). Close that session and retry.".into(),
        )
    } else {
        ProviderError::ConnectionFailed(msg.to_string())
    }
}

fn parse_handle(handle: &str) -> Result<u32, ProviderError> {
    if let Some(rest) = handle.strip_prefix("storage:") {
        return rest
            .parse::<u32>()
            .map_err(|_| ProviderError::InvalidPath(format!("bad storage handle: {handle}")));
    }
    handle
        .parse::<u32>()
        .map_err(|_| ProviderError::InvalidPath(format!("bad object handle: {handle}")))
}

fn parse_storage_id(s: &str) -> Result<u32, ProviderError> {
    s.parse::<u32>()
        .map_err(|_| ProviderError::InvalidPath(format!("bad storage_id: {s}")))
}

unsafe extern "C" fn progress_trampoline(sent: u64, total: u64, data: *const c_void) -> c_int {
    if data.is_null() {
        return 0;
    }
    // data points to Box<dyn Fn(u64, u64) + Send> kept alive by the caller.
    let cb = &*(data as *const Box<dyn Fn(u64, u64) + Send>);
    cb(sent, total);
    0
}

// ─── Backend state ──────────────────────────────────────────────────────────

struct Inner {
    device: *mut LibmtpMtpDevice,
    device_id: Option<String>,
    display_name: Option<String>,
}

// SAFETY: `device` is only touched while holding LIBMTP_LOCK + Inner mutex.
unsafe impl Send for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        if !self.device.is_null() {
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            ensure_init();
            unsafe {
                LIBMTP_Release_Device(self.device);
            }
            self.device = ptr::null_mut();
        }
    }
}

/// libmtp-backed [`MtpBackend`] for Linux.
pub struct LinuxLibmtpBackend {
    inner: Arc<Mutex<Inner>>,
}

impl LinuxLibmtpBackend {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                device: ptr::null_mut(),
                device_id: None,
                display_name: None,
            })),
        }
    }

    fn lock_inner(&self) -> Result<std::sync::MutexGuard<'_, Inner>, ProviderError> {
        self.inner
            .lock()
            .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))
    }
}

impl Default for LinuxLibmtpBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Read USB iSerial from sysfs for `bus:devnum` without claiming the interface.
///
/// Prefer this over a brief libmtp open during list/detect so PLACES polling
/// does not fight gvfs or an already-open AeroFTP session.
///
/// Note: `/sys/bus/usb/devices` mixes device nodes (`5-1`) with interfaces
/// (`5-1:1.0`) and hubs. Skip entries missing busnum/devnum; do not `?` out of
/// the whole scan on the first incomplete row.
fn usb_sysfs_serial(bus: u32, devnum: u8) -> Option<String> {
    let dir = std::fs::read_dir("/sys/bus/usb/devices").ok()?;
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(busnum) = std::fs::read_to_string(path.join("busnum"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        else {
            continue;
        };
        let Some(dn) = std::fs::read_to_string(path.join("devnum"))
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
        else {
            continue;
        };
        if busnum != bus || dn != devnum {
            continue;
        }
        let serial = std::fs::read_to_string(path.join("serial")).ok()?;
        let s = serial.trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
        return None;
    }
    None
}

/// Brief open → `LIBMTP_Get_Serialnumber` → release, using a pointer into the
/// live `Detect_Raw_Devices` array (same open rule as `open_raw_by_id_locked`).
///
/// Caller must hold `LIBMTP_LOCK`. Always releases on success path; never leaves
/// a dual-open USB session for list/detect.
unsafe fn serial_via_brief_open(raw_ref: *mut LibmtpRawDevice) -> Option<String> {
    if raw_ref.is_null() {
        return None;
    }
    let mut dev = LIBMTP_Open_Raw_Device_Uncached(raw_ref);
    if dev.is_null() {
        dev = LIBMTP_Open_Raw_Device(raw_ref);
    }
    if dev.is_null() {
        return None;
    }
    let serial_ptr = LIBMTP_Get_Serialnumber(dev);
    let owned = cstr_opt(serial_ptr);
    free_c_string(serial_ptr);
    LIBMTP_Release_Device(dev);
    owned
}

/// Caller must hold `LIBMTP_LOCK`.
///
/// Returns owned device rows. The raw libmtp array is freed before return;
/// only POD fields of `LibmtpRawDevice` are copied (safe for list/display).
///
/// Identity for device profiles (APPENDIX-DEVICE-PROFILES Phase 0):
/// - `vendor_id` / `product_id` always from the raw entry
/// - `serial` from sysfs iSerial when present; else brief open+Get_Serialnumber
/// - never leaves the device open after list
fn detect_raw_devices_locked(
) -> Result<Vec<(String, MtpDeviceInfo, LibmtpRawDevice)>, ProviderError> {
    ensure_init();

    let mut raw_ptr: *mut LibmtpRawDevice = ptr::null_mut();
    let mut num: c_int = 0;
    let rc = unsafe { LIBMTP_Detect_Raw_Devices(&mut raw_ptr, &mut num) };
    if rc == LIBMTP_ERROR_NO_DEVICE_ATTACHED || (rc == LIBMTP_ERROR_NONE && num == 0) {
        return Ok(Vec::new());
    }
    if rc != LIBMTP_ERROR_NONE {
        return Err(map_detect_error(rc));
    }
    if raw_ptr.is_null() {
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(num as usize);
    unsafe {
        for i in 0..num as usize {
            let raw = *raw_ptr.add(i);
            let id = raw_device_id(&raw);
            let product = cstr_opt(raw.device_entry.product);
            let vendor = cstr_opt(raw.device_entry.vendor);
            let display = match (vendor.as_deref(), product.as_deref()) {
                (Some(v), Some(p)) => format!("{v} {p}"),
                (None, Some(p)) => p.to_string(),
                (Some(v), None) => v.to_string(),
                _ => format!(
                    "MTP {:04x}:{:04x}",
                    raw.device_entry.vendor_id, raw.device_entry.product_id
                ),
            };
            // Prefer sysfs (no exclusive claim). Fall back to brief open only
            // when iSerial is missing; open failure leaves serial None but
            // vid/pid still support a weak fingerprint.
            let serial = usb_sysfs_serial(raw.bus_location, raw.devnum)
                .or_else(|| serial_via_brief_open(raw_ptr.add(i)));
            let info = MtpDeviceInfo {
                device_id: id.clone(),
                display_name: display,
                serial,
                vendor_id: Some(raw.device_entry.vendor_id),
                product_id: Some(raw.device_entry.product_id),
                bus_location: Some(format!("{}:{}", raw.bus_location, raw.devnum)),
                platform: "linux-libmtp".into(),
                storages_hint: 0,
            };
            out.push((id, info, raw));
        }
        libc::free(raw_ptr as *mut c_void);
    }
    Ok(out)
}

fn detect_raw_devices() -> Result<Vec<(String, MtpDeviceInfo, LibmtpRawDevice)>, ProviderError> {
    let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    detect_raw_devices_locked()
}

/// Caller must hold `LIBMTP_LOCK`.
///
/// Open must use a pointer into the live `Detect_Raw_Devices` allocation (same
/// pattern as libmtp examples and our C smoke). Freeing the array before
/// `LIBMTP_Open_Raw_Device*` can leave dangling internal pointers and yields
/// PTP_ERROR_IO on some Sony/Android devices even when detect succeeds.
fn open_raw_by_id_locked(device_id: &str) -> Result<(*mut LibmtpMtpDevice, String), ProviderError> {
    let (bus, devnum) = parse_device_id(device_id)?;
    ensure_init();

    let mut raw_ptr: *mut LibmtpRawDevice = ptr::null_mut();
    let mut num: c_int = 0;
    let rc = unsafe { LIBMTP_Detect_Raw_Devices(&mut raw_ptr, &mut num) };
    if rc == LIBMTP_ERROR_NO_DEVICE_ATTACHED || (rc == LIBMTP_ERROR_NONE && num == 0) {
        return Err(ProviderError::NotFound(format!(
            "MTP device {device_id} not found (unplugged or not in file-transfer mode)"
        )));
    }
    if rc != LIBMTP_ERROR_NONE {
        return Err(map_detect_error(rc));
    }
    if raw_ptr.is_null() || num < 1 {
        return Err(ProviderError::NotFound(format!(
            "MTP device {device_id} not found (unplugged or not in file-transfer mode)"
        )));
    }

    let mut match_idx: Option<usize> = None;
    unsafe {
        for i in 0..num as usize {
            let raw = &*raw_ptr.add(i);
            let id = raw_device_id(raw);
            if id == device_id || (raw.bus_location == bus && raw.devnum == devnum) {
                match_idx = Some(i);
                break;
            }
        }
    }
    let idx = match_idx.ok_or_else(|| {
        unsafe {
            libc::free(raw_ptr as *mut c_void);
        }
        ProviderError::NotFound(format!(
            "MTP device {device_id} not found (unplugged or not in file-transfer mode)"
        ))
    })?;

    let device = unsafe {
        let raw_ref = raw_ptr.add(idx);
        let mut dev = LIBMTP_Open_Raw_Device_Uncached(raw_ref);
        if dev.is_null() {
            // Cached open is a second chance used by libmtp tools after a
            // flaky uncached session on some Android builds.
            dev = LIBMTP_Open_Raw_Device(raw_ref);
        }
        // Free detect array only after open has consumed the raw entry.
        libc::free(raw_ptr as *mut c_void);
        dev
    };
    if device.is_null() {
        return Err(ProviderError::ConnectionFailed(
            "failed to open MTP device (busy, unauthorized, or not in MTP mode). If a file manager has the phone open, close that window and retry.".into(),
        ));
    }

    // Build display name from open device strings (owned, free after copy).
    let display = unsafe {
        let friendly = LIBMTP_Get_Friendlyname(device);
        let model = LIBMTP_Get_Modelname(device);
        let mfg = LIBMTP_Get_Manufacturername(device);
        let name = cstr_opt(friendly)
            .or_else(|| {
                let m = cstr_opt(model);
                let v = cstr_opt(mfg);
                match (v, m) {
                    (Some(v), Some(m)) => Some(format!("{v} {m}")),
                    (None, Some(m)) => Some(m),
                    (Some(v), None) => Some(v),
                    _ => None,
                }
            })
            .unwrap_or_else(|| device_id.to_string());
        free_c_string(friendly);
        free_c_string(model);
        free_c_string(mfg);
        let serial = LIBMTP_Get_Serialnumber(device);
        free_c_string(serial);
        name
    };

    Ok((device, display))
}

/// Caller must hold `LIBMTP_LOCK`.
fn list_storages_on_locked(device: *mut LibmtpMtpDevice) -> Result<Vec<MtpStorage>, ProviderError> {
    ensure_init();
    let rc = unsafe { LIBMTP_Get_Storage(device, LIBMTP_STORAGE_SORTBY_NOTSORTED) };
    if rc != 0 {
        let msg = device_error_message(device);
        return Err(friendly_busy(&msg));
    }
    let mut out = Vec::new();
    unsafe {
        let mut cur = (*device).storage;
        while !cur.is_null() {
            let s = &*cur;
            let desc =
                cstr_opt(s.storage_description).unwrap_or_else(|| format!("Storage {}", s.id));
            out.push(MtpStorage {
                storage_id: s.id.to_string(),
                display_name: desc,
                total_bytes: if s.max_capacity > 0 {
                    Some(s.max_capacity)
                } else {
                    None
                },
                free_bytes: Some(s.free_space_in_bytes),
            });
            cur = s.next;
        }
    }
    Ok(out)
}

/// Caller must hold `LIBMTP_LOCK`.
fn list_objects_on_locked(
    device: *mut LibmtpMtpDevice,
    parent: Option<&MtpObjectId>,
    storage_id: Option<&str>,
) -> Result<Vec<MtpObject>, ProviderError> {
    let storage = match storage_id {
        Some(s) => parse_storage_id(s)?,
        None => {
            if let Some(p) = parent {
                parse_storage_id(&p.storage_id)?
            } else {
                return Err(ProviderError::InvalidPath(
                    "list_objects requires storage_id at storage root".into(),
                ));
            }
        }
    };
    let parent_handle = match parent {
        None => LIBMTP_FILES_AND_FOLDERS_ROOT,
        Some(p) if p.handle.starts_with("storage:") => LIBMTP_FILES_AND_FOLDERS_ROOT,
        Some(p) => parse_handle(&p.handle)?,
    };

    ensure_init();
    let head = unsafe { LIBMTP_Get_Files_And_Folders(device, storage, parent_handle) };
    // NULL can mean empty folder or error; empty is fine.
    let mut out = Vec::new();
    unsafe {
        let mut cur = head;
        while !cur.is_null() {
            let f = &*cur;
            let name = cstr_opt(f.filename).unwrap_or_else(|| format!("object-{}", f.item_id));
            let is_dir = f.filetype == LIBMTP_FILETYPE_FOLDER;
            let modified = if f.modificationdate > 0 {
                Some(f.modificationdate.to_string())
            } else {
                None
            };
            out.push(MtpObject {
                id: MtpObjectId {
                    storage_id: f.storage_id.to_string(),
                    handle: f.item_id.to_string(),
                },
                name,
                is_dir,
                size: if is_dir { 0 } else { f.filesize },
                modified,
            });
            let next = f.next;
            LIBMTP_destroy_file_t(cur);
            cur = next;
        }
    }
    Ok(out)
}

fn run_with_progress(
    on_progress: Option<MtpProgress>,
    mut op: impl FnMut(LibmtpProgressFunc, *const c_void) -> c_int,
) -> c_int {
    if let Some(cb) = on_progress {
        let data = &cb as *const Box<dyn Fn(u64, u64) + Send> as *const c_void;
        op(Some(progress_trampoline), data)
    } else {
        op(None, ptr::null())
    }
}

#[async_trait]
impl MtpBackend for LinuxLibmtpBackend {
    async fn list_devices(&self) -> Result<Vec<MtpDeviceInfo>, ProviderError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _keep = inner; // keep Arc alive for lifetime consistency
            let found = detect_raw_devices()?;
            Ok(found.into_iter().map(|(_, info, _)| info).collect())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP list_devices join: {e}")))?
    }

    async fn open(&mut self, device_id: &str) -> Result<(), ProviderError> {
        if device_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "MTP device_id is required".into(),
            ));
        }
        let id = device_id.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if !guard.device.is_null() {
                let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                ensure_init();
                unsafe {
                    LIBMTP_Release_Device(guard.device);
                }
                guard.device = ptr::null_mut();
            }
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let (device, display) = open_raw_by_id_locked(&id)?;
            guard.device = device;
            guard.device_id = Some(id);
            guard.display_name = Some(display);
            Ok(())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP open join: {e}")))?
    }

    async fn close(&mut self) -> Result<(), ProviderError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if !guard.device.is_null() {
                let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                ensure_init();
                unsafe {
                    LIBMTP_Release_Device(guard.device);
                }
                guard.device = ptr::null_mut();
            }
            guard.device_id = None;
            guard.display_name = None;
            Ok(())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP close join: {e}")))?
    }

    fn is_open(&self) -> bool {
        self.lock_inner()
            .map(|g| !g.device.is_null())
            .unwrap_or(false)
    }

    fn device_display_name(&self) -> Option<String> {
        self.lock_inner().ok().and_then(|g| g.display_name.clone())
    }

    async fn list_storages(&mut self) -> Result<Vec<MtpStorage>, ProviderError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if guard.device.is_null() {
                return Err(ProviderError::NotConnected);
            }
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            list_storages_on_locked(guard.device)
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP list_storages join: {e}")))?
    }

    async fn list_objects(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: Option<&str>,
    ) -> Result<Vec<MtpObject>, ProviderError> {
        let parent = parent.cloned();
        let storage_id = storage_id.map(|s| s.to_string());
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if guard.device.is_null() {
                return Err(ProviderError::NotConnected);
            }
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            list_objects_on_locked(guard.device, parent.as_ref(), storage_id.as_deref())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP list_objects join: {e}")))?
    }

    async fn get_object(
        &mut self,
        id: &MtpObjectId,
        dest: &Path,
        on_progress: Option<MtpProgress>,
    ) -> Result<(), ProviderError> {
        let handle = parse_handle(&id.handle)?;
        if id.handle.starts_with("storage:") {
            return Err(ProviderError::InvalidPath(
                "cannot download a storage root".into(),
            ));
        }
        let dest_c = CString::new(dest.to_string_lossy().as_bytes()).map_err(|_| {
            ProviderError::InvalidPath("destination path contains interior NUL".into())
        })?;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if guard.device.is_null() {
                return Err(ProviderError::NotConnected);
            }
            ensure_init();
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let rc = run_with_progress(on_progress, |cb, data| unsafe {
                LIBMTP_Get_File_To_File(guard.device, handle, dest_c.as_ptr(), cb, data)
            });
            if rc != 0 {
                let msg = device_error_message(guard.device);
                return Err(ProviderError::TransferFailed(format!(
                    "MTP download failed: {msg}"
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP get_object join: {e}")))?
    }

    async fn send_object(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        src: &Path,
        name: &str,
        on_progress: Option<MtpProgress>,
    ) -> Result<MtpObjectId, ProviderError> {
        let storage = parse_storage_id(storage_id)?;
        let parent_id = match parent {
            None => LIBMTP_FILES_AND_FOLDERS_ROOT,
            Some(p) if p.handle.starts_with("storage:") => LIBMTP_FILES_AND_FOLDERS_ROOT,
            Some(p) => parse_handle(&p.handle)?,
        };
        let meta = std::fs::metadata(src).map_err(ProviderError::IoError)?;
        let filesize = meta.len();
        let src_c = CString::new(src.to_string_lossy().as_bytes())
            .map_err(|_| ProviderError::InvalidPath("source path contains interior NUL".into()))?;
        let name_c = CString::new(name.as_bytes())
            .map_err(|_| ProviderError::InvalidPath("object name contains interior NUL".into()))?;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if guard.device.is_null() {
                return Err(ProviderError::NotConnected);
            }
            ensure_init();
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());

            let file = unsafe { LIBMTP_new_file_t() };
            if file.is_null() {
                return Err(ProviderError::TransferFailed(
                    "LIBMTP_new_file_t returned null".into(),
                ));
            }
            unsafe {
                (*file).filename = name_c.into_raw();
                (*file).filesize = filesize;
                (*file).filetype = LIBMTP_FILETYPE_UNKNOWN;
                (*file).parent_id = parent_id;
                (*file).storage_id = storage;
                (*file).next = ptr::null_mut();
            }

            let rc = run_with_progress(on_progress, |cb, data| unsafe {
                LIBMTP_Send_File_From_File(guard.device, src_c.as_ptr(), file, cb, data)
            });

            let result = if rc != 0 {
                let msg = device_error_message(guard.device);
                if msg.to_ascii_lowercase().contains("full")
                    || rc as i32 == LIBMTP_ERROR_STORAGE_FULL
                {
                    Err(ProviderError::TransferFailed(format!(
                        "MTP upload failed (storage full?): {msg}"
                    )))
                } else {
                    Err(ProviderError::TransferFailed(format!(
                        "MTP upload failed: {msg}"
                    )))
                }
            } else {
                let item_id = unsafe { (*file).item_id };
                Ok(MtpObjectId {
                    storage_id: storage.to_string(),
                    handle: item_id.to_string(),
                })
            };

            // destroy_file_t frees filename we handed over via into_raw.
            unsafe {
                LIBMTP_destroy_file_t(file);
            }
            result
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP send_object join: {e}")))?
    }

    async fn delete_object(&mut self, id: &MtpObjectId) -> Result<(), ProviderError> {
        if id.handle.starts_with("storage:") {
            return Err(ProviderError::NotSupported(
                "cannot delete a storage root".into(),
            ));
        }
        let handle = parse_handle(&id.handle)?;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if guard.device.is_null() {
                return Err(ProviderError::NotConnected);
            }
            ensure_init();
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let rc = unsafe { LIBMTP_Delete_Object(guard.device, handle) };
            if rc != 0 {
                let msg = device_error_message(guard.device);
                return Err(ProviderError::TransferFailed(format!(
                    "MTP delete failed: {msg}"
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP delete join: {e}")))?
    }

    async fn create_folder(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        name: &str,
    ) -> Result<MtpObjectId, ProviderError> {
        let storage = parse_storage_id(storage_id)?;
        let parent_id = match parent {
            None => LIBMTP_FILES_AND_FOLDERS_ROOT,
            Some(p) if p.handle.starts_with("storage:") => LIBMTP_FILES_AND_FOLDERS_ROOT,
            Some(p) => parse_handle(&p.handle)?,
        };
        let name_c = CString::new(name.as_bytes())
            .map_err(|_| ProviderError::InvalidPath("folder name contains interior NUL".into()))?;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if guard.device.is_null() {
                return Err(ProviderError::NotConnected);
            }
            ensure_init();
            let _g = LIBMTP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // Create_Folder takes non-const char* name; keep CString alive.
            let mut name_buf = name_c;
            let id = unsafe {
                LIBMTP_Create_Folder(
                    guard.device,
                    name_buf.as_ptr() as *mut c_char,
                    parent_id,
                    storage,
                )
            };
            // silence unused mut if as_ptr is enough
            let _ = &mut name_buf;
            if id == 0 {
                let msg = device_error_message(guard.device);
                return Err(ProviderError::TransferFailed(format!(
                    "MTP create folder failed: {msg}"
                )));
            }
            Ok(MtpObjectId {
                storage_id: storage.to_string(),
                handle: id.to_string(),
            })
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("MTP create_folder join: {e}")))?
    }
}

/// Whether this build linked libmtp.
pub fn libmtp_linked() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_device_id_roundtrip() {
        let (b, d) = parse_device_id("usb:1:12").unwrap();
        assert_eq!(b, 1);
        assert_eq!(d, 12);
        assert!(parse_device_id("nope").is_err());
        assert!(parse_device_id("usb:x:y").is_err());
    }

    #[tokio::test]
    async fn list_devices_does_not_panic() {
        let b = LinuxLibmtpBackend::new();
        // May be empty (no phone) or populated; must not panic.
        let result = b.list_devices().await;
        assert!(result.is_ok(), "list_devices err: {result:?}");
    }
}
