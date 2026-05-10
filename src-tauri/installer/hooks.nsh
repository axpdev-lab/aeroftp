; AeroFTP NSIS Installer Hooks
; Post-install and pre-uninstall actions for Windows.

; ── StrContains — in-place substring search (no !include needed) ──
; Usage:   Push <needle>
;          Push <haystack>
;          Call StrContains
;          Pop  $result   ; "yes" if needle is found inside haystack, else "no"
;
; Kept inline in hooks.nsh so the installer does not require the
; StrFunc.nsh plugin or any external lib, matching the convention of
; the VC++ runtime block below.
Function StrContains
  Exch $R1 ; haystack
  Exch
  Exch $R2 ; needle
  Push $R3
  Push $R4
  Push $R5
  StrLen $R4 $R2
  StrCpy $R5 0
  StrCpy $R3 ""
_scan_loop:
  StrCpy $R3 $R1 $R4 $R5
  StrCmp $R3 $R2 _found
  StrCmp $R3 "" _not_found
  IntOp $R5 $R5 + 1
  Goto _scan_loop
_found:
  StrCpy $R3 "yes"
  Goto _done
_not_found:
  StrCpy $R3 "no"
_done:
  Pop $R5
  Pop $R4
  StrCpy $R1 $R3
  Pop $R3
  Pop $R2
  Exch $R1
FunctionEnd

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

; CRITICAL: Tauri's bundled installer.nsi invokes the four hooks below by
; the names NSIS_HOOK_{PRE,POST}{INSTALL,UNINSTALL}, gated by an
; `!ifmacrodef`. From v3.6.2 (commit e4d7868e) through v3.6.3 we shipped
; these as CUSTOM_PRE_INSTALL / CUSTOM_POST_INSTALL / CUSTOM_PRE_UNINSTALL,
; which `!ifmacrodef` skipped silently. Result: every hook in this file —
; the HKCU PATH registration, the .aerovault association, the VC++
; Runtime bootstrap, the desktop-shortcut respect logic — was inert in
; every shipped Windows installer. See bug report
; docs/dev/aeroftp-windows-path-hook-bug-report-2026-04-25.md.
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
    ; PR-T11 follow-up. The Tauri per-user installer drops binaries in
    ; %LOCALAPPDATA%\AeroFTP\ but historically never registered that
    ; directory in HKCU\Environment\Path. Result: the VS Code MCP
    ; extension, the terminal, and any tool that relies on PATH
    ; resolution could not locate aeroftp-cli, even though it was on
    ; disk and worked fine when invoked by absolute path. Fix: append
    ; $INSTDIR to HKCU Path if not already present, then broadcast
    ; WM_SETTINGCHANGE so processes started after the installer (but
    ; within the same session) pick up the new value without a shell
    ; restart. Idempotent — reinstall or repair does nothing if the
    ; entry is already there.
    ReadRegStr $0 HKCU "Environment" "Path"
    StrCpy $1 ";$0;"          ; wrap haystack so full-segment match disambiguates
    Push ";$INSTDIR;"         ; needle
    Push $1                   ; haystack
    Call StrContains
    Pop $2
    StrCmp $2 "yes" _aeroftp_path_done 0
        StrCmp $0 "" 0 _aeroftp_path_append
            WriteRegExpandStr HKCU "Environment" "Path" "$INSTDIR"
            Goto _aeroftp_path_broadcast
        _aeroftp_path_append:
            WriteRegExpandStr HKCU "Environment" "Path" "$0;$INSTDIR"
        _aeroftp_path_broadcast:
            ; WM_SETTINGCHANGE = 0x001A — same signal Inno Setup's
            ; ChangesEnvironment=yes emits. Running shells (Explorer,
            ; VS Code, PowerShell via integrated terminal) get a
            ; chance to refresh without logoff.
            System::Call 'USER32::SendMessageTimeoutW(i 0xffff, i 0x001A, i 0, w "Environment", i 0, i 5000, *i .r3)'
            ; PowerShell sessions started before this install caches its
            ; environment at launch, so even after WM_SETTINGCHANGE they
            ; cannot resolve `aeroftp-cli`. A short notice in the install
            ; log (and visible MessageBox in interactive mode) saves the
            ; user some confusion. Issue #125.
            DetailPrint "Added $INSTDIR to PATH. Open a NEW terminal to run 'aeroftp-cli'."
    _aeroftp_path_done:

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

    ; MIME database entries (HKCU\Software\Classes\MIME mirrors HKLM in
    ; the merged HKCR view, so Explorer picks them up the same way).
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aerovault" "Extension" ".aerovault"
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp" "Extension" ".aeroftp"
    WriteRegStr HKCU "Software\Classes\MIME\Database\Content Type\application/x-aeroftp-keystore" "Extension" ".aeroftp-keystore"

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
    ; --- Remove install dir from user PATH (HKCU) ---
    ; Mirror of NSIS_HOOK_POSTINSTALL. Read the current PATH, build a
    ; new value where ";$INSTDIR" (with or without the trailing ";")
    ; is stripped, then broadcast WM_SETTINGCHANGE so new processes
    ; don't see the defunct entry. Keeps HKCU Path tidy after
    ; uninstall and matches installer-contract expectations.
    ReadRegStr $0 HKCU "Environment" "Path"
    StrCmp $0 "" _aeroftp_unpath_done 0
        StrCpy $1 ";$0"                 ; left-pad haystack with ';' to catch first-entry case
        StrCpy $2 ";$INSTDIR"           ; needle
        StrLen $3 $2
        StrCpy $4 ""                    ; accumulator — new path value
        StrCpy $5 0                     ; scan cursor
