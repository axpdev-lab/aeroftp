; AeroFTP issue #454: explicit and durable Windows integration choices.
!include nsDialogs.nsh
!include StrFunc.nsh
${StrRep}
${UnStrRep}
Var AeroFTPOwnedFormats
Var AeroFTPArchiveFormats
Var AeroFTPOwnedCheckbox
Var AeroFTPArchivesCheckbox

Function AeroFTPReadAssociationPreference
    Pop $0
    ClearErrors
    ReadRegDWORD $1 HKCU "Software\AeroFTP\Settings" "$0"
    IfErrors _aeroftp_pref_presence 0
    StrCmp $1 1 _aeroftp_pref_enabled _aeroftp_pref_disabled
    _aeroftp_pref_presence:
        StrCpy $2 0
    _aeroftp_pref_enum:
        ClearErrors
        EnumRegValue $1 HKCU "Software\AeroFTP\Settings" $2
        IfErrors _aeroftp_pref_enabled 0
        ; Existing but unreadable/malformed values are disabled.
        StrCmp $1 $0 _aeroftp_pref_disabled 0
        IntOp $2 $2 + 1
        Goto _aeroftp_pref_enum
    _aeroftp_pref_disabled:
        Push 0
        Return
    _aeroftp_pref_enabled:
        Push 1
FunctionEnd

Function AeroFTPReadAssociationChoices
    Push "OwnedFormats"
    Call AeroFTPReadAssociationPreference
    Pop $AeroFTPOwnedFormats
    Push "ArchiveFormats"
    Call AeroFTPReadAssociationPreference
    Pop $AeroFTPArchiveFormats
    ClearErrors
    ${GetOptions} $CMDLINE "/NOASSOC" $0
    IfErrors _aeroftp_noassoc_done 0
        StrCpy $AeroFTPOwnedFormats 0
        StrCpy $AeroFTPArchiveFormats 0
    _aeroftp_noassoc_done:
FunctionEnd

; Delete a value only if it is still ours; never delete a shared parent tree.
!macro AeroFTPDeleteMatchingValue KEY NAME VALUE
    Push $0
    ClearErrors
    ReadRegStr $0 HKCU "${KEY}" "${NAME}"
    IfErrors +6 0
    StrCmp $0 "${VALUE}" 0 +5
        DeleteRegValue HKCU "${KEY}" "${NAME}"
        IfErrors 0 +3
        SetErrorLevel 1
        Abort "Could not remove the Windows integration entry."
    Pop $0
!macroend

!macro AeroFTPRemoveLegacyProgId PROGID
    ReadRegStr $0 HKCU "Software\Classes\${PROGID}\shell\open\command" ""
    StrCmp $0 '$INSTDIR\AeroFTP.exe $\"%1$\"' 0 +2
        DeleteRegKey HKCU "Software\Classes\${PROGID}"
    StrCmp $0 '$\"$INSTDIR\AeroFTP.exe$\" $\"%1$\"' 0 +2
        DeleteRegKey HKCU "Software\Classes\${PROGID}"
!macroend

!macro AeroFTPRemoveOpenWithHistory EXT
    Push $0
    Push $1
    Push $2
    Push $3
    ReadRegStr $3 HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithList" "MRUList"
    StrCpy $0 0
    _aeroftp_history_loop_${EXT}:
        ClearErrors
        EnumRegValue $1 HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithList" $0
        IfErrors _aeroftp_history_done_${EXT} 0
        StrCmp $1 "MRUList" _aeroftp_history_next_${EXT} 0
        ReadRegStr $2 HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithList" "$1"
        StrCmp $2 "AeroFTP.exe" 0 _aeroftp_history_next_${EXT}
        DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithList" "$1"
        !ifdef AEROFTP_HISTORY_UNINSTALL
            ${UnStrRep} $3 $3 $1 ""
        !else
            ${StrRep} $3 $3 $1 ""
        !endif
        Goto _aeroftp_history_loop_${EXT}
    _aeroftp_history_next_${EXT}:
        IntOp $0 $0 + 1
        Goto _aeroftp_history_loop_${EXT}
    _aeroftp_history_done_${EXT}:
        StrCmp $3 "" _aeroftp_history_empty_${EXT} 0
        WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithList" "MRUList" "$3"
        Goto _aeroftp_history_finish_${EXT}
    _aeroftp_history_empty_${EXT}:
        DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithList" "MRUList"
    _aeroftp_history_finish_${EXT}:
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithList"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}"
    Pop $3
    Pop $2
    Pop $1
    Pop $0
!macroend

!macro AeroFTPRemoveArchiveHistory
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.zip\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.zip\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "zip"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.7z\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.7z\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "7z"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.rar\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.rar\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "rar"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.tar\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.tar\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "tar"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.tgz\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.tgz\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "tgz"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.gz\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.gz\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "gz"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.xz\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.xz\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "xz"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.bz2\OpenWithProgids" "AeroFTP.Archive"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.bz2\OpenWithProgids"
    !insertmacro AeroFTPRemoveOpenWithHistory "bz2"
!macroend

