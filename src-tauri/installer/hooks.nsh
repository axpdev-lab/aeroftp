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
; G82: the two application ids this file has to know about. The id changed to
; `app.aeroftp.AeroFTP` in #814 while these hooks kept naming the old one, so
; everything here watched a directory Tauri no longer touches; on an upgraded
; machine the legacy directory also holds real data (the AI chat history and
; the agent memory), which Tauri's own section never removes.
;
; They are written out rather than taken from `${IDENTIFIER}`: that macro
; appears in this file only inside comments, so there is no evidence it is
; defined for hook code, and a rebuild would fail on a missing symbol in a way
; that reads like a syntax error.
;
; There used to be two snapshot variables here as well, recording whether each
; `$APPDATA` directory existed before the uninstaller sections ran, so that
; POSTUNINSTALL could infer the user's choice from whether one of them had
; disappeared. They are gone: the choice is read directly now, see the decision
; in NSIS_HOOK_POSTUNINSTALL. An inference that only works when there is
; something to observe is worth less than the value itself.
!define AEROFTP_APPID_CURRENT "app.aeroftp.AeroFTP"
!define AEROFTP_APPID_LEGACY "com.aeroftp.AeroFTP"

; CRITICAL: Tauri's bundled installer.nsi invokes the four hooks below by
; the names NSIS_HOOK_{PRE,POST}{INSTALL,UNINSTALL}, gated by an
; `!ifmacrodef`. From v3.6.2 (commit e4d7868e) through v3.6.3 we shipped
; these as CUSTOM_PRE_INSTALL / CUSTOM_POST_INSTALL / CUSTOM_PRE_UNINSTALL,
; which `!ifmacrodef` skipped silently. Result: every hook in this file —
; the HKCU PATH registration, the .aerovault association, the VC++
; Runtime bootstrap, the desktop-shortcut respect logic — was inert in
; every shipped Windows installer. See bug report
; docs/dev/aeroftp-windows-path-hook-bug-report-2026-04-25.md.
!include "${__FILEDIR__}\association-choices.nsh"
!include "${__FILEDIR__}\associations-generated.nsh"

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

    ; Apply the same persisted choices on normal, silent and updater installs.
    !insertmacro AeroFTPApplyAssociationChoices

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
    !define AEROFTP_HISTORY_UNINSTALL
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

    ; Upgrades preserve the user's integration state until the new installer
    ; applies the persisted choices. Full uninstall removes only our values.
    StrCmp $UpdateMode "1" _aeroftp_keep_associations 0
        !insertmacro AeroFTPRemoveLegacyAssociations
        !insertmacro AeroFTPUnregister_owned
        !insertmacro AeroFTPUnregister_archives
        !insertmacro AeroFTPUnregister_common
        !insertmacro AeroFTPRemoveArchiveHistory
        System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x0000, p 0, p 0)'
    _aeroftp_keep_associations:
    !undef AEROFTP_HISTORY_UNINSTALL

    ; Selective user data cleanup moved to NSIS_HOOK_POSTUNINSTALL so the
    ; hook can observe whether Tauri's optional "Remove application data"
    ; section actually ran. See the matching POSTUNINSTALL block below.
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
    ; The custom template has no APP_UNASSOCIATE calls to recreate empty
    ; extension keys. Association cleanup is now owned by PREUNINSTALL above.

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
    ;                   both `$LOCALAPPDATA` application-id directories, the WebView
    ;                   cache + Cloud Filter state). No further prompts.
    ; "kept"         => fall back to the granular 3-prompt flow so the
    ;                   user can still cherry-pick what to remove.
    ; G82: the answer is READ, not inferred. Tauri's generated `installer.nsi`
    ; declares `$DeleteAppDataCheckboxState` and fills it from the checkbox when
    ; the user leaves the confirm page, which happens before `Section Uninstall`
    ; begins; both hooks are expanded inside that section, so the value is
    ; already there when this runs. Found by W2 on 2026-09-18 by opening the
    ; script a build on her machine had generated, instead of reasoning about
    ; what the section does.
    ;
    ; What this replaces, and why the replacement is not a refinement. The old
    ; signal was "a directory Tauri deletes was here before and is gone now",
    ; which cannot be asked AT ALL when that directory does not exist. Raised by
    ; CodeRabbit on the pull request that repaired the application id: on a
    ; machine that upgraded from a build carrying the legacy id and is
    ; uninstalled before the new one ever runs, only the legacy directory
    ; exists, Tauri's section deletes nothing, and a ticked box still routed to
    ; the granular prompts. Reading the answer removes the whole class of state
    ; the inference could not cover, rather than covering one more state.
    ;
    ; Fragility, declared rather than left to be discovered: this is an internal
    ; detail of the bundler template and a Tauri upgrade can rename it. The
    ; failure mode is the loud one: the Windows bundle stops compiling on an
    ; unknown variable and the build job says so before anything ships. The
    ; design it replaces depended on an internal detail at least as fragile,
    ; that the same section does `RmDir /r` on that exact path, and that one
    ; fails in silence.
    ;
    ; Plain `StrCmp` rather than LogicLib's `${If}`: this file has never
    ; depended on LogicLib being included by whatever wraps it.
    StrCmp $DeleteAppDataCheckboxState "1" _aeroftp_post_full_wipe _aeroftp_post_granular

    ; Branch A: the caches go, the vault still asks.
    ;
    ; Owner decision, 2026-09-17: making this branch reachable again must not
    ; widen what the uninstaller destroys without asking. The caches are
    ; disposable and go silently, as they always did here. `$APPDATA\aeroftp`
    ; holds the vault, the saved credentials and the AeroVault containers, and
    ; it keeps its own prompt: until today this branch never ran, so that
    ; wipe has never actually been silent, and repairing the detection is not
    ; a reason to start. The user who ticked the box still gets asked once
    ; about the one thing that cannot be rebuilt.
    _aeroftp_post_full_wipe:
    DetailPrint "AeroFTP: Remove application data confirmed; wiping caches and agent data."
    RMDir /r "$LOCALAPPDATA\${AEROFTP_APPID_CURRENT}"
    RMDir /r "$LOCALAPPDATA\${AEROFTP_APPID_LEGACY}"
    ; Tauri's own section removes both roots for the CURRENT id, `$APPDATA` and
    ; `$LOCALAPPDATA`, and nothing for the legacy one. Read in the generated
    ; `installer.nsi` by W2 on 2026-09-18, which corrects what this comment used
    ; to say: it claimed the section touched `$APPDATA` alone, and the two
    ; current-id lines here were described as one no-op and one real deletion
    ; when in fact both are no-ops.
    ;
    ; What that does NOT change is why the legacy lines exist: on an upgraded
    ; machine the legacy `$APPDATA` directory holds the AI chat history and the
    ; agent memory, and it used to survive a ticked "Remove application data"
    ; with no one naming it. The current-id lines stay because a no-op costs
    ; nothing and a future template that stops removing one of them would
    ; otherwise reopen the same hole in silence. The granular branch covers the
    ; same pair through its own prompt.
    ;
    ; Owner decision, 2026-09-17: this goes silently here, like the caches.
    ; The tick is the consent, and only the vault, which is the thing that
    ; cannot be rebuilt from anywhere, is worth a second question.
    RMDir /r "$APPDATA\${AEROFTP_APPID_CURRENT}"
    RMDir /r "$APPDATA\${AEROFTP_APPID_LEGACY}"
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove saved servers, credentials, and vaults?$\n$\n\
This deletes all connection profiles, stored passwords,$\n\
and AeroVault containers.$\n$\n\
Select 'No' to keep them for a future reinstall." \
        IDYES _rm_servers_full IDNO _aeroftp_post_data_cleanup_done
    _rm_servers_full:
        RMDir /r "$APPDATA\aeroftp"
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
    ; (Skipped when Tauri already removed the current-id $APPDATA directory via
    ; its own section: the directory is gone and the RMDir below is a
    ; no-op, but we suppress the prompt to avoid confusing the user.)
    ;
    ; G82: each directory is probed twice, `\*.*` first and then the bare path.
    ; The wildcard form is the one this file has always used and the one NSIS
    ; documents for directories, but it asks "does this directory hold
    ; anything", so an existing but empty shell answers no and the prompt would
    ; be skipped for a directory that is still there.
    ;
    ; W0 measured the wildcard form on Windows 10 on 2026-09-17 and it DOES
    ; report an empty directory as present: `FindFirstFile`, which
    ; `IfFileExists` uses for a wildcard path, finds the `.` entry NTFS keeps in
    ; every directory. Probed through P/Invoke with a negative control, so the
    ; probe is known to be able to say no: empty dir FOUND, dir with a file
    ; FOUND, missing dir NOT FOUND with GetLastError 3. One step stays inferred
    ; rather than executed, because neither machine had an NSIS toolchain: the
    ; measurement covers the Win32 primitive and not `IfFileExists` itself. The
    ; second probe therefore stays as deliberate redundancy for that one step,
    ; and not as a hedge against something unknown.
    IfFileExists "$APPDATA\${AEROFTP_APPID_CURRENT}\*.*" _aeroftp_ai_prompt 0
    IfFileExists "$APPDATA\${AEROFTP_APPID_CURRENT}" _aeroftp_ai_prompt 0
    IfFileExists "$APPDATA\${AEROFTP_APPID_LEGACY}\*.*" _aeroftp_ai_prompt 0
    IfFileExists "$APPDATA\${AEROFTP_APPID_LEGACY}" 0 _skip_ai_prompt
    _aeroftp_ai_prompt:
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove AI chat history and agent memory?$\n$\n\
This deletes AeroAgent conversations, tool history,$\n\
and learned context." \
        IDYES _rm_ai IDNO _skip_ai
    _rm_ai:
        RMDir /r "$APPDATA\${AEROFTP_APPID_CURRENT}"
        RMDir /r "$APPDATA\${AEROFTP_APPID_LEGACY}"
    _skip_ai:
    _skip_ai_prompt:

    ; 3) Cache and temporary files
    MessageBox MB_YESNO|MB_ICONQUESTION \
        "Remove cache and temporary files?$\n$\n\
This deletes WebView cache, logs, and temp data.$\n\
Safe to remove, frees disk space." \
        IDYES _rm_cache IDNO _skip_cache
    _rm_cache:
        RMDir /r "$LOCALAPPDATA\${AEROFTP_APPID_CURRENT}"
        RMDir /r "$LOCALAPPDATA\${AEROFTP_APPID_LEGACY}"
    _skip_cache:
    _aeroftp_post_data_cleanup_done:
!macroend