_aeroftp_unpath_scan:
        StrCpy $6 $1 $3 $5
        StrCmp $6 $2 _aeroftp_unpath_hit
        StrCmp $6 "" _aeroftp_unpath_finish
        StrCpy $6 $1 1 $5
        StrCpy $4 "$4$6"
        IntOp $5 $5 + 1
        Goto _aeroftp_unpath_scan
_aeroftp_unpath_hit:
        IntOp $5 $5 + $3                ; skip ;$INSTDIR (and optional trailing ;)
        StrCpy $6 $1 1 $5
        StrCmp $6 ";" 0 _aeroftp_unpath_scan
        IntOp $5 $5 + 1
        Goto _aeroftp_unpath_scan
_aeroftp_unpath_finish:
        ; $4 starts with the leading ';' we prepended; drop it.
        StrCpy $4 $4 "" 1
        WriteRegExpandStr HKCU "Environment" "Path" "$4"
        System::Call 'USER32::SendMessageTimeoutW(i 0xffff, i 0x001A, i 0, w "Environment", i 0, i 5000, *i .r3)'
_aeroftp_unpath_done:

    ; Remove file associations and class registrations for all 3 AeroFTP
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

    ; SHCNE_ASSOCCHANGED (0x08000000) — notify Explorer to refresh file associations and icons
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x0000, p 0, p 0)'

    ; --- Selective user data cleanup on uninstall ---
    ; Three separate prompts let the user choose exactly what to remove.
    ;
    ; Silent mode (e.g. WinGet's `upgrade` flow, which runs the old
    ; installer's uninstaller silently before launching the new one) MUST
    ; preserve user data — otherwise every non-interactive upgrade would
    ; either pop blocking modal dialogs or, worse, silently wipe the
    ; user's saved servers, AI chats and cache. Skip the whole section
    ; when ${Silent} is true and let the user reinstall keep everything.
    IfSilent _aeroftp_data_cleanup_done

    ; 1) Saved servers, credentials, and vaults
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
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove AI chat history and agent memory?$\n$\n\
This deletes AeroAgent conversations, tool history,$\n\
and learned context." \
        IDYES _rm_ai IDNO _skip_ai
    _rm_ai:
        ; Tauri app data — AI chat DB, ai_history.json, agent_memory.db
        RMDir /r "$APPDATA\com.aeroftp.AeroFTP"
    _skip_ai:

    ; 3) Cache and temporary files
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove cache and temporary files?$\n$\n\
This deletes WebView cache, logs, and temp data.$\n\
Safe to remove, frees disk space." \
        IDYES _rm_cache IDNO _skip_cache
    _rm_cache:
        RMDir /r "$LOCALAPPDATA\com.aeroftp.AeroFTP"
    _skip_cache:
    _aeroftp_data_cleanup_done:
!macroend
