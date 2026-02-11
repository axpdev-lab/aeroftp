; AeroFTP NSIS Installer Hooks
; Post-install and pre-uninstall actions for Windows.
;
; Currently a stub — prepared for future Shell Icon Overlay COM DLL registration
; if Cloud Filter API proves insufficient for certain Explorer versions.

!macro CUSTOM_POST_INSTALL
    ; Future: register COM DLL for Shell Icon Overlay
    ; regsvr32 /s "$INSTDIR\aerocloud_overlay.dll"
!macroend

!macro CUSTOM_PRE_UNINSTALL
    ; Future: unregister COM DLL for Shell Icon Overlay
    ; regsvr32 /u /s "$INSTDIR\aerocloud_overlay.dll"
!macroend
