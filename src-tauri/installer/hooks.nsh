; AeroFTP NSIS Installer Hooks
; Post-install and pre-uninstall actions for Windows.

!macro CUSTOM_POST_INSTALL
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
!macroend
