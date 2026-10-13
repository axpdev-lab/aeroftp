// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//! Installed Windows shell integration. NSIS preferences are per-user. MSI
//! changes go through Windows Installer, preserving its component bookkeeping.
use super::association_policy::{self, Choices, OwnedType, Registration, Registry};
use super::WindowsIntegrationStatus;
use serde::Deserialize;
use std::{collections::BTreeSet, io, path::Path};
use winreg::{enums::*, types::FromRegValue, RegKey, RegValue};

static OPERATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const SETTINGS: &str = r"Software\AeroFTP\Settings";
const OWNED_FEATURE: &str = "AeroFTPOwnedAssociations";
const ARCHIVE_FEATURE: &str = "AeroFTPArchiveAssociations";

#[derive(Deserialize)]
struct Catalog {
    owned: Vec<CatalogType>,
    archives: Vec<String>,
    values: Vec<CatalogValue>,
}
#[derive(Deserialize)]
struct CatalogType {
    extension: String,
    progid: String,
    mime: String,
}
#[derive(Deserialize)]
struct CatalogValue {
    group: String,
    key: String,
    name: String,
    value: String,
}
fn catalog() -> Result<Catalog, String> {
    serde_json::from_str(include_str!("../installer/association-registry.json"))
        .map_err(|e| e.to_string())
}
fn missing(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
}
fn norm(path: &Path) -> String {
    path.to_string_lossy()
        .trim_matches('"')
        .trim_start_matches(r"\\?\")
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}
enum Installer {
    Nsis,
    Msi(String),
}

fn install_record() -> Result<Option<Installer>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("Executable has no parent folder")?;
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let root = RegKey::predef(hive);
            let key = match root.open_subkey_with_flags(
                r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
                KEY_READ | view,
            ) {
                Ok(k) => k,
                Err(e) if missing(&e) => continue,
                Err(e) => return Err(e.to_string()),
            };
            for name in key.enum_keys() {
                let name = name.map_err(|e| e.to_string())?;
                // Unrelated products may have inaccessible uninstall entries.
                let sub = match key.open_subkey_with_flags(&name, KEY_READ | view) {
                    Ok(sub) => sub,
                    Err(_) => continue,
                };
                let display: String = sub.get_value("DisplayName").unwrap_or_default();
                let loc: String = sub.get_value("InstallLocation").unwrap_or_default();
                if !display.contains("AeroFTP")
                    || loc.is_empty()
                    || norm(Path::new(&loc)) != norm(dir)
                {
                    continue;
                }
                if sub.get_value::<u32, _>("WindowsInstaller").unwrap_or(0) == 1 {
                    if name.len() != 38
                        || !name.starts_with('{')
                        || !name.ends_with('}')
                        || !name[1..37]
                            .chars()
                            .all(|c| c.is_ascii_hexdigit() || c == '-')
                    {
                        return Err("Invalid MSI product code".into());
                    }
                    return Ok(Some(Installer::Msi(name)));
                }
                return Ok(Some(Installer::Nsis));
            }
        }
    }
    Ok(None)
}

