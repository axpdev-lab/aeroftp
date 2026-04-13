; AeroFTP NSIS Installer Hooks
; Post-install and pre-uninstall actions for Windows.

!macro CUSTOM_POST_INSTALL
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

    ; Register .aerovault file association
    WriteRegStr HKLM "Software\Classes\.aerovault" "" "AeroFTP.AeroVault"
    WriteRegStr HKLM "Software\Classes\.aerovault" "Content Type" "application/x-aerovault"
    WriteRegStr HKLM "Software\Classes\.aerovault" "PerceivedType" "document"

    WriteRegStr HKLM "Software\Classes\AeroFTP.AeroVault" "" "AeroVault Encrypted Container"
    WriteRegStr HKLM "Software\Classes\AeroFTP.AeroVault\DefaultIcon" "" "$INSTDIR\icons\mimetypes\aerovault.ico,0"
    WriteRegStr HKLM "Software\Classes\AeroFTP.AeroVault\shell\open" "" "Open with AeroFTP"
    WriteRegStr HKLM "Software\Classes\AeroFTP.AeroVault\shell\open\command" "" '"$INSTDIR\AeroFTP.exe" "%1"'

    ; Register MIME type in Windows MIME database
    WriteRegStr HKLM "Software\Classes\MIME\Database\Content Type\application/x-aerovault" "Extension" ".aerovault"

    ; SHCNE_ASSOCCHANGED (0x08000000) — notify Explorer to refresh file associations and icons
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x0000, p 0, p 0)'
!macroend

!macro CUSTOM_PRE_UNINSTALL
    ; Remove .aerovault file association and class registration
    DeleteRegKey HKLM "Software\Classes\.aerovault"
    DeleteRegKey HKLM "Software\Classes\AeroFTP.AeroVault"
    DeleteRegKey HKLM "Software\Classes\MIME\Database\Content Type\application/x-aerovault"

    ; SHCNE_ASSOCCHANGED (0x08000000) — notify Explorer to refresh file associations and icons
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x0000, p 0, p 0)'

    ; --- Selective user data cleanup on uninstall ---
    ; Three separate prompts let the user choose exactly what to remove.

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
!macroend
