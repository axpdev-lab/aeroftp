// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//! Registry policy, independent of Windows APIs so foreign-data preservation can
//! be exercised on every platform. Callers journal writes and roll back on error.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choices {
    pub owned_formats: bool,
    pub archive_formats: bool,
}

pub struct Registration {
    pub group: String,
    pub key: String,
    pub name: String,
    pub value: String,
}

pub struct OwnedType {
    pub extension: String,
    pub progid: String,
    pub mime: String,
}

pub trait Registry {
    fn get(&self, key: &str, name: &str) -> Result<Option<String>, String>;
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), String>;
    fn remove(&mut self, key: &str, name: &str) -> Result<(), String>;
    fn remove_tree(&mut self, key: &str) -> Result<(), String>;
}

fn is_owned(progid: &str, item: &OwnedType) -> bool {
    [
        item.progid.as_str(),
        item.extension.as_str(),
        &format!("AeroFTP.{}", item.extension),
    ]
    .iter()
    .any(|candidate| progid.eq_ignore_ascii_case(candidate))
}

/// Retire old NSIS/Tauri registrations without touching another handler or the
/// protected UserChoice branch. A prior handler is restored only from a real,
/// non-AeroFTP backup. New registrations are additive and need no such backup.
pub fn remove_legacy<R: Registry>(
    reg: &mut R,
    types: &[OwnedType],
    exe: &str,
) -> Result<(), String> {
    for item in types {
        let key = format!(r"Software\Classes\.{}", item.extension);
        let backup_name = format!("{}_backup", item.extension);
        if reg
            .get(&key, "")?
            .as_deref()
            .is_some_and(|p| is_owned(p, item))
        {
            match reg.get(&key, &backup_name)? {
                Some(p) if !p.is_empty() && !is_owned(&p, item) => reg.set(&key, "", &p)?,
                _ => reg.remove(&key, "")?,
            }
        }
        reg.remove(&key, &backup_name)?;
        if reg.get(&key, "Content Type")?.as_deref() == Some(item.mime.as_str()) {
            reg.remove(&key, "Content Type")?;
            if reg.get(&key, "PerceivedType")?.as_deref() == Some("document") {
                reg.remove(&key, "PerceivedType")?;
            }
        }
        let mime = format!(r"Software\Classes\MIME\Database\Content Type\{}", item.mime);
        if reg.get(&mime, "Extension")?.as_deref() == Some(&format!(".{}", item.extension)) {
            reg.remove(&mime, "Extension")?;
        }
        for progid in [
            &item.progid,
            &item.extension,
            &format!("AeroFTP.{}", item.extension),
        ] {
            reg.remove(&format!(r"{key}\OpenWithProgids"), progid)?;
            let base = format!(r"Software\Classes\{progid}");
            // A similarly named registration belonging to another installation
            // is not ours to delete. Accept both historical command spellings.
            if reg
                .get(&format!(r"{base}\shell\open\command"), "")?
                .is_some_and(|c| {
                    c.eq_ignore_ascii_case(&format!("\"{exe}\" \"%1\""))
                        || c.eq_ignore_ascii_case(&format!("{exe} \"%1\""))
                })
            {
                reg.remove_tree(&base)?;
            }
        }
    }
    Ok(())
}

