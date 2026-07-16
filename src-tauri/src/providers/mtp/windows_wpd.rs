//! Windows WPD (Portable Devices) backend for MTP portable devices.
//!
//! Phase 3 of APPENDIX-MTP / tracker #422. Maps the shared [`MtpBackend`]
//! surface to Win32 COM (`IPortableDeviceManager` / `IPortableDevice` /
//! content / resources). Device ids are opaque PnP strings.
//!
//! Hotplug: [`start_mtp_device_watcher`] emits the Tauri event
//! `mtp-devices-changed` on `WM_DEVICECHANGE` arrival/removal. It is a
//! dedicated wake path and does **not** touch lettered-volume mask logic
//! (`LAST_DRIVE_MASK` lives on `feat/windows-hotplug-volumes-changed`).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(windows)]

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use windows::core::{w, BSTR, HSTRING, PCWSTR, PROPVARIANT, PWSTR};
use windows::Win32::Devices::PortableDevices::{
    IEnumPortableDeviceObjectIDs, IPortableDevice, IPortableDeviceContent,
    IPortableDeviceKeyCollection, IPortableDeviceManager, IPortableDevicePropVariantCollection,
    IPortableDeviceProperties, IPortableDeviceResources, IPortableDeviceValues, PortableDeviceFTM,
    PortableDeviceKeyCollection, PortableDeviceManager, PortableDevicePropVariantCollection,
    PortableDeviceValues, PORTABLE_DEVICE_DELETE_NO_RECURSION, WPD_CLIENT_DESIRED_ACCESS,
    WPD_CLIENT_MAJOR_VERSION, WPD_CLIENT_MINOR_VERSION, WPD_CLIENT_NAME, WPD_CLIENT_REVISION,
    WPD_CONTENT_TYPE_FOLDER, WPD_CONTENT_TYPE_FUNCTIONAL_OBJECT, WPD_CONTENT_TYPE_GENERIC_FILE,
    WPD_FUNCTIONAL_CATEGORY_STORAGE, WPD_OBJECT_CONTENT_TYPE, WPD_OBJECT_NAME,
    WPD_OBJECT_ORIGINAL_FILE_NAME, WPD_OBJECT_PARENT_ID, WPD_OBJECT_SIZE, WPD_RESOURCE_DEFAULT,
    WPD_STORAGE_CAPACITY, WPD_STORAGE_FREE_SPACE_IN_BYTES,
};
use windows::Win32::Foundation::{
    GetLastError, GENERIC_READ, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IStream, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED, STGC_DEFAULT, STGM_READ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassExW,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, HMENU, MSG, WM_DEVICECHANGE, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::providers::mtp::backend::{
    MtpBackend, MtpDeviceInfo, MtpObject, MtpObjectId, MtpProgress, MtpStorage,
};
use crate::providers::types::ProviderError;

// ─── dbt.h constants (avoid extra DeviceIo features) ────────────────────────

const DBT_DEVICEARRIVAL: usize = 0x8000;
const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;
/// ERROR_CLASS_ALREADY_EXISTS (winerror.h)
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

// ─── COM init per worker thread ─────────────────────────────────────────────

fn ensure_com_thread() -> Result<ComGuard, ProviderError> {
    ComGuard::new()
}

struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    fn new() -> Result<Self, ProviderError> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK (0) or S_FALSE (1) = success; RPC_E_CHANGED_MODE (0x80010106) means
        // already initialized with a different model: still usable for our calls.
        let code = hr.0 as u32;
        if hr.is_ok() || code == 0x8001_0106 {
            Ok(Self {
                initialized: hr.is_ok(),
            })
        } else {
            Err(ProviderError::ConnectionFailed(format!(
                "CoInitializeEx failed: {hr}"
            )))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn map_com(err: windows::core::Error, ctx: &str) -> ProviderError {
    let msg = err.message();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("access") || lower.contains("denied") || lower.contains("busy") {
        ProviderError::ConnectionFailed(format!(
            "{ctx}: device busy or access denied ({msg}). Close other apps using the phone and retry."
        ))
    } else if lower.contains("not found") || lower.contains("removed") {
        ProviderError::NotFound(format!("{ctx}: {msg}"))
    } else {
        ProviderError::ConnectionFailed(format!("{ctx}: {msg}"))
    }
}

fn transfer_err(err: windows::core::Error, ctx: &str) -> ProviderError {
    ProviderError::TransferFailed(format!("{ctx}: {}", err.message()))
}

unsafe fn pwstr_to_string(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    p.to_string().ok().filter(|s| !s.is_empty())
}

unsafe fn free_pwstr(p: PWSTR) {
    if !p.is_null() {
        CoTaskMemFree(Some(p.0 as *const _));
    }
}

fn propvar_to_string(pv: &PROPVARIANT) -> Option<String> {
    BSTR::try_from(pv)
        .ok()
        .map(|b| b.to_string())
        .filter(|s| !s.is_empty())
}

fn create_values() -> Result<IPortableDeviceValues, ProviderError> {
    unsafe {
        CoCreateInstance(&PortableDeviceValues, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| map_com(e, "create IPortableDeviceValues"))
    }
}

fn create_key_collection() -> Result<IPortableDeviceKeyCollection, ProviderError> {
    unsafe {
        CoCreateInstance(&PortableDeviceKeyCollection, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| map_com(e, "create IPortableDeviceKeyCollection"))
    }
}

fn create_propvar_collection() -> Result<IPortableDevicePropVariantCollection, ProviderError> {
    unsafe {
        CoCreateInstance(
            &PortableDevicePropVariantCollection,
            None,
            CLSCTX_INPROC_SERVER,
        )
        .map_err(|e| map_com(e, "create IPortableDevicePropVariantCollection"))
    }
}

fn create_manager() -> Result<IPortableDeviceManager, ProviderError> {
    unsafe {
        CoCreateInstance(&PortableDeviceManager, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| map_com(e, "create IPortableDeviceManager"))
    }
}

fn create_device() -> Result<IPortableDevice, ProviderError> {
    // FTM (free-threaded marshaler) form is safer across spawn_blocking threads.
    unsafe {
        CoCreateInstance(&PortableDeviceFTM, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| map_com(e, "create IPortableDevice"))
    }
}

fn client_info_values() -> Result<IPortableDeviceValues, ProviderError> {
    let values = create_values()?;
    unsafe {
        values
            .SetStringValue(&WPD_CLIENT_NAME, w!("AeroFTP"))
            .map_err(|e| map_com(e, "WPD_CLIENT_NAME"))?;
        values
            .SetUnsignedIntegerValue(&WPD_CLIENT_MAJOR_VERSION, 1)
            .map_err(|e| map_com(e, "WPD_CLIENT_MAJOR_VERSION"))?;
        values
            .SetUnsignedIntegerValue(&WPD_CLIENT_MINOR_VERSION, 0)
            .map_err(|e| map_com(e, "WPD_CLIENT_MINOR_VERSION"))?;
        values
            .SetUnsignedIntegerValue(&WPD_CLIENT_REVISION, 0)
            .map_err(|e| map_com(e, "WPD_CLIENT_REVISION"))?;
        // GENERIC_READ: enough for browse + whole-file get; put may need write
        // on some devices but Open with read has been sufficient for CreateObject
        // on modern phones; if put fails with access, reopen with write later.
        let _ = values.SetUnsignedIntegerValue(&WPD_CLIENT_DESIRED_ACCESS, GENERIC_READ.0);
    }
    Ok(values)
}

fn device_string_prop(
    manager: &IPortableDeviceManager,
    pnp_id: &HSTRING,
    getter: unsafe fn(
        &IPortableDeviceManager,
        PCWSTR,
        PWSTR,
        *mut u32,
    ) -> windows::core::Result<()>,
) -> Option<String> {
    unsafe {
        let mut len = 0u32;
        let _ = getter(manager, PCWSTR(pnp_id.as_ptr()), PWSTR::null(), &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if getter(
            manager,
            PCWSTR(pnp_id.as_ptr()),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_err()
        {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..end]);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

fn list_pnp_devices_locked() -> Result<Vec<MtpDeviceInfo>, ProviderError> {
    let manager = create_manager()?;
    unsafe {
        let _ = manager.RefreshDeviceList();
        let mut count = 0u32;
        manager
            .GetDevices(std::ptr::null_mut(), &mut count)
            .map_err(|e| map_com(e, "GetDevices count"))?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut ids: Vec<PWSTR> = vec![PWSTR::null(); count as usize];
        manager
            .GetDevices(ids.as_mut_ptr(), &mut count)
            .map_err(|e| {
                for p in &ids {
                    free_pwstr(*p);
                }
                map_com(e, "GetDevices")
            })?;

        let mut out = Vec::with_capacity(count as usize);
        for p in ids.into_iter().take(count as usize) {
            let id_str = match pwstr_to_string(p) {
                Some(s) => s,
                None => {
                    free_pwstr(p);
                    continue;
                }
            };
            free_pwstr(p);
            let h_id = HSTRING::from(id_str.as_str());
            let friendly = device_string_prop(&manager, &h_id, |m, id, buf, len| {
                m.GetDeviceFriendlyName(id, buf, len)
            });
            let description = device_string_prop(&manager, &h_id, |m, id, buf, len| {
                m.GetDeviceDescription(id, buf, len)
            });
            let manufacturer = device_string_prop(&manager, &h_id, |m, id, buf, len| {
                m.GetDeviceManufacturer(id, buf, len)
            });
            let display = friendly
                .or_else(|| match (manufacturer.as_deref(), description.as_deref()) {
                    (Some(m), Some(d)) => Some(format!("{m} {d}")),
                    (None, Some(d)) => Some(d.to_string()),
                    (Some(m), None) => Some(m.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| "Portable device".to_string());

            out.push(MtpDeviceInfo {
                device_id: id_str,
                display_name: display,
                // WPD serial/vid/pid property mapping is a peer-station follow-up.
                serial: None,
                vendor_id: None,
                product_id: None,
                bus_location: None,
                platform: "windows-wpd".into(),
                storages_hint: 0,
            });
        }
        Ok(out)
    }
}

fn open_device_locked(device_id: &str) -> Result<(IPortableDevice, String), ProviderError> {
    if device_id.trim().is_empty() {
        return Err(ProviderError::InvalidConfig(
            "MTP device_id is required".into(),
        ));
    }
    // Verify the PnP id is still present (honest NotFound if unplugged).
    let listed = list_pnp_devices_locked()?;
    if !listed.iter().any(|d| d.device_id == device_id) {
        return Err(ProviderError::NotFound(format!(
            "MTP device not found (unplugged or not in file-transfer mode): {device_id}"
        )));
    }
    let display = listed
        .into_iter()
        .find(|d| d.device_id == device_id)
        .map(|d| d.display_name)
        .unwrap_or_else(|| device_id.to_string());

    let device = create_device()?;
    let client = client_info_values()?;
    let h_id = HSTRING::from(device_id);
    unsafe {
        device
            .Open(&h_id, &client)
            .map_err(|e| map_com(e, "IPortableDevice::Open"))?;
    }
    Ok((device, display))
}

fn content_of(device: &IPortableDevice) -> Result<IPortableDeviceContent, ProviderError> {
    unsafe {
        device
            .Content()
            .map_err(|e| map_com(e, "IPortableDevice::Content"))
    }
}

fn properties_of(
    content: &IPortableDeviceContent,
) -> Result<IPortableDeviceProperties, ProviderError> {
    unsafe {
        content
            .Properties()
            .map_err(|e| map_com(e, "IPortableDeviceContent::Properties"))
    }
}

fn get_string_prop(
    props: &IPortableDeviceProperties,
    object_id: &HSTRING,
    key: &PROPERTYKEY,
) -> Option<String> {
    let keys = create_key_collection().ok()?;
    unsafe {
        keys.Add(key).ok()?;
        let values = props.GetValues(object_id, &keys).ok()?;
        let pw = values.GetStringValue(key).ok()?;
        let s = pwstr_to_string(pw);
        free_pwstr(pw);
        s
    }
}

fn get_u64_prop(
    props: &IPortableDeviceProperties,
    object_id: &HSTRING,
    key: &PROPERTYKEY,
) -> Option<u64> {
    let keys = create_key_collection().ok()?;
    unsafe {
        keys.Add(key).ok()?;
        let values = props.GetValues(object_id, &keys).ok()?;
        values.GetUnsignedLargeIntegerValue(key).ok()
    }
}

fn get_guid_prop(
    props: &IPortableDeviceProperties,
    object_id: &HSTRING,
    key: &PROPERTYKEY,
) -> Option<windows::core::GUID> {
    let keys = create_key_collection().ok()?;
    unsafe {
        keys.Add(key).ok()?;
        let values = props.GetValues(object_id, &keys).ok()?;
        values.GetGuidValue(key).ok()
    }
}

fn list_storages_on(device: &IPortableDevice) -> Result<Vec<MtpStorage>, ProviderError> {
    let caps = unsafe {
        device
            .Capabilities()
            .map_err(|e| map_com(e, "IPortableDevice::Capabilities"))?
    };
    let objects = unsafe {
        caps.GetFunctionalObjects(&WPD_FUNCTIONAL_CATEGORY_STORAGE)
            .map_err(|e| map_com(e, "GetFunctionalObjects STORAGE"))?
    };
    let content = content_of(device)?;
    let props = properties_of(&content)?;

    let mut count = 0u32;
    unsafe {
        objects
            .GetCount(&count)
            .map_err(|e| map_com(e, "storage collection GetCount"))?;
    }

    let mut out = Vec::new();
    for i in 0..count {
        let mut pv = PROPVARIANT::new();
        if unsafe { objects.GetAt(i, &pv) }.is_err() {
            continue;
        }
        let Some(storage_id) = propvar_to_string(&pv) else {
            continue;
        };
        let h_id = HSTRING::from(storage_id.as_str());
        let name = get_string_prop(&props, &h_id, &WPD_OBJECT_NAME)
            .unwrap_or_else(|| format!("Storage {storage_id}"));
        let total = get_u64_prop(&props, &h_id, &WPD_STORAGE_CAPACITY);
        let free = get_u64_prop(&props, &h_id, &WPD_STORAGE_FREE_SPACE_IN_BYTES);
        out.push(MtpStorage {
            storage_id,
            display_name: name,
            total_bytes: total.filter(|&n| n > 0),
            free_bytes: free,
        });
    }
    Ok(out)
}

fn parent_object_id(
    parent: Option<&MtpObjectId>,
    storage_id: Option<&str>,
) -> Result<String, ProviderError> {
    match parent {
        None => {
            let sid = storage_id.ok_or_else(|| {
                ProviderError::InvalidPath(
                    "list_objects requires storage_id at storage root".into(),
                )
            })?;
            Ok(sid.to_string())
        }
        Some(p) if p.handle.starts_with("storage:") => {
            // Compatibility with Linux-style storage: handles; prefer storage_id field.
            Ok(p.storage_id.clone())
        }
        Some(p) => Ok(p.handle.clone()),
    }
}

fn list_objects_on(
    device: &IPortableDevice,
    parent: Option<&MtpObjectId>,
    storage_id: Option<&str>,
) -> Result<Vec<MtpObject>, ProviderError> {
    let parent_id = parent_object_id(parent, storage_id)?;
    let storage_for_children = parent
        .map(|p| p.storage_id.clone())
        .or_else(|| storage_id.map(|s| s.to_string()))
        .unwrap_or_else(|| parent_id.clone());

    let content = content_of(device)?;
    let props = properties_of(&content)?;
    let h_parent = HSTRING::from(parent_id.as_str());

    let enumerator: IEnumPortableDeviceObjectIDs = unsafe {
        content
            .EnumObjects(0, &h_parent, None)
            .map_err(|e| map_com(e, "EnumObjects"))?
    };

    let mut out = Vec::new();
    loop {
        let mut batch = [PWSTR::null(); 32];
        let mut fetched = 0u32;
        let hr = unsafe { enumerator.Next(&mut batch, &mut fetched) };
        if fetched == 0 {
            break;
        }
        // S_OK / S_FALSE both fine when fetched > 0
        let _ = hr;
        for slot in batch.iter().take(fetched as usize) {
            let obj_id = unsafe { pwstr_to_string(*slot) };
            unsafe { free_pwstr(*slot) };
            let Some(obj_id) = obj_id else { continue };
            let h_obj = HSTRING::from(obj_id.as_str());
            let name = get_string_prop(&props, &h_obj, &WPD_OBJECT_ORIGINAL_FILE_NAME)
                .or_else(|| get_string_prop(&props, &h_obj, &WPD_OBJECT_NAME))
                .unwrap_or_else(|| obj_id.clone());
            let content_type = get_guid_prop(&props, &h_obj, &WPD_OBJECT_CONTENT_TYPE);
            let is_dir = content_type
                .map(|g| g == WPD_CONTENT_TYPE_FOLDER || g == WPD_CONTENT_TYPE_FUNCTIONAL_OBJECT)
                .unwrap_or(false);
            let size = if is_dir {
                0
            } else {
                get_u64_prop(&props, &h_obj, &WPD_OBJECT_SIZE).unwrap_or(0)
            };
            out.push(MtpObject {
                id: MtpObjectId {
                    storage_id: storage_for_children.clone(),
                    handle: obj_id,
                },
                name,
                is_dir,
                size,
                modified: None,
            });
        }
        if fetched < batch.len() as u32 {
            break;
        }
    }
    Ok(out)
}

fn copy_stream_to_file(
    stream: &IStream,
    dest: &Path,
    total_hint: u64,
    on_progress: Option<&MtpProgress>,
) -> Result<(), ProviderError> {
    let mut file = File::create(dest).map_err(ProviderError::IoError)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut transferred = 0u64;
    loop {
        let mut read = 0u32;
        let hr = unsafe {
            stream.Read(
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                Some(&mut read),
            )
        };
        if read == 0 {
            // S_FALSE or S_OK with 0 = EOF
            let _ = hr;
            break;
        }
        if hr.is_err() {
            return Err(ProviderError::TransferFailed(format!(
                "IStream::Read failed: {hr}"
            )));
        }
        file.write_all(&buf[..read as usize])
            .map_err(ProviderError::IoError)?;
        transferred += read as u64;
        if let Some(cb) = on_progress {
            cb(transferred, total_hint);
        }
    }
    file.flush().map_err(ProviderError::IoError)?;
    Ok(())
}

fn copy_file_to_stream(
    src: &Path,
    stream: &IStream,
    on_progress: Option<&MtpProgress>,
) -> Result<(), ProviderError> {
    let meta = std::fs::metadata(src).map_err(ProviderError::IoError)?;
    let total = meta.len();
    let mut file = File::open(src).map_err(ProviderError::IoError)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut transferred = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(ProviderError::IoError)?;
        if n == 0 {
            break;
        }
        let mut written = 0u32;
        let hr = unsafe { stream.Write(buf.as_ptr() as *const _, n as u32, Some(&mut written)) };
        if hr.is_err() || written as usize != n {
            return Err(ProviderError::TransferFailed(format!(
                "IStream::Write failed (wrote {written}/{n}): {hr}"
            )));
        }
        transferred += written as u64;
        if let Some(cb) = on_progress {
            cb(transferred, total);
        }
    }
    unsafe {
        stream
            .Commit(STGC_DEFAULT)
            .map_err(|e| transfer_err(e, "IStream::Commit"))?;
    }
    Ok(())
}

fn get_object_on(
    device: &IPortableDevice,
    id: &MtpObjectId,
    dest: &Path,
    on_progress: Option<MtpProgress>,
) -> Result<(), ProviderError> {
    if id.handle.starts_with("storage:") {
        return Err(ProviderError::InvalidPath(
            "cannot download a storage root".into(),
        ));
    }
    let content = content_of(device)?;
    let resources: IPortableDeviceResources = unsafe {
        content
            .Transfer()
            .map_err(|e| map_com(e, "IPortableDeviceContent::Transfer"))?
    };
    let props = properties_of(&content)?;
    let h_obj = HSTRING::from(id.handle.as_str());
    let total_hint = get_u64_prop(&props, &h_obj, &WPD_OBJECT_SIZE).unwrap_or(0);

    let mut optimal = 0u32;
    let mut stream: Option<IStream> = None;
    unsafe {
        resources
            .GetStream(
                &h_obj,
                &WPD_RESOURCE_DEFAULT,
                STGM_READ.0,
                &mut optimal,
                &mut stream,
            )
            .map_err(|e| transfer_err(e, "GetStream"))?;
    }
    let stream = stream
        .ok_or_else(|| ProviderError::TransferFailed("GetStream returned null IStream".into()))?;
    copy_stream_to_file(&stream, dest, total_hint, on_progress.as_ref())
}

fn send_object_on(
    device: &IPortableDevice,
    parent: Option<&MtpObjectId>,
    storage_id: &str,
    src: &Path,
    name: &str,
    on_progress: Option<MtpProgress>,
) -> Result<MtpObjectId, ProviderError> {
    let parent_id = match parent {
        None => storage_id.to_string(),
        Some(p) if p.handle.starts_with("storage:") => p.storage_id.clone(),
        Some(p) => p.handle.clone(),
    };
    let meta = std::fs::metadata(src).map_err(ProviderError::IoError)?;
    let filesize = meta.len();

    let values = create_values()?;
    unsafe {
        values
            .SetStringValue(&WPD_OBJECT_PARENT_ID, &HSTRING::from(parent_id.as_str()))
            .map_err(|e| map_com(e, "WPD_OBJECT_PARENT_ID"))?;
        values
            .SetStringValue(&WPD_OBJECT_NAME, &HSTRING::from(name))
            .map_err(|e| map_com(e, "WPD_OBJECT_NAME"))?;
        values
            .SetStringValue(&WPD_OBJECT_ORIGINAL_FILE_NAME, &HSTRING::from(name))
            .map_err(|e| map_com(e, "WPD_OBJECT_ORIGINAL_FILE_NAME"))?;
        values
            .SetGuidValue(&WPD_OBJECT_CONTENT_TYPE, &WPD_CONTENT_TYPE_GENERIC_FILE)
            .map_err(|e| map_com(e, "WPD_OBJECT_CONTENT_TYPE"))?;
        values
            .SetUnsignedLargeIntegerValue(&WPD_OBJECT_SIZE, filesize)
            .map_err(|e| map_com(e, "WPD_OBJECT_SIZE"))?;
    }

    let content = content_of(device)?;
    let mut stream: Option<IStream> = None;
    let mut optimal = 0u32;
    let mut cookie = PWSTR::null();
    unsafe {
        content
            .CreateObjectWithPropertiesAndData(&values, &mut stream, &mut optimal, &mut cookie)
            .map_err(|e| transfer_err(e, "CreateObjectWithPropertiesAndData"))?;
    }
    // cookie is optional; free if present
    unsafe { free_pwstr(cookie) };

    let stream = stream.ok_or_else(|| {
        ProviderError::TransferFailed("CreateObject returned null IStream".into())
    })?;
    copy_file_to_stream(src, &stream, on_progress.as_ref())?;

    // Best-effort: recover new object id via data stream if available; else
    // re-list parent for matching name (slow but correct).
    let new_id = match recover_created_object_id(&content, &parent_id, name) {
        Ok(id) => id,
        Err(_) => format!("created:{name}"),
    };

    Ok(MtpObjectId {
        storage_id: storage_id.to_string(),
        handle: new_id,
    })
}

fn recover_created_object_id(
    content: &IPortableDeviceContent,
    parent_id: &str,
    name: &str,
) -> Result<String, ProviderError> {
    let h_parent = HSTRING::from(parent_id);
    let enumerator = unsafe {
        content
            .EnumObjects(0, &h_parent, None)
            .map_err(|e| map_com(e, "EnumObjects after put"))?
    };
    let props = properties_of(content)?;
    loop {
        let mut batch = [PWSTR::null(); 32];
        let mut fetched = 0u32;
        let hr = unsafe { enumerator.Next(&mut batch, &mut fetched) };
        if fetched == 0 {
            break;
        }
        let _ = hr;
        for slot in batch.iter().take(fetched as usize) {
            let obj_id = unsafe { pwstr_to_string(*slot) };
            unsafe { free_pwstr(*slot) };
            let Some(obj_id) = obj_id else { continue };
            let h_obj = HSTRING::from(obj_id.as_str());
            let n = get_string_prop(&props, &h_obj, &WPD_OBJECT_ORIGINAL_FILE_NAME)
                .or_else(|| get_string_prop(&props, &h_obj, &WPD_OBJECT_NAME));
            if n.as_deref() == Some(name) {
                return Ok(obj_id);
            }
        }
        if fetched < batch.len() as u32 {
            break;
        }
    }
    Err(ProviderError::NotFound(format!(
        "created object {name} not found under {parent_id}"
    )))
}

fn delete_object_on(device: &IPortableDevice, id: &MtpObjectId) -> Result<(), ProviderError> {
    if id.handle.starts_with("storage:") {
        return Err(ProviderError::NotSupported(
            "cannot delete a storage root".into(),
        ));
    }
    let content = content_of(device)?;
    let coll = create_propvar_collection()?;
    let pv = PROPVARIANT::from(id.handle.as_str());
    unsafe {
        coll.Add(&pv)
            .map_err(|e| map_com(e, "delete collection Add"))?;
        let mut results: Option<IPortableDevicePropVariantCollection> = None;
        content
            .Delete(
                PORTABLE_DEVICE_DELETE_NO_RECURSION.0 as u32,
                &coll,
                &mut results,
            )
            .map_err(|e| map_com(e, "IPortableDeviceContent::Delete"))?;
    }
    Ok(())
}

fn create_folder_on(
    device: &IPortableDevice,
    parent: Option<&MtpObjectId>,
    storage_id: &str,
    name: &str,
) -> Result<MtpObjectId, ProviderError> {
    let parent_id = match parent {
        None => storage_id.to_string(),
        Some(p) if p.handle.starts_with("storage:") => p.storage_id.clone(),
        Some(p) => p.handle.clone(),
    };
    let values = create_values()?;
    unsafe {
        values
            .SetStringValue(&WPD_OBJECT_PARENT_ID, &HSTRING::from(parent_id.as_str()))
            .map_err(|e| map_com(e, "WPD_OBJECT_PARENT_ID"))?;
        values
            .SetStringValue(&WPD_OBJECT_NAME, &HSTRING::from(name))
            .map_err(|e| map_com(e, "WPD_OBJECT_NAME"))?;
        values
            .SetStringValue(&WPD_OBJECT_ORIGINAL_FILE_NAME, &HSTRING::from(name))
            .map_err(|e| map_com(e, "WPD_OBJECT_ORIGINAL_FILE_NAME"))?;
        values
            .SetGuidValue(&WPD_OBJECT_CONTENT_TYPE, &WPD_CONTENT_TYPE_FOLDER)
            .map_err(|e| map_com(e, "WPD_OBJECT_CONTENT_TYPE folder"))?;
    }
    let content = content_of(device)?;
    let mut new_id = PWSTR::null();
    unsafe {
        content
            .CreateObjectWithPropertiesOnly(&values, &mut new_id)
            .map_err(|e| map_com(e, "CreateObjectWithPropertiesOnly"))?;
    }
    let handle = unsafe { pwstr_to_string(new_id) }.unwrap_or_else(|| name.to_string());
    unsafe { free_pwstr(new_id) };
    Ok(MtpObjectId {
        storage_id: storage_id.to_string(),
        handle,
    })
}

// ─── Backend ────────────────────────────────────────────────────────────────

struct Inner {
    device: Option<IPortableDevice>,
    device_id: Option<String>,
    display_name: Option<String>,
}

/// WPD-backed [`MtpBackend`] for Windows.
pub struct WindowsWpdBackend {
    inner: Arc<Mutex<Inner>>,
}

impl WindowsWpdBackend {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                device: None,
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

impl Default for WindowsWpdBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MtpBackend for WindowsWpdBackend {
    async fn list_devices(&self) -> Result<Vec<MtpDeviceInfo>, ProviderError> {
        tokio::task::spawn_blocking(|| {
            let _com = ensure_com_thread()?;
            list_pnp_devices_locked()
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD list_devices join: {e}")))?
    }

    async fn open(&mut self, device_id: &str) -> Result<(), ProviderError> {
        let id = device_id.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _com = ensure_com_thread()?;
            let mut guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if let Some(dev) = guard.device.take() {
                let _ = unsafe { dev.Close() };
            }
            let (device, display) = open_device_locked(&id)?;
            guard.device = Some(device);
            guard.device_id = Some(id);
            guard.display_name = Some(display);
            Ok(())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD open join: {e}")))?
    }

    async fn close(&mut self) -> Result<(), ProviderError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _com = ensure_com_thread()?;
            let mut guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            if let Some(dev) = guard.device.take() {
                let _ = unsafe { dev.Close() };
            }
            guard.device_id = None;
            guard.display_name = None;
            Ok(())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD close join: {e}")))?
    }

    fn is_open(&self) -> bool {
        self.lock_inner()
            .map(|g| g.device.is_some())
            .unwrap_or(false)
    }

    fn device_display_name(&self) -> Option<String> {
        self.lock_inner().ok().and_then(|g| g.display_name.clone())
    }

    async fn list_storages(&mut self) -> Result<Vec<MtpStorage>, ProviderError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _com = ensure_com_thread()?;
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            let device = guard.device.as_ref().ok_or(ProviderError::NotConnected)?;
            list_storages_on(device)
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD list_storages join: {e}")))?
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
            let _com = ensure_com_thread()?;
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            let device = guard.device.as_ref().ok_or(ProviderError::NotConnected)?;
            list_objects_on(device, parent.as_ref(), storage_id.as_deref())
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD list_objects join: {e}")))?
    }

    async fn get_object(
        &mut self,
        id: &MtpObjectId,
        dest: &Path,
        on_progress: Option<MtpProgress>,
    ) -> Result<(), ProviderError> {
        let id = id.clone();
        let dest = dest.to_path_buf();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _com = ensure_com_thread()?;
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            let device = guard.device.as_ref().ok_or(ProviderError::NotConnected)?;
            get_object_on(device, &id, &dest, on_progress)
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD get_object join: {e}")))?
    }

    async fn send_object(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        src: &Path,
        name: &str,
        on_progress: Option<MtpProgress>,
    ) -> Result<MtpObjectId, ProviderError> {
        let parent = parent.cloned();
        let storage_id = storage_id.to_string();
        let src = src.to_path_buf();
        let name = name.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _com = ensure_com_thread()?;
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            let device = guard.device.as_ref().ok_or(ProviderError::NotConnected)?;
            send_object_on(
                device,
                parent.as_ref(),
                &storage_id,
                &src,
                &name,
                on_progress,
            )
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD send_object join: {e}")))?
    }

    async fn delete_object(&mut self, id: &MtpObjectId) -> Result<(), ProviderError> {
        let id = id.clone();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _com = ensure_com_thread()?;
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            let device = guard.device.as_ref().ok_or(ProviderError::NotConnected)?;
            delete_object_on(device, &id)
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD delete_object join: {e}")))?
    }

    async fn create_folder(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        name: &str,
    ) -> Result<MtpObjectId, ProviderError> {
        let parent = parent.cloned();
        let storage_id = storage_id.to_string();
        let name = name.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _com = ensure_com_thread()?;
            let guard = inner
                .lock()
                .map_err(|_| ProviderError::TransferFailed("MTP session lock poisoned".into()))?;
            let device = guard.device.as_ref().ok_or(ProviderError::NotConnected)?;
            create_folder_on(device, parent.as_ref(), &storage_id, &name)
        })
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("WPD create_folder join: {e}")))?
    }
}

// ─── Hotplug wake: mtp-devices-changed ──────────────────────────────────────

static MTP_WATCHER_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();
static MTP_WATCHER_DEBOUNCE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn schedule_mtp_devices_changed() {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;

    if MTP_WATCHER_DEBOUNCE.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("mtp-watcher-debounce".to_string())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            MTP_WATCHER_DEBOUNCE.store(false, Ordering::SeqCst);
            if let Some(app) = MTP_WATCHER_APP.get() {
                let _ = app.emit("mtp-devices-changed", ());
            }
        })
        .ok();
}

/// WndProc: wake PLACES on device arrival/removal.
/// Does **not** emit `volumes-changed` and does not touch drive-letter masks.
/// Discovery still goes through `IPortableDeviceManager` (filters non-WPD).
unsafe extern "system" fn mtp_watcher_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let _ = lparam;
    if msg == WM_DEVICECHANGE {
        let event = wparam.0;
        if event == DBT_DEVICEARRIVAL || event == DBT_DEVICEREMOVECOMPLETE {
            schedule_mtp_devices_changed();
        }
        return LRESULT(1);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Start a background thread with a hidden tool window that receives
/// `WM_DEVICECHANGE` and emits debounced `mtp-devices-changed`.
///
/// Intentionally separate from lettered-volume `volumes-changed` /
/// `LAST_DRIVE_MASK` so the hotplug volumes branch can merge without conflict.
pub fn start_mtp_device_watcher(app_handle: tauri::AppHandle) {
    if MTP_WATCHER_APP.set(app_handle).is_err() {
        tracing::warn!("mtp-watcher: Windows watcher already started");
        return;
    }

    std::thread::Builder::new()
        .name("mtp-device-watcher".to_string())
        .spawn(move || {
            let hinstance: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
                Ok(h) => h.into(),
                Err(e) => {
                    tracing::warn!(
                        "mtp-watcher: GetModuleHandleW failed: {}; FE poll remains",
                        e
                    );
                    return;
                }
            };

            let class_name = w!("AeroFTPMtpDeviceWatcher");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(mtp_watcher_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: Default::default(),
                hCursor: Default::default(),
                hbrBackground: Default::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: class_name,
                hIconSm: Default::default(),
            };

            let atom = unsafe { RegisterClassExW(&wc) };
            if atom == 0 {
                let err = unsafe { GetLastError() };
                if err.0 != ERROR_CLASS_ALREADY_EXISTS {
                    tracing::warn!(
                        "mtp-watcher: RegisterClassExW failed (win32 {}); FE poll remains",
                        err.0
                    );
                    return;
                }
            }

            let hwnd = match unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class_name,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    HWND::default(),
                    HMENU::default(),
                    hinstance,
                    None,
                )
            } {
                Ok(h) if !h.is_invalid() => h,
                Ok(_) => {
                    tracing::warn!("mtp-watcher: CreateWindowExW returned null HWND");
                    return;
                }
                Err(e) => {
                    tracing::warn!("mtp-watcher: CreateWindowExW failed: {}", e);
                    return;
                }
            };
            let _hwnd = hwnd;
            tracing::info!("mtp-watcher: Windows WM_DEVICECHANGE -> mtp-devices-changed started");

            let mut msg = MSG::default();
            loop {
                let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if ret.0 == 0 {
                    break;
                }
                if ret.0 == -1 {
                    tracing::warn!("mtp-watcher: GetMessageW error; exiting");
                    break;
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            tracing::info!("mtp-watcher thread exited");
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_constructible() {
        let b = WindowsWpdBackend::new();
        assert!(!b.is_open());
    }

    #[tokio::test]
    async fn list_devices_does_not_panic() {
        let b = WindowsWpdBackend::new();
        // May return empty or real devices; must not panic.
        let result = b.list_devices().await;
        assert!(result.is_ok(), "{result:?}");
    }
}