!macro AeroFTPRemoveLegacyType EXT PROGID MIME
    Push $0
    Push $1
    ReadRegStr $0 HKCU "Software\Classes\.${EXT}" ""
    StrCmp $0 "${PROGID}" _aeroftp_restore_${EXT} 0
    StrCmp $0 "${EXT}" _aeroftp_restore_${EXT} 0
    StrCmp $0 "AeroFTP.${EXT}" 0 _aeroftp_restored_${EXT}
    _aeroftp_restore_${EXT}:
        ReadRegStr $1 HKCU "Software\Classes\.${EXT}" "${EXT}_backup"
        StrCmp $1 "" _aeroftp_clear_default_${EXT} 0
        StrCmp $1 "${PROGID}" _aeroftp_clear_default_${EXT} 0
        StrCmp $1 "${EXT}" _aeroftp_clear_default_${EXT} 0
        StrCmp $1 "AeroFTP.${EXT}" _aeroftp_clear_default_${EXT} 0
        WriteRegStr HKCU "Software\Classes\.${EXT}" "" "$1"
        Goto _aeroftp_restored_${EXT}
    _aeroftp_clear_default_${EXT}:
        DeleteRegValue HKCU "Software\Classes\.${EXT}" ""
    _aeroftp_restored_${EXT}:
    DeleteRegValue HKCU "Software\Classes\.${EXT}" "${EXT}_backup"
    ReadRegStr $1 HKCU "Software\Classes\.${EXT}" "Content Type"
    StrCmp $1 "${MIME}" 0 _aeroftp_metadata_done_${EXT}
    DeleteRegValue HKCU "Software\Classes\.${EXT}" "Content Type"
    !insertmacro AeroFTPDeleteMatchingValue "Software\Classes\.${EXT}" "PerceivedType" "document"
    _aeroftp_metadata_done_${EXT}:
    DeleteRegValue HKCU "Software\Classes\.${EXT}\OpenWithProgids" "${EXT}"
    DeleteRegValue HKCU "Software\Classes\.${EXT}\OpenWithProgids" "AeroFTP.${EXT}"
    DeleteRegKey /ifempty HKCU "Software\Classes\.${EXT}\OpenWithProgids"
    DeleteRegKey /ifempty HKCU "Software\Classes\.${EXT}"
    ; Bare ProgIDs were generated by the old Tauri template, never by other apps.
    !insertmacro AeroFTPRemoveLegacyProgId "${EXT}"
    !insertmacro AeroFTPRemoveLegacyProgId "${PROGID}"
    !insertmacro AeroFTPRemoveLegacyProgId "AeroFTP.${EXT}"
    !insertmacro AeroFTPDeleteMatchingValue "Software\Classes\MIME\Database\Content Type\${MIME}" "Extension" ".${EXT}"
    DeleteRegKey /ifempty HKCU "Software\Classes\MIME\Database\Content Type\${MIME}"
    ; Do not delete FileExts or UserChoice: Windows owns the user's defaults.
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithProgids" "${EXT}"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithProgids" "${PROGID}"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithProgids" "AeroFTP.${EXT}"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}\OpenWithProgids"
    DeleteRegKey /ifempty HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.${EXT}"
    !insertmacro AeroFTPRemoveOpenWithHistory "${EXT}"
    Pop $1
    Pop $0
!macroend
!macro AeroFTPRemoveLegacyAssociations
    !insertmacro AeroFTPRemoveLegacyType "aerovault" "AeroFTP.AeroVault" "application/x-aerovault"
    !insertmacro AeroFTPRemoveLegacyType "aeroftp" "AeroFTP.Profile" "application/x-aeroftp"
    !insertmacro AeroFTPRemoveLegacyType "aeroftp-keystore" "AeroFTP.Keystore" "application/x-aeroftp-keystore"
    !insertmacro AeroFTPRemoveLegacyType "aerozip" "AeroFTP.AeroZip" "application/x-aerozip"
    !insertmacro AeroFTPRemoveLegacyType "aeroftp-script" "AeroFTP.Script" "application/x-aeroftp-script"
!macroend
!macro AeroFTPApplyAssociationChoices
    StrCmp $AeroFTPOwnedFormats 1 _aeroftp_owned_on 0
        !insertmacro AeroFTPRemoveLegacyAssociations
        !insertmacro AeroFTPUnregister_owned
        Goto _aeroftp_owned_done
    _aeroftp_owned_on:
        !insertmacro AeroFTPRegister_owned
    _aeroftp_owned_done:
    StrCmp $AeroFTPArchiveFormats 1 _aeroftp_archives_on 0
        !insertmacro AeroFTPUnregister_archives
        !insertmacro AeroFTPRemoveArchiveHistory
        Goto _aeroftp_archives_done
    _aeroftp_archives_on:
        !insertmacro AeroFTPRegister_archives
    _aeroftp_archives_done:
    StrCmp $AeroFTPOwnedFormats 1 _aeroftp_common_on 0
    StrCmp $AeroFTPArchiveFormats 1 _aeroftp_common_on 0
        !insertmacro AeroFTPUnregister_common
        Goto _aeroftp_common_done
    _aeroftp_common_on:
        !insertmacro AeroFTPRegister_common
    _aeroftp_common_done:
    ClearErrors
    WriteRegDWORD HKCU "Software\AeroFTP\Settings" "OwnedFormats" $AeroFTPOwnedFormats
    IfErrors _aeroftp_registration_failed 0
    WriteRegDWORD HKCU "Software\AeroFTP\Settings" "ArchiveFormats" $AeroFTPArchiveFormats
    IfErrors _aeroftp_registration_failed 0
    Goto _aeroftp_registration_done
    _aeroftp_registration_failed:
        SetErrorLevel 1
        Abort "Could not apply Windows integration. Setup did not complete."
    _aeroftp_registration_done:
!macroend
