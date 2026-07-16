//! Linux hotplug wake for portable devices (APPENDIX-DEVICE-PROFILES Task 1).
//!
//! Emits the Tauri event `mtp-devices-changed` when a USB device appears or
//! disappears, so a My Servers device card flips red on unplug within a second
//! instead of waiting for the frontend fallback poll.
//!
//! Why netlink and not a `list_mtp_devices` poll: that command runs
//! `LIBMTP_Detect_Raw_Devices`, which touches the USB bus and takes
//! `LIBMTP_LOCK`. Polling it every few seconds would contend with the very
//! open/claim path this appendix is trying to stabilise. The kernel uevent
//! socket costs nothing while idle and never touches the bus.
//!
//! Fallback: if the netlink socket cannot be opened or bound (unusual, but
//! possible under a restrictive sandbox), a sysfs scan thread takes over. It
//! only reads `/sys/bus/usb/devices`, so it also never touches the bus.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

static MTP_WATCHER_APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static MTP_WATCHER_DEBOUNCE: AtomicBool = AtomicBool::new(false);

/// Coalesce the burst of uevents one physical plug emits (interfaces, endpoints)
/// into a single frontend wake. Mirrors the Windows watcher's 300ms debounce.
const DEBOUNCE_MS: u64 = 300;
/// Fallback-only: sysfs scan cadence when netlink is unavailable.
const SYSFS_POLL_MS: u64 = 2_000;

fn schedule_mtp_devices_changed() {
    use tauri::Emitter;

    if MTP_WATCHER_DEBOUNCE.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("mtp-hotplug-emit".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_millis(DEBOUNCE_MS));
            MTP_WATCHER_DEBOUNCE.store(false, Ordering::SeqCst);
            if let Some(app) = MTP_WATCHER_APP.get() {
                let _ = app.emit("mtp-devices-changed", ());
            }
        })
        .ok();
}

/// Kernel uevent netlink socket, bound to the kernel (1) and udev (2) groups.
///
/// `NETLINK_KOBJECT_UEVENT` is created by the kernel with `NL_CFG_F_NONROOT_RECV`,
/// so an unprivileged bind to these groups succeeds (same path `udevadm monitor`
/// uses as a normal user).
fn open_uevent_socket() -> Option<i32> {
    // SAFETY: plain socket/bind on a netlink address we fully initialise; the fd
    // is closed on every failure path and otherwise owned by the reader thread.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::NETLINK_KOBJECT_UEVENT,
        );
        if fd < 0 {
            return None;
        }
        let mut addr: libc::sockaddr_nl = std::mem::zeroed();
        addr.nl_family = libc::AF_NETLINK as u16;
        addr.nl_pid = 0; // let the kernel assign a unique port id
        addr.nl_groups = 1 | 2; // kernel uevents + udev-processed events
        let rc = libc::bind(
            fd,
            &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        );
        if rc < 0 {
            libc::close(fd);
            return None;
        }
        Some(fd)
    }
}

/// True when a uevent payload is a USB device add/remove worth re-listing for.
///
/// Payloads are NUL-separated `KEY=value` records (the udev-group variant adds a
/// binary header first), so a record scan works for both formats.
fn is_usb_hotplug_event(payload: &[u8]) -> bool {
    let mut usb = false;
    let mut hotplug = false;
    for record in payload.split(|b| *b == 0) {
        let Ok(text) = std::str::from_utf8(record) else {
            continue;
        };
        match text {
            "SUBSYSTEM=usb" => usb = true,
            "ACTION=add" | "ACTION=remove" | "ACTION=bind" | "ACTION=unbind" => hotplug = true,
            _ => {}
        }
    }
    usb && hotplug
}

fn spawn_netlink_watcher(fd: i32) {
    std::thread::Builder::new()
        .name("mtp-hotplug-netlink".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                // SAFETY: recv into a buffer we own; fd stays valid for the loop.
                let n =
                    unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    log::warn!(
                        "MTP hotplug: netlink recv failed ({err}); falling back to sysfs poll"
                    );
                    // SAFETY: fd is not used after this point on this path.
                    unsafe { libc::close(fd) };
                    spawn_sysfs_watcher();
                    return;
                }
                if is_usb_hotplug_event(&buf[..n as usize]) {
                    schedule_mtp_devices_changed();
                }
            }
        })
        .ok();
}

/// Signature of every USB device currently on the bus, read from sysfs only.
fn usb_device_signature() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(dir) = std::fs::read_dir("/sys/bus/usb/devices") else {
        return out;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let read = |name: &str| {
            std::fs::read_to_string(path.join(name))
                .ok()
                .map(|s| s.trim().to_string())
        };
        // Only real devices carry idVendor/idProduct; interfaces do not.
        if let (Some(vid), Some(pid), Some(busnum), Some(devnum)) = (
            read("idVendor"),
            read("idProduct"),
            read("busnum"),
            read("devnum"),
        ) {
            out.insert(format!("{busnum}:{devnum}:{vid}:{pid}"));
        }
    }
    out
}

fn spawn_sysfs_watcher() {
    std::thread::Builder::new()
        .name("mtp-hotplug-sysfs".into())
        .spawn(|| {
            let mut last = usb_device_signature();
            loop {
                std::thread::sleep(Duration::from_millis(SYSFS_POLL_MS));
                let now = usb_device_signature();
                if now != last {
                    last = now;
                    schedule_mtp_devices_changed();
                }
            }
        })
        .ok();
}

/// Start the Linux portable-device hotplug wake. Idempotent.
pub fn start_mtp_device_watcher(app_handle: tauri::AppHandle) {
    if MTP_WATCHER_APP.set(app_handle).is_err() {
        return; // already started
    }
    match open_uevent_socket() {
        Some(fd) => {
            log::info!("MTP hotplug: watching kernel uevents for USB add/remove");
            spawn_netlink_watcher(fd);
        }
        None => {
            log::info!(
                "MTP hotplug: netlink uevent socket unavailable; using {SYSFS_POLL_MS}ms sysfs scan"
            );
            spawn_sysfs_watcher();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(records: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in records {
            out.extend_from_slice(r.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn usb_add_is_a_hotplug_event() {
        assert!(is_usb_hotplug_event(&payload(&[
            "add@/devices/pci0000:00/usb1/1-2",
            "ACTION=add",
            "SUBSYSTEM=usb",
            "PRODUCT=fce/20d/100",
        ])));
    }

    #[test]
    fn usb_remove_is_a_hotplug_event() {
        assert!(is_usb_hotplug_event(&payload(&[
            "remove@/devices/pci0000:00/usb1/1-2",
            "ACTION=remove",
            "SUBSYSTEM=usb",
        ])));
    }

    #[test]
    fn other_subsystems_are_ignored() {
        assert!(!is_usb_hotplug_event(&payload(&[
            "add@/devices/virtual/block/loop0",
            "ACTION=add",
            "SUBSYSTEM=block",
        ])));
    }

    #[test]
    fn usb_change_without_hotplug_action_is_ignored() {
        assert!(!is_usb_hotplug_event(&payload(&[
            "change@/devices/pci0000:00/usb1/1-2",
            "ACTION=change",
            "SUBSYSTEM=usb",
        ])));
    }

    #[test]
    fn sysfs_signature_does_not_panic() {
        let _ = usb_device_signature();
    }
}