pub fn apply<R: Registry>(
    reg: &mut R,
    values: &[Registration],
    choices: Choices,
    dir: &str,
    exe: &str,
) -> Result<(), String> {
    for item in values {
        let enabled = match item.group.as_str() {
            "owned" => choices.owned_formats,
            "archives" => choices.archive_formats,
            "common" => choices.owned_formats || choices.archive_formats,
            _ => return Err("Unknown Windows association group".to_string()),
        };
        let value = item.value.replace("@DIR@", dir).replace("@EXE@", exe);
        if enabled {
            reg.set(&item.key, &item.name, &value)?;
        } else if reg
            .get(&item.key, &item.name)?
            .as_deref()
            .is_some_and(|current| current.eq_ignore_ascii_case(&value))
        {
            reg.remove(&item.key, &item.name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Fake(BTreeMap<(String, String), String>);
    impl Registry for Fake {
        fn get(&self, k: &str, n: &str) -> Result<Option<String>, String> {
            Ok(self.0.get(&(k.into(), n.into())).cloned())
        }
        fn set(&mut self, k: &str, n: &str, v: &str) -> Result<(), String> {
            self.0.insert((k.into(), n.into()), v.into());
            Ok(())
        }
        fn remove(&mut self, k: &str, n: &str) -> Result<(), String> {
            self.0.remove(&(k.into(), n.into()));
            Ok(())
        }
        fn remove_tree(&mut self, k: &str) -> Result<(), String> {
            self.0
                .retain(|(key, _), _| key != k && !key.starts_with(&format!("{k}\\")));
            Ok(())
        }
    }
    fn owned() -> Vec<OwnedType> {
        vec![OwnedType {
            extension: "aeroftp".into(),
            progid: "AeroFTP.Profile".into(),
            mime: "application/x-aeroftp".into(),
        }]
    }
    #[test]
    fn unregister_restores_old_handler_and_preserves_foreign_extension_values_and_userchoice() {
        let mut r = Fake::default();
        let k = r"Software\Classes\.aeroftp";
        r.set(k, "", "AeroFTP.Profile").unwrap();
        r.set(k, "aeroftp_backup", "Other.Profile").unwrap();
        r.set(k, "ForeignValue", "keep").unwrap();
        let uc = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.aeroftp\UserChoice";
        r.set(uc, "ProgId", "Other.Profile").unwrap();
        remove_legacy(&mut r, &owned(), r"C:\AeroFTP\AeroFTP.exe").unwrap();
        assert_eq!(r.get(k, "").unwrap().as_deref(), Some("Other.Profile"));
        assert_eq!(r.get(k, "ForeignValue").unwrap().as_deref(), Some("keep"));
        assert_eq!(
            r.get(uc, "ProgId").unwrap().as_deref(),
            Some("Other.Profile")
        );
        assert_eq!(r.get(k, "aeroftp_backup").unwrap(), None);
    }
    #[test]
    fn unregister_never_overwrites_a_handler_changed_after_install() {
        let mut r = Fake::default();
        let k = r"Software\Classes\.aeroftp";
        r.set(k, "", "New.Handler").unwrap();
        r.set(k, "aeroftp_backup", "Old.Handler").unwrap();
        remove_legacy(&mut r, &owned(), r"C:\AeroFTP\AeroFTP.exe").unwrap();
        assert_eq!(r.get(k, "").unwrap().as_deref(), Some("New.Handler"));
    }
    #[test]
    fn self_backups_are_removed_without_recreating_an_empty_default() {
        for backup in ["", "aeroftp", "AeroFTP.Profile", "AeroFTP.aeroftp"] {
            let mut r = Fake::default();
            let k = r"Software\Classes\.aeroftp";
            r.set(k, "", "AeroFTP.Profile").unwrap();
            r.set(k, "aeroftp_backup", backup).unwrap();
            remove_legacy(&mut r, &owned(), "aeroftp.exe").unwrap();
            assert_eq!(r.get(k, "").unwrap(), None);
        }
    }
    #[test]
    fn toggles_are_independent_and_preserve_shared_openwith_values() {
        let mut r = Fake::default();
        let k = r"Software\Classes\.zip\OpenWithProgids";
        r.set(k, "Other.Archive", "").unwrap();
        let entries = vec![
            Registration {
                group: "archives".into(),
                key: k.into(),
                name: "AeroFTP.Archive".into(),
                value: "".into(),
            },
            Registration {
                group: "owned".into(),
                key: r"Software\Classes\.aeroftp\OpenWithProgids".into(),
                name: "AeroFTP.Profile".into(),
                value: "".into(),
            },
        ];
        apply(
            &mut r,
            &entries,
            Choices {
                owned_formats: true,
                archive_formats: true,
            },
            "D",
            "E",
        )
        .unwrap();
        apply(
            &mut r,
            &entries,
            Choices {
                owned_formats: true,
                archive_formats: false,
            },
            "D",
            "E",
        )
        .unwrap();
        assert_eq!(r.get(k, "Other.Archive").unwrap().as_deref(), Some(""));
        assert_eq!(r.get(k, "AeroFTP.Archive").unwrap(), None);
        assert!(r.get(&entries[1].key, &entries[1].name).unwrap().is_some());
        apply(
            &mut r,
            &entries,
            Choices {
                owned_formats: false,
                archive_formats: false,
            },
            "D",
            "E",
        )
        .unwrap();
        assert_eq!(r.0.len(), 1);
    }
    #[test]
    fn legacy_progid_of_another_installation_is_preserved() {
        let mut r = Fake::default();
        let k = r"Software\Classes\aeroftp\shell\open\command";
        r.set(k, "", r#""C:\Other\AeroFTP.exe" "%1""#).unwrap();
        remove_legacy(&mut r, &owned(), r"C:\Current\AeroFTP.exe").unwrap();
        assert!(r.get(k, "").unwrap().is_some());
    }
}