#[link(name = "msi")]
unsafe extern "system" {
    fn MsiQueryFeatureStateW(product: *const u16, feature: *const u16) -> i32;
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn feature_state(product: &str, feature: &str) -> Result<bool, String> {
    let product = wide(product);
    let feature = wide(feature);
    // INSTALLSTATE_LOCAL=3, INSTALLSTATE_ABSENT=2. Unknown indicates an older
    // installer without these features; never silently claim that can be changed.
    match unsafe {MsiQueryFeatureStateW(product.as_ptr(),feature.as_ptr())} {
        3=>Ok(true),2=>Ok(false),state=>Err(format!("This MSI does not expose selectable file associations (state {state}). Install the current MSI first.")),
    }
}
fn pref(root: &RegKey, name: &str) -> Result<bool, String> {
    let key = match root.open_subkey_with_flags(SETTINGS, KEY_READ) {
        Ok(k) => k,
        Err(e) if missing(&e) => return Ok(true),
        Err(e) => return Err(e.to_string()),
    };
    match key.get_value::<u32, _>(name) {
        Ok(v) => Ok(v == 1),
        Err(e) if missing(&e) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

pub fn status() -> Result<WindowsIntegrationStatus, String> {
    let _guard = OPERATION_LOCK
        .lock()
        .map_err(|_| "Windows integration lock is poisoned")?;
    read_status()
}

fn read_status() -> Result<WindowsIntegrationStatus, String> {
    if crate::portable::is_portable() {
        return Ok(WindowsIntegrationStatus {
            owned_formats: false,
            archive_formats: false,
            managed_by: "portable".into(),
            can_change: false,
        });
    }
    let installation = install_record()?;
    if installation.is_none() {
        return Ok(WindowsIntegrationStatus {
            owned_formats: false,
            archive_formats: false,
            managed_by: "portable".into(),
            can_change: false,
        });
    }
    if let Some(Installer::Msi(product)) = installation {
        return Ok(WindowsIntegrationStatus {
            owned_formats: feature_state(&product, OWNED_FEATURE)?,
            archive_formats: feature_state(&product, ARCHIVE_FEATURE)?,
            managed_by: "msi".into(),
            can_change: true,
        });
    }
    let root = RegKey::predef(HKEY_CURRENT_USER);
    Ok(WindowsIntegrationStatus {
        owned_formats: pref(&root, "OwnedFormats")?,
        archive_formats: pref(&root, "ArchiveFormats")?,
        managed_by: "nsis".into(),
        can_change: true,
    })
}

/// Journal original raw values, including registry types. Failed writes restore
/// their originals; preferences only commit together with registration changes.
struct Journal {
    root: RegKey,
    originals: Vec<(String, String, Option<RegValue<'static>>)>,
    keys: BTreeSet<String>,
}
impl Journal {
    fn new() -> Self {
        Self {
            root: RegKey::predef(HKEY_CURRENT_USER),
            originals: Vec::new(),
            keys: BTreeSet::new(),
        }
    }
    fn raw(&self, key: &str, name: &str) -> Result<Option<RegValue<'static>>, String> {
        let k = match self.root.open_subkey_with_flags(key, KEY_READ) {
            Ok(k) => k,
            Err(e) if missing(&e) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        match k.get_raw_value(name) {
            Ok(v) => Ok(Some(v)),
            Err(e) if missing(&e) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn remember(&mut self, key: &str, name: &str) -> Result<(), String> {
        if !self.originals.iter().any(|(k, n, _)| k == key && n == name) {
            self.originals
                .push((key.into(), name.into(), self.raw(key, name)?));
        }
        let mut k = key.to_string();
        while k.contains('\\') {
            self.keys.insert(k.clone());
            k.truncate(k.rfind('\\').unwrap());
        }
        Ok(())
    }
    fn set_dword(&mut self, name: &str, value: bool) -> Result<(), String> {
        self.remember(SETTINGS, name)?;
        self.root
            .create_subkey(SETTINGS)
            .map_err(|e| e.to_string())?
            .0
            .set_value(name, &u32::from(value))
            .map_err(|e| e.to_string())
    }
    fn cleanup(&self) -> Result<(), String> {
        let mut keys: Vec<_> = self.keys.iter().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        for name in keys {
            // Never sweep OS containers outside the registration subtrees.
            if !(name.starts_with(r"Software\Classes\")
                || name.starts_with(r"Software\AeroFTP\Capabilities")
                || name
                    .starts_with(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\."))
            {
                continue;
            }
            let key = match self.root.open_subkey_with_flags(name, KEY_READ) {
                Ok(k) => k,
                Err(e) if missing(&e) => continue,
                Err(e) => return Err(e.to_string()),
            };
            let info = key.query_info().map_err(|e| e.to_string())?;
            if info.sub_keys == 0 && info.values == 0 {
                self.root.delete_subkey(name).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    fn rollback(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        for (key, name, original) in self.originals.iter().rev() {
            let r = match original {
                Some(v) => self
                    .root
                    .create_subkey(key)
                    .and_then(|(k, _)| k.set_raw_value(name, v)),
                None => match self.root.open_subkey_with_flags(key, KEY_SET_VALUE) {
                    Ok(k) => k.delete_value(name),
                    Err(e) => Err(e),
                },
            };
            if let Err(e) = r {
                if !missing(&e) {
                    errors.push(e.to_string());
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
    fn remove_history(&mut self, ext: &str, progids: &[String]) -> Result<(), String> {
        let base = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{ext}");
        for p in progids {
            self.remove(&format!(r"{base}\OpenWithProgids"), p)?;
        }
        let list = format!(r"{base}\OpenWithList");
        let values = match self.root.open_subkey_with_flags(&list, KEY_READ) {
            Ok(k) => k
                .enum_values()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
            Err(e) if missing(&e) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let mut mru = self.get(&list, "MRUList")?;
        for (name, value) in values {
            if name == "MRUList" {
                continue;
            }
            if String::from_reg_value(&value)
                .ok()
                .is_some_and(|v| v.eq_ignore_ascii_case("AeroFTP.exe"))
            {
                self.remove(&list, &name)?;
                if let Some(mru) = mru.as_mut() {
                    *mru = mru.replace(&name, "");
                }
            }
        }
        if let Some(mru) = mru {
            if mru.is_empty() {
                self.remove(&list, "MRUList")?;
            } else {
                self.set(&list, "MRUList", &mru)?;
            }
        }
        Ok(())
    }
}
impl Registry for Journal {
    fn get(&self, key: &str, name: &str) -> Result<Option<String>, String> {
        match self.raw(key, name)? {
            None => Ok(None),
            Some(v) if v.bytes.is_empty() => Ok(Some(String::new())),
            Some(v) => String::from_reg_value(&v)
                .map(Some)
                .map_err(|e| e.to_string()),
        }
    }
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), String> {
        self.remember(key, name)?;
        self.root
            .create_subkey(key)
            .map_err(|e| e.to_string())?
            .0
            .set_value(name, &value)
            .map_err(|e| e.to_string())
    }
    fn remove(&mut self, key: &str, name: &str) -> Result<(), String> {
        self.remember(key, name)?;
        match self.root.open_subkey_with_flags(key, KEY_SET_VALUE) {
            Ok(k) => match k.delete_value(name) {
                Ok(()) => Ok(()),
                Err(e) if missing(&e) => Ok(()),
                Err(e) => Err(e.to_string()),
            },
            Err(e) if missing(&e) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn remove_tree(&mut self, key: &str) -> Result<(), String> {
        let k = match self.root.open_subkey_with_flags(key, KEY_READ) {
            Ok(k) => k,
            Err(e) if missing(&e) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        for name in k
            .enum_keys()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            self.remove_tree(&format!(r"{key}\{name}"))?;
        }
        for (name, _) in k
            .enum_values()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            self.remove(key, &name)?;
        }
        Ok(()) // Empty containers are removed after the transaction succeeds.
    }
}

pub fn apply(choices: Choices) -> Result<WindowsIntegrationStatus, String> {
    let _guard = OPERATION_LOCK
        .lock()
        .map_err(|_| "Windows integration lock is poisoned")?;
    if crate::portable::is_portable() {
        return Err("Install AeroFTP before changing Windows integration".into());
    }
    let installation =
        install_record()?.ok_or("Install AeroFTP before changing Windows integration")?;
    if let Installer::Msi(product) = installation {
        // Validate both features before dispatching a maintenance operation.
        feature_state(&product, OWNED_FEATURE)?;
        feature_state(&product, ARCHIVE_FEATURE)?;
        let mut add = Vec::new();
        let mut remove = Vec::new();
        for (feature, enabled) in [
            (OWNED_FEATURE, choices.owned_formats),
            (ARCHIVE_FEATURE, choices.archive_formats),
        ] {
            if enabled {
                add.push(feature);
            } else {
                remove.push(feature);
            }
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let install_dir = exe.parent().ok_or("Executable has no parent folder")?;
        let mut cmd = crate::hidden_command("msiexec.exe");
        // Another Windows user may not have this package's HKCU InstallDir.
        // Bind maintenance to the running installation's verified location.
        cmd.args(["/i", &product, "/qb", "REBOOT=ReallySuppress"]);
        cmd.arg(format!("INSTALLDIR={}", install_dir.display()));
        if !add.is_empty() {
            cmd.arg(format!("ADDLOCAL={}", add.join(",")));
        }
        if !remove.is_empty() {
            cmd.arg(format!("REMOVE={}", remove.join(",")));
        }
        let code = cmd
            .status()
            .map_err(|e| e.to_string())?
            .code()
            .unwrap_or(-1);
        if code != 0 && code != 3010 {
            return Err(format!(
                "Windows Installer did not apply the association choices (exit {code})"
            ));
        }
        let state = read_status()?;
        if state.owned_formats != choices.owned_formats
            || state.archive_formats != choices.archive_formats
        {
            return Err("Windows Installer did not confirm the requested association state".into());
        }
        cleanup_history(choices)?;
        notify();
        return Ok(state);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe
        .parent()
        .ok_or("Executable has no parent")?
        .to_string_lossy();
    let exe = exe.to_string_lossy();
    apply_nsis(&mut Journal::new(), choices, &dir, &exe)?;
    notify();
    read_status()
}

fn apply_nsis(journal: &mut Journal, choices: Choices, dir: &str, exe: &str) -> Result<(), String> {
    let catalog = catalog()?;
    let types: Vec<_> = catalog
        .owned
        .into_iter()
        .map(|t| OwnedType {
            extension: t.extension,
            progid: t.progid,
            mime: t.mime,
        })
        .collect();
    let values: Vec<_> = catalog
        .values
        .into_iter()
        .map(|v| Registration {
            group: v.group,
            key: v.key,
            name: v.name,
            value: v.value,
        })
        .collect();
    let result = (|| {
        if !choices.owned_formats {
            association_policy::remove_legacy(journal, &types, exe)?;
            for t in &types {
                journal.remove_history(
                    &t.extension,
                    &[
                        t.progid.clone(),
                        t.extension.clone(),
                        format!("AeroFTP.{}", t.extension),
                    ],
                )?;
            }
        }
        if !choices.archive_formats {
            for ext in &catalog.archives {
                journal.remove_history(ext, &["AeroFTP.Archive".into()])?;
            }
        }
        association_policy::apply(journal, &values, choices, dir, exe)?;
        journal.set_dword("OwnedFormats", choices.owned_formats)?;
        journal.set_dword("ArchiveFormats", choices.archive_formats)?;
        journal.cleanup()
    })();
    if let Err(e) = result {
        let rollback = journal.rollback();
        let _ = journal.cleanup();
        return Err(match rollback {
            Ok(()) => e,
            Err(r) => format!("{e}; rollback: {r}"),
        });
    }
    Ok(())
}

pub fn cleanup_history(choices: Choices) -> Result<(), String> {
    let catalog = catalog()?;
    let mut journal = Journal::new();
    let result = (|| {
        if !choices.owned_formats {
            for t in &catalog.owned {
                journal.remove_history(
                    &t.extension,
                    &[
                        t.progid.clone(),
                        t.extension.clone(),
                        format!("AeroFTP.{}", t.extension),
                    ],
                )?;
            }
        }
        if !choices.archive_formats {
            for ext in &catalog.archives {
                journal.remove_history(ext, &["AeroFTP.Archive".into()])?;
            }
        }
        journal.cleanup()
    })();
    if let Err(error) = result {
        return match journal.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!("{error}; rollback: {rollback}")),
        };
    }
    Ok(())
}

pub fn available(progids: &[&str]) -> bool {
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let root = RegKey::predef(hive);
        for id in progids {
            if root
                .open_subkey_with_flags(
                    format!(r"Software\Classes\{id}\shell\open\command"),
                    KEY_READ,
                )
                .is_ok()
            {
                return true;
            }
        }
    }
    false
}
fn notify() {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHChangeNotify(
            event: i32,
            flags: u32,
            item1: *const core::ffi::c_void,
            item2: *const core::ffi::c_void,
        );
    }
    unsafe {
        SHChangeNotify(0x08000000, 0, core::ptr::null(), core::ptr::null());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        key: String,
        journal: Journal,
    }
    impl Fixture {
        fn new() -> Self {
            let key = format!(
                r"Software\AeroFTP-454-test\{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let root = RegKey::predef(HKEY_CURRENT_USER)
                .create_subkey(&key)
                .unwrap()
                .0;
            Self {
                key,
                journal: Journal {
                    root,
                    originals: vec![],
                    keys: BTreeSet::new(),
                },
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.key);
        }
    }
    #[test]
    fn native_registry_choices_preserve_foreign_data_and_protected_defaults() {
        let mut f = Fixture::new();
        let r = &mut f.journal;
        r.set(
            r"Software\Classes\.zip\OpenWithProgids",
            "Other.Archive",
            "",
        )
        .unwrap();
        r.set(r"Software\Classes\.aeroftp", "", "Other.Profile")
            .unwrap();
        r.set(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.aeroftp\UserChoice",
            "ProgId",
            "Other.Profile",
        )
        .unwrap();
        let list =
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.aeroftp\OpenWithList";
        r.set(list, "a", "AeroFTP.exe").unwrap();
        r.set(list, "b", "notepad.exe").unwrap();
        r.set(list, "MRUList", "ba").unwrap();
        apply_nsis(
            r,
            Choices {
                owned_formats: true,
                archive_formats: true,
            },
            r"C:\Apps\AeroFTP",
            r"C:\Apps\AeroFTP\AeroFTP.exe",
        )
        .unwrap();
        apply_nsis(
            r,
            Choices {
                owned_formats: true,
                archive_formats: false,
            },
            r"C:\Apps\AeroFTP",
            r"C:\Apps\AeroFTP\AeroFTP.exe",
        )
        .unwrap();
        assert!(pref(&r.root, "OwnedFormats").unwrap());
        assert!(!pref(&r.root, "ArchiveFormats").unwrap());
        apply_nsis(
            r,
            Choices {
                owned_formats: false,
                archive_formats: false,
            },
            r"C:\Apps\AeroFTP",
            r"C:\Apps\AeroFTP\AeroFTP.exe",
        )
        .unwrap();
        assert_eq!(
            r.get(r"Software\Classes\.zip\OpenWithProgids", "Other.Archive")
                .unwrap()
                .as_deref(),
            Some("")
        );
        assert_eq!(
            r.get(r"Software\Classes\.aeroftp", "").unwrap().as_deref(),
            Some("Other.Profile")
        );
        assert_eq!(
            r.get(
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.aeroftp\UserChoice",
                "ProgId"
            )
            .unwrap()
            .as_deref(),
            Some("Other.Profile")
        );
        assert_eq!(r.get(list, "MRUList").unwrap().as_deref(), Some("b"));
        assert!(r.get(list, "a").unwrap().is_none());
        assert!(r
            .root
            .open_subkey(r"Software\Classes\AeroFTP.Profile")
            .is_err());
    }
    #[test]
    fn failed_change_rolls_back_values_types_and_preferences() {
        let mut f = Fixture::new();
        let r = &mut f.journal;
        let key = r"Software\Classes\SystemFileAssociations\.zip\shell\AeroFTPExtractHere";
        let (k, _) = r.root.create_subkey(key).unwrap();
        let original = RegValue {
            vtype: REG_BINARY,
            bytes: vec![1, 2, 3].into(),
        };
        k.set_raw_value("", &original).unwrap();
        // Owned writes occur before the bad archive type is encountered.
        assert!(apply_nsis(
            r,
            Choices {
                owned_formats: true,
                archive_formats: false
            },
            r"C:\AeroFTP",
            r"C:\AeroFTP\AeroFTP.exe"
        )
        .is_err());
        let actual = k.get_raw_value("").unwrap();
        assert_eq!(actual.vtype, REG_BINARY);
        assert_eq!(actual.bytes, original.bytes);
        assert!(r
            .root
            .open_subkey(r"Software\Classes\AeroFTP.Profile")
            .is_err());
        assert!(r.raw(SETTINGS, "OwnedFormats").unwrap().is_none());
    }
}
