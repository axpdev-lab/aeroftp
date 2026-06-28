; AeroFTP NSIS Installer Hooks
; Post-install and pre-uninstall actions for Windows.

; ── EnVar plugin search path ──
; The EnVar plugin (https://nsis.sourceforge.io/EnVar_plug-in, zlib licence)
; ships vendored under installer/Plugins/. It is used by the PATH manipulation
; blocks below so that adding and removing $INSTDIR from HKCU\Environment\Path
; goes through the Win32 registry APIs directly, instead of NSIS string ops.
; This is what fixes issue #240: the previous read-then-write implementation
; could silently wipe the user's PATH when its value exceeded NSIS_MAX_STRLEN
; (8192 chars, easily exceeded by a developer profile with Scoop, NVM,
; Flutter, npm, pyenv, etc. accumulated). ${__FILEDIR__} resolves to the
; directory of this .nsh file so the path stays correct regardless of where
; the Tauri bundler stages the build.
!addplugindir "${__FILEDIR__}\Plugins\x86-unicode"

; ── Upgrade detection state ──
; Captured in NSIS_HOOK_PREINSTALL, consumed in NSIS_HOOK_POSTINSTALL.
; "yes" means an AeroFTP.exe already lived in $INSTDIR before the bundler
; copied the new files, i.e. this install is an upgrade rather than a
; first-time install.
Var AeroFTPWasInstalled
; "yes" means $DESKTOP\AeroFTP.lnk existed before the install ran. We use
; this to respect the user's choice: if they had previously deleted the
; desktop shortcut, an upgrade (any source — in-app updater, WinGet,
; manual reinstall) should not silently recreate it. See issue #123.
Var AeroFTPHadDesktopShortcut
; Captured at the very start of NSIS_HOOK_PREUNINSTALL, consumed at the
; end of NSIS_HOOK_POSTUNINSTALL. "yes" means `$APPDATA\com.aeroftp.AeroFTP`
; existed before the uninstaller sections ran. Combined with a post-state
; check of the same path, it tells us whether the Tauri "Remove
; application data" optional section actually deleted it -- the canonical
; signal that the user selected "delete all data" on the components page.
; See APPENDIX-O Auto-Update System addendum 2026-05-11.
Var AeroFTPAppDataPresentPre

; CRITICAL: Tauri's bundled installer.nsi invokes the four hooks below by
; the names NSIS_HOOK_{PRE,POST}{INSTALL,UNINSTALL}, gated by an
; `!ifmacrodef`. From v3.6.2 (commit e4d7868e) through v3.6.3 we shipped
; these as CUSTOM_PRE_INSTALL / CUSTOM_POST_INSTALL / CUSTOM_PRE_UNINSTALL,
; which `!ifmacrodef` skipped silently. Result: every hook in this file —
; the HKCU PATH registration, the .aerovault association, the VC++
; Runtime bootstrap, the desktop-shortcut respect logic — was inert in
; every shipped Windows installer. See bug report
; docs/dev/aeroftp-windows-path-hook-bug-report-2026-04-25.md.
; ── Deliverable G: "Extract here / Extract to folder" context-menu verbs ──
; These two helper macros write (and delete) the AeroFTP extract verbs under a
; given HKCU class base key. They are ADDITIVE verbs (owner decision c): they
; never write a default `shell\open`, so double-click behaviour of the general
; archive formats (.zip/.7z/.tar*/.rar) is left untouched and whatever app the
; user already has stays their default extractor. For those general formats the
; caller points BASEKEY at `Software\Classes\SystemFileAssociations\.<ext>`,
; which layers the verb on top of the system ProgID regardless of who owns it;
; for our own aero* ProgIDs the caller points BASEKEY at the ProgID directly so
; the verbs sit next to their existing "Open with AeroFTP" entry.
;
; The command launches the cross-platform GUI extract intent already shipped on
; main: `AeroFTP.exe --extract-here "%1"` / `--extract-to "%1"`. The dedicated
; extract window (extract.html, a tiny bundle) computes the never-clobber stem
; subfolder (resolve_unique_extract_dir), prompts for a password on encrypted
; archives and vaults, and shows the native destination picker for "to folder".
; We target the GUI binary directly (not the dispatcher) because on Windows the
; product binary keeps the name AeroFTP.exe and parses --extract-here/-to from
; its own argv; a cold launch opens ONLY the extract window and exits.
!macro AeroFTPWriteExtractVerbs BASEKEY
    WriteRegStr HKCU "${BASEKEY}\shell\AeroFTPExtractHere" "" "Extract Here with AeroFTP"
    WriteRegStr HKCU "${BASEKEY}\shell\AeroFTPExtractHere" "Icon" '"$INSTDIR\AeroFTP.exe",0'
    WriteRegStr HKCU "${BASEKEY}\shell\AeroFTPExtractHere\command" "" '"$INSTDIR\AeroFTP.exe" --extract-here "%1"'
    WriteRegStr HKCU "${BASEKEY}\shell\AeroFTPExtractToFolder" "" "Extract to Folder with AeroFTP..."
    WriteRegStr HKCU "${BASEKEY}\shell\AeroFTPExtractToFolder" "Icon" '"$INSTDIR\AeroFTP.exe",0'
    WriteRegStr HKCU "${BASEKEY}\shell\AeroFTPExtractToFolder\command" "" '"$INSTDIR\AeroFTP.exe" --extract-to "%1"'
!macroend

!macro AeroFTPDeleteExtractVerbs BASEKEY
    DeleteRegKey HKCU "${BASEKEY}\shell\AeroFTPExtractHere"
    DeleteRegKey HKCU "${BASEKEY}\shell\AeroFTPExtractToFolder"
!macroend

!macro NSIS_HOOK_PREINSTALL
    StrCpy $AeroFTPWasInstalled "no"
    StrCpy $AeroFTPHadDesktopShortcut "no"
    IfFileExists "$INSTDIR\AeroFTP.exe" 0 _aeroftp_pre_check_shortcut
        StrCpy $AeroFTPWasInstalled "yes"
    _aeroftp_pre_check_shortcut:
    IfFileExists "$DESKTOP\AeroFTP.lnk" 0 _aeroftp_pre_install_done
        StrCpy $AeroFTPHadDesktopShortcut "yes"
    _aeroftp_pre_install_done:
!macroend

!macro NSIS_HOOK_POSTINSTALL
    ; --- Don't recreate desktop shortcut on upgrades (issue #123) ---
    ; When this run is an upgrade AND the user had no desktop shortcut
    ; before it started, delete the one the Tauri NSIS template just
    ; recreated. First-time installs (where AeroFTPWasInstalled stays
    ; "no") keep the shortcut Tauri creates. Users who like having the
    ; shortcut will still see it after upgrades because it was already
    ; present (AeroFTPHadDesktopShortcut == "yes") and we don't touch it.
    StrCmp $AeroFTPWasInstalled "yes" 0 _aeroftp_shortcut_done
        StrCmp $AeroFTPHadDesktopShortcut "no" 0 _aeroftp_shortcut_done
            Delete "$DESKTOP\AeroFTP.lnk"
    _aeroftp_shortcut_done:

    ; --- Register install dir in user PATH (HKCU) ---
    ; PR-T11 follow-up (issue #125). The Tauri per-user installer drops
    ; binaries in %LOCALAPPDATA%\AeroFTP\ but the bundled NSIS template
    ; does not register that directory in HKCU\Environment\Path. Without
    ; this hook the VS Code MCP extension, the in-app terminal, and any
    ; tool that relies on PATH resolution cannot locate aeroftp-cli even
    ; though it is on disk and works fine when invoked by absolute path.
    ;
    ; Implementation: delegate the append to EnVar::AddValue, which
    ; talks to the Win32 registry APIs directly, handles arbitrarily
    ; long PATH values, preserves the original value type (REG_EXPAND_SZ
    ; vs REG_SZ), and is idempotent (a no-op when $INSTDIR is already
    ; present). The previous implementation read the value into a NSIS
    ; string and then rewrote it via WriteRegExpandStr; on developer
    ; profiles whose PATH exceeded NSIS_MAX_STRLEN (8192 chars), the
    ; read silently returned empty and the rewrite replaced the entire
    ; user PATH with just $INSTDIR, wiping Scoop / NVM / Flutter / npm /
    ; pyenv / etc. entries. See issue #240.
    ;
    ; EnVar::AddValue return codes (popped into $0):
    ;   0 = success (path added, or already present)
    ;   1 = out of memory          2 = could not read environment
    ;   3 = variable does not exist 4 = wrong type
    ;   5 = value does not exist    6 = could not write environment
    EnVar::SetHKCU
    EnVar::AddValue "Path" "$INSTDIR"
    Pop $0
    DetailPrint "EnVar::AddValue Path $INSTDIR -> code $0"

    ; --- Opt-in alias bin dir on PATH (for `aeroftp-cli alias-toggle`) ---
    ; v4.0.5 / discussion #273. `alias-toggle <name>` drops a `<name>.cmd`
    ; shim into %LOCALAPPDATA%\AeroFTP\bin (its default --bin-dir). Pre-create
    ; that directory and register it in HKCU\Environment\Path so the shim is
    ; usable in a fresh shell without the user hand-editing PATH. This mirrors
    ; the convenience the Linux packages get for free (~/.local/bin is already
    ; on PATH on most distros). EnVar::AddValue is idempotent; the matching
    ; PREUNINSTALL block removes the entry. The directory stays empty until
    ; the user actually runs alias-toggle, which is harmless on PATH.
    CreateDirectory "$LOCALAPPDATA\AeroFTP\bin"
    EnVar::AddValue "Path" "$LOCALAPPDATA\AeroFTP\bin"
    Pop $0
    DetailPrint "EnVar::AddValue Path $LOCALAPPDATA\AeroFTP\bin -> code $0"

    ; WM_SETTINGCHANGE = 0x001A — same signal Inno Setup's
    ; ChangesEnvironment=yes emits. Running shells (Explorer, VS Code,
    ; PowerShell via integrated terminal) get a chance to refresh
    ; without logoff. PowerShell sessions started before this install
    ; cache their environment at launch, so even after the broadcast
    ; they cannot resolve aeroftp-cli — the DetailPrint below documents
    ; the new-terminal requirement.
    System::Call 'USER32::SendMessageTimeoutW(i 0xffff, i 0x001A, i 0, w "Environment", i 0, i 5000, *i .r3)'
    DetailPrint "Added $INSTDIR to PATH. Open a NEW terminal to run 'aeroftp-cli' or 'aftp'."

    ; --- Ship the `aftp` short-name launcher (mirrors Linux /usr/bin/aftp) ---
    ; v4.0.5 / discussion #273. The reporter could run `aeroftp-cli` but not
    ; `aftp` on Windows because no aftp.exe was shipped. On Linux deb-postinst
    ; symlinks `aftp` -> the dispatcher; Windows has no package symlink step,
    ; so copy the dispatcher to aftp.exe here. aeroftp-dispatch.exe routes by
    ; argv[0]: invoked as `aftp`, its stem != "aeroftp", so it forwards every
    ; argument straight to aeroftp-cli.exe (both live in $INSTDIR, already on
    ; PATH from the block above). The matching PREUNINSTALL block deletes it.
    IfFileExists "$INSTDIR\aeroftp-dispatch.exe" 0 _aeroftp_no_dispatch
        CopyFiles /SILENT "$INSTDIR\aeroftp-dispatch.exe" "$INSTDIR\aftp.exe"
        DetailPrint "Installed aftp.exe (copy of aeroftp-dispatch.exe)."
        Goto _aeroftp_aftp_done
    _aeroftp_no_dispatch:
        DetailPrint "WARNING: aeroftp-dispatch.exe not found in $INSTDIR; aftp.exe not created."
    _aeroftp_aftp_done:

    ; --- VC++ Runtime dependency check ---
    ; Tauri (MSVC toolchain) requires vcruntime140.dll / vcruntime140_1.dll.
    ; On clean Windows installs without VC++ Redistributable, the app crashes
    ; with STATUS_DLL_NOT_FOUND (0xC0000135). This block checks for the DLL
    ; and silently installs the redistributable if missing.
    ; Uses NSISdl (built-in NSIS plugin, same as Tauri's WebView2 bootstrapper download).
    IfFileExists "$SYSDIR\vcruntime140.dll" _vcredist_done 0
        DetailPrint "Installing Visual C++ Runtime..."
        NSISdl::download "https://aka.ms/vs/17/release/vc_redist.x64.exe" "$TEMP\vc_redist.x64.exe"
        Pop $0
        StrCmp $0 "success" 0 _vcredist_dl_failed
            ExecWait '"$TEMP\vc_redist.x64.exe" /install /quiet /norestart' $1
            DetailPrint "VC++ Runtime installer exited with code: $1"
            Delete "$TEMP\vc_redist.x64.exe"
            Goto _vcredist_done
        _vcredist_dl_failed:
            DetailPrint "VC++ Runtime download failed ($0) — install manually from https://aka.ms/vs/17/release/vc_redist.x64.exe"
    _vcredist_done:

    ; Register file associations under HKCU because Tauri's NSIS installer
    ; runs per-user by default (no admin elevation). HKLM writes from a
    ; non-elevated installer are silently dropped by Windows registry
    ; virtualisation, which is why the doc-style MIME icons we ship were
    ; never honoured: Tauri's auto-section already wrote AppIcon to
    ; HKCU\Software\Classes\<ProgID>\DefaultIcon, our HKLM overwrite
    ; never reached anywhere visible to Explorer, and HKCU's value won
    ; the HKEY_CLASSES_ROOT merge. Issue: see APPENDIX-SPRING.

    ; .aerovault
    WriteRegStr HKCU "Software\Classes\.aerovault" "" "AeroFTP.AeroVault"
    WriteRegStr HKCU "Software\Classes\.aerovault" "Content Type" "application/x-aerovault"
    WriteRegStr HKCU "Software\Classes\.aerovault" "PerceivedType" "document"

    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroVault" "" "AeroVault Encrypted Container"
    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroVault\DefaultIcon" "" "$INSTDIR\icons\mimetypes\aerovault.ico,0"
    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroVault\shell\open" "" "Open with AeroFTP"
    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroVault\shell\open\command" "" '"$INSTDIR\AeroFTP.exe" "%1"'

    ; .aeroftp (server profile export)
    WriteRegStr HKCU "Software\Classes\.aeroftp" "" "AeroFTP.Profile"
    WriteRegStr HKCU "Software\Classes\.aeroftp" "Content Type" "application/x-aeroftp"
    WriteRegStr HKCU "Software\Classes\.aeroftp" "PerceivedType" "document"

    WriteRegStr HKCU "Software\Classes\AeroFTP.Profile" "" "AeroFTP Server Profile"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Profile\DefaultIcon" "" "$INSTDIR\icons\mimetypes\aeroftp.ico,0"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Profile\shell\open" "" "Open with AeroFTP"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Profile\shell\open\command" "" '"$INSTDIR\AeroFTP.exe" "%1"'

    ; .aeroftp-keystore (encrypted keystore backup)
    WriteRegStr HKCU "Software\Classes\.aeroftp-keystore" "" "AeroFTP.Keystore"
    WriteRegStr HKCU "Software\Classes\.aeroftp-keystore" "Content Type" "application/x-aeroftp-keystore"
    WriteRegStr HKCU "Software\Classes\.aeroftp-keystore" "PerceivedType" "document"

    WriteRegStr HKCU "Software\Classes\AeroFTP.Keystore" "" "AeroFTP Keystore Backup"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Keystore\DefaultIcon" "" "$INSTDIR\icons\mimetypes\aeroftp-keystore.ico,0"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Keystore\shell\open" "" "Open with AeroFTP"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Keystore\shell\open\command" "" '"$INSTDIR\AeroFTP.exe" "%1"'

    ; .aerozip (plaintext recoverable archive)
    WriteRegStr HKCU "Software\Classes\.aerozip" "" "AeroFTP.AeroZip"
    WriteRegStr HKCU "Software\Classes\.aerozip" "Content Type" "application/x-aerozip"
    WriteRegStr HKCU "Software\Classes\.aerozip" "PerceivedType" "document"

    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroZip" "" "AeroVault Zip Archive"
    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroZip\DefaultIcon" "" "$INSTDIR\icons\mimetypes\aerozip.ico,0"
    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroZip\shell\open" "" "Open with AeroFTP"
    WriteRegStr HKCU "Software\Classes\AeroFTP.AeroZip\shell\open\command" "" '"$INSTDIR\AeroFTP.exe" "%1"'

    ; .aeroftp-script (portable batch script for `aeroftp-cli batch`)
    WriteRegStr HKCU "Software\Classes\.aeroftp-script" "" "AeroFTP.Script"
    WriteRegStr HKCU "Software\Classes\.aeroftp-script" "Content Type" "application/x-aeroftp-script"
    WriteRegStr HKCU "Software\Classes\.aeroftp-script" "PerceivedType" "document"

    WriteRegStr HKCU "Software\Classes\AeroFTP.Script" "" "AeroFTP Batch Script"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Script\DefaultIcon" "" "$INSTDIR\icons\mimetypes\aeroftp-script.ico,0"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Script\shell\open" "" "Open with AeroFTP"
    WriteRegStr HKCU "Software\Classes\AeroFTP.Script\shell\open\command" "" '"$INSTDIR\AeroFTP.exe" "%1"'

    ; MIME database entries (HKCU\Software\Classes\MIME mirrors HKLM in
    ; the merged HKCR view, so Explorer picks them up the same way).
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aerovault" "Extension" ".aerovault"
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp" "Extension" ".aeroftp"
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp-keystore" "Extension" ".aeroftp-keystore"
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aerozip" "Extension" ".aerozip"
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp-script" "Extension" ".aeroftp-script"

    ; --- "Extract here / Extract to folder" context-menu verbs (Deliverable G) ---
    ; Additive verbs only (owner decision c): never a default shell\open for the
    ; general archive formats, so their double-click Open handler is untouched.
    ; General formats attach via SystemFileAssociations\.<ext> (works regardless
    ; of which app owns the .zip/.7z/... ProgID). The double-extension tarballs
    ; .tar.gz / .tar.xz / .tar.bz2 register under their LAST component (.gz/.xz/
    ; .bz2) because that is the only handle Explorer keys associations on. Our
    ; own aero* archive ProgIDs get the verbs alongside their Open entry.
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.zip"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.7z"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.rar"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.tar"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.tgz"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.gz"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.xz"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\SystemFileAssociations\.bz2"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\AeroFTP.AeroVault"
    !insertmacro AeroFTPWriteExtractVerbs "Software\Classes\AeroFTP.AeroZip"

    ; Flush Explorer's icon cache so the doc-style MIME icons are
    ; rendered immediately after install (otherwise users would have
    ; to log out / restart Explorer to see the change). The cache file
    ; lives at $LOCALAPPDATA\IconCache.db on Win10/11; deleting it is
    ; safe (Windows rebuilds on demand) and is the documented way to
    ; force a refresh post-association change.
    Delete "$LOCALAPPDATA\IconCache.db"
    Delete "$LOCALAPPDATA\Microsoft\Windows\Explorer\iconcache_*.db"

    ; SHCNE_ASSOCCHANGED (0x08000000) — notify Explorer to refresh file associations and icons
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x0000, p 0, p 0)'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
    ; --- Snapshot pre-state for "delete all data" detection ---
    ; Recorded BEFORE Tauri's bundled uninstaller sections execute. If the
    ; user ticked the optional "Remove application data" component on the
    ; uninstaller's components page, Tauri's section will `RMDir /r` the
    ; `$APPDATA\${IDENTIFIER}` directory between this hook and POSTUNINSTALL.
    ; POSTUNINSTALL compares pre-state vs current state to know which
    ; cleanup branch to take, instead of triple-prompting the user for
    ; consent they already gave up front (issue #178 follow-up).
    StrCpy $AeroFTPAppDataPresentPre "no"
    IfFileExists "$APPDATA\com.aeroftp.AeroFTP\*.*" 0 _aeroftp_pre_appdata_done
        StrCpy $AeroFTPAppDataPresentPre "yes"
    _aeroftp_pre_appdata_done:

    ; --- Remove install dir from user PATH (HKCU) ---
    ; Mirror of the install-side EnVar::AddValue. EnVar::DeleteValue
    ; removes every occurrence of "$INSTDIR" from HKCU\Environment\Path,
    ; preserves the rest of the value intact, and is safe on PATH values
    ; of any length. See the corresponding block in NSIS_HOOK_POSTINSTALL
    ; for the rationale (issue #240, the old string-scan implementation
    ; could corrupt long PATH values).
    EnVar::SetHKCU
    EnVar::DeleteValue "Path" "$INSTDIR"
    Pop $0
    DetailPrint "EnVar::DeleteValue Path $INSTDIR -> code $0"

    ; --- Remove the `aftp` launcher and the opt-in alias bin dir (v4.0.5) ---
    ; Mirror of the POSTINSTALL additions. aftp.exe is an untracked copy the
    ; Tauri uninstaller does not know about, so delete it here (before Tauri's
    ; file loop + final RMDir) or $INSTDIR would be left behind non-empty.
    ; RMDir /r drops the bin dir together with any managed `<name>.cmd` shims
    ; alias-toggle created (satisfies "uninstall removes any managed shim"),
    ; then EnVar::DeleteValue removes its PATH entry.
    Delete "$INSTDIR\aftp.exe"
    RMDir /r "$LOCALAPPDATA\AeroFTP\bin"
    EnVar::DeleteValue "Path" "$LOCALAPPDATA\AeroFTP\bin"
    Pop $0
    DetailPrint "EnVar::DeleteValue Path $LOCALAPPDATA\AeroFTP\bin -> code $0"
    System::Call 'USER32::SendMessageTimeoutW(i 0xffff, i 0x001A, i 0, w "Environment", i 0, i 5000, *i .r3)'

    ; Remove file associations and class registrations for all 5 AeroFTP
    ; MIME types. Mirror of the install-side HKCU writes (per-user
    ; install scope). HKLM keys are also dropped on the off chance an
    ; older AeroFTP build registered there before the HKCU migration.
    DeleteRegKey HKCU "Software\Classes\.aerovault"
    DeleteRegKey HKCU "Software\Classes\AeroFTP.AeroVault"
    DeleteRegKey HKCU "Software\Classes\MIME\Database\Content Type\application/x-aerovault"
    DeleteRegKey HKCU "Software\Classes\.aeroftp"
    DeleteRegKey HKCU "Software\Classes\AeroFTP.Profile"
    DeleteRegKey HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp"
    DeleteRegKey HKCU "Software\Classes\.aeroftp-keystore"
    DeleteRegKey HKCU "Software\Classes\AeroFTP.Keystore"
    DeleteRegKey HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp-keystore"
    DeleteRegKey HKCU "Software\Classes\.aerozip"
    DeleteRegKey HKCU "Software\Classes\AeroFTP.AeroZip"
    DeleteRegKey HKCU "Software\Classes\MIME\Database\Content Type\application/x-aerozip"
    DeleteRegKey HKCU "Software\Classes\.aeroftp-script"
    DeleteRegKey HKCU "Software\Classes\AeroFTP.Script"
    DeleteRegKey HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp-script"
    ; Legacy HKLM cleanup (pre-HKCU migration installs).
    DeleteRegKey HKLM "Software\Classes\.aerovault"
    DeleteRegKey HKLM "Software\Classes\AeroFTP.AeroVault"
    DeleteRegKey HKLM "Software\Classes\MIME\Database\Content Type\application/x-aerovault"
    DeleteRegKey HKLM "Software\Classes\.aeroftp"
    DeleteRegKey HKLM "Software\Classes\AeroFTP.Profile"
    DeleteRegKey HKLM "Software\Classes\MIME\Database\Content Type\application/x-aeroftp"
    DeleteRegKey HKLM "Software\Classes\.aeroftp-keystore"
    DeleteRegKey HKLM "Software\Classes\AeroFTP.Keystore"
    DeleteRegKey HKLM "Software\Classes\MIME\Database\Content Type\application/x-aeroftp-keystore"
    DeleteRegKey HKLM "Software\Classes\.aerozip"
    DeleteRegKey HKLM "Software\Classes\AeroFTP.AeroZip"
    DeleteRegKey HKLM "Software\Classes\MIME\Database\Content Type\application/x-aerozip"
    DeleteRegKey HKLM "Software\Classes\.aeroftp-script"
    DeleteRegKey HKLM "Software\Classes\AeroFTP.Script"
    DeleteRegKey HKLM "Software\Classes\MIME\Database\Content Type\application/x-aeroftp-script"

    ; Remove the "Extract here / Extract to folder" verbs (Deliverable G).
    ; Mirror of the POSTINSTALL block: drop only our two verb subkeys, never the
    ; parent SystemFileAssociations\.<ext> key (Windows and other apps share it).
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.zip"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.7z"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.rar"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.tar"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.tgz"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.gz"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.xz"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\SystemFileAssociations\.bz2"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\AeroFTP.AeroVault"
    !insertmacro AeroFTPDeleteExtractVerbs "Software\Classes\AeroFTP.AeroZip"

    ; SHCNE_ASSOCCHANGED (0x08000000) — notify Explorer to refresh file associations and icons
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x0000, p 0, p 0)'

    ; Selective user data cleanup moved to NSIS_HOOK_POSTUNINSTALL so the
    ; hook can observe whether Tauri's optional "Remove application data"
    ; section actually ran. See the matching POSTUNINSTALL block below.
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
    ; --- Coherent user-data cleanup, choice-aware ---
    ;
    ; Silent mode (WinGet upgrade, `/S` flag) preserves user data: same
    ; contract as before (issue #128 line of reasoning). Skip everything.
    IfSilent _aeroftp_post_data_cleanup_done

    ; Decide which branch to take. Tauri's bundled NSIS template exposes
    ; an optional component on the uninstaller's "Choose components" page
    ; (labelled "Application data" / "Donnees d'application" depending on
    ; locale) which, when ticked, runs `RMDir /r "$APPDATA\${IDENTIFIER}"`
    ; as part of the standard uninstall sequence. We do not depend on
    ; that section's internal symbol name (it has changed across Tauri
    ; minors); instead we observe the side-effect: if the directory was
    ; present before our PREUNINSTALL hook and is gone by the time
    ; POSTUNINSTALL fires, the user opted in for "delete all data".
    ;
    ; "delete all"   => silently wipe the two extra paths the Tauri
    ;                   section does not know about ($APPDATA\aeroftp,
    ;                   the legacy vault location from pre-v3.7.6, and
    ;                   $LOCALAPPDATA\com.aeroftp.AeroFTP, the WebView
    ;                   cache + Cloud Filter state). No further prompts.
    ; "kept"         => fall back to the granular 3-prompt flow so the
    ;                   user can still cherry-pick what to remove.
    StrCmp $AeroFTPAppDataPresentPre "yes" 0 _aeroftp_post_granular
    IfFileExists "$APPDATA\com.aeroftp.AeroFTP\*.*" _aeroftp_post_granular 0

    ; Branch A: coherent full wipe
    DetailPrint "AeroFTP: Remove application data confirmed; wiping legacy vault and WebView caches."
    RMDir /r "$APPDATA\aeroftp"
    RMDir /r "$LOCALAPPDATA\com.aeroftp.AeroFTP"
    Goto _aeroftp_post_data_cleanup_done

    ; Branch B: granular per-area prompts
    _aeroftp_post_granular:

    ; 1) Saved servers, credentials, and vaults (legacy $APPDATA\aeroftp)
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove saved servers, credentials, and vaults?$\n$\n\
This deletes all connection profiles, stored passwords,$\n\
and AeroVault containers.$\n$\n\
Select 'No' to keep them for a future reinstall." \
        IDYES _rm_servers IDNO _skip_servers
    _rm_servers:
        RMDir /r "$APPDATA\aeroftp"
    _skip_servers:

    ; 2) AI chat history and agent memory
    ; (Skipped when Tauri already removed $APPDATA\com.aeroftp.AeroFTP via
    ; its own section: the directory is gone and the RMDir below is a
    ; no-op, but we suppress the prompt to avoid confusing the user.)
    IfFileExists "$APPDATA\com.aeroftp.AeroFTP\*.*" 0 _skip_ai_prompt
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove AI chat history and agent memory?$\n$\n\
This deletes AeroAgent conversations, tool history,$\n\
and learned context." \
        IDYES _rm_ai IDNO _skip_ai
    _rm_ai:
        RMDir /r "$APPDATA\com.aeroftp.AeroFTP"
    _skip_ai:
    _skip_ai_prompt:

    ; 3) Cache and temporary files
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove cache and temporary files?$\n$\n\
This deletes WebView cache, logs, and temp data.$\n\
Safe to remove, frees disk space." \
        IDYES _rm_cache IDNO _skip_cache
    _rm_cache:
        RMDir /r "$LOCALAPPDATA\com.aeroftp.AeroFTP"
    _skip_cache:
    _aeroftp_post_data_cleanup_done:
!macroend
