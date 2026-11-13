Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "WinVer.nsh"
!include "FileFunc.nsh"

!ifndef PACKAGE_DIR
  !error "Define PACKAGE_DIR with the packaged launcher folder."
!endif
!ifndef OUTPUT_FILE
  !error "Define OUTPUT_FILE with the installer destination."
!endif
!ifndef APP_VERSION
  !error "Define APP_VERSION with the launcher package version."
!endif

!ifndef UNINSTALL_KEY
  !define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\CrystalLauncher"
!endif
!ifndef SHORTCUT_NAME
  !define SHORTCUT_NAME "Crystal Launcher"
!endif
Name "Crystal Launcher"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\Crystal Launcher"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor lzma
AllowSkipFiles off
BrandingText "Crystal Launcher"
; Version information on Setup itself (APP_VERSION is x.y.z; Windows wants four parts).
VIProductVersion "${APP_VERSION}.0"
VIFileVersion "${APP_VERSION}.0"
VIAddVersionKey /LANG=0 "ProductName" "Crystal Launcher"
VIAddVersionKey /LANG=0 "CompanyName" "Crystal Launcher project"
VIAddVersionKey /LANG=0 "FileDescription" "Crystal Launcher Setup"
VIAddVersionKey /LANG=0 "FileVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=0 "ProductVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=0 "LegalCopyright" "GPL-3.0-only. FINAL FANTASY XI and PlayOnline are trademarks of Square Enix."
Icon "${__FILEDIR__}\..\assets\crystal.ico"
UninstallIcon "${__FILEDIR__}\..\assets\crystal.ico"

!define MUI_ABORTWARNING
!define MUI_CUSTOMFUNCTION_ABORT RollbackInstall
!define MUI_WELCOMEPAGE_TEXT "Install Crystal Launcher ${APP_VERSION} for your Windows account.$\r$\n$\r$\nSaved accounts and settings are kept separately from the launcher installation."
!insertmacro MUI_PAGE_WELCOME
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE CheckDestination
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Var StageDir
Var CreatedDestination
Var CreatedHostDirectory
Var Transaction
Var BackupFiles
Var InstalledFiles
Var ShortcutNew
Var ShortcutBackup
Var ShortcutBackedUp
Var ShortcutInstalled
Var DesktopShortcutNew
Var DesktopShortcutBackup
Var DesktopShortcutBackedUp
Var DesktopShortcutInstalled
Var RegistryParent
Var RegistryName
Var RegistryHandle
Var RegistryNew
Var RegistryBackup
Var RegistryStaged
Var RegistryExisted
Var RegistryBackedUp
Var RegistryInstalled
Var RollbackFailed
Var SetupMutex

; Only these seven package files are ever replaced or removed. A failed rename
; leaves the original in place; a later failure restores the exact saved file.
!macro BackupFile NAME BIT
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR\${NAME}") i.r0 ?e'
  Pop $1
  ${If} $0 == -1
    StrCpy $0 $1
    ${If} $0 != 2
    ${AndIf} $0 != 3
      Goto install_failed
    ${EndIf}
  ${Else}
    IntOp $0 $0 & 0x410 ; Reject directories and reparse points.
    ${If} $0 != 0
      Goto install_failed
    ${EndIf}
    ClearErrors
    Rename "$INSTDIR\${NAME}" "$StageDir\previous\${NAME}"
    IfErrors install_failed
    IntOp $BackupFiles $BackupFiles | ${BIT}
  ${EndIf}
!macroend

!macro InstallFile NAME BIT
  ClearErrors
  Rename "$StageDir\new\${NAME}" "$INSTDIR\${NAME}"
  IfErrors install_failed
  IntOp $InstalledFiles $InstalledFiles | ${BIT}
!macroend

!macro RestoreFile NAME BIT
  IntOp $0 $InstalledFiles & ${BIT}
  ${If} $0 != 0
    ClearErrors
    Delete "$INSTDIR\${NAME}"
    ${If} ${Errors}
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${EndIf}
  IntOp $0 $BackupFiles & ${BIT}
  ${If} $0 != 0
    ClearErrors
    Rename "$StageDir\previous\${NAME}" "$INSTDIR\${NAME}"
    ${If} ${Errors}
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${EndIf}
!macroend

!macro DeletePackage DIRECTORY
  Delete "${DIRECTORY}\crystal\pol.exe"
  RMDir "${DIRECTORY}\crystal"
  Delete "${DIRECTORY}\Crystal-Launcher.exe"
  Delete "${DIRECTORY}\README.md"
  Delete "${DIRECTORY}\LICENSE"
  Delete "${DIRECTORY}\THIRD_PARTY.md"
  Delete "${DIRECTORY}\THIRD_PARTY_LICENSES.txt"
  Delete "${DIRECTORY}\Uninstall.exe"
  RMDir "${DIRECTORY}"
!macroend

!macro StageReg TYPE NAME VALUE
  ClearErrors
  WriteReg${TYPE} HKCU "$RegistryParent\$RegistryNew" "${NAME}" "${VALUE}"
  IfErrors install_failed
!macroend

!macro ClaimSetupMutex
  System::Call 'kernel32::CreateMutexW(p 0, i 0, w "Local\${SHORTCUT_NAME}.Setup") p.s ?e'
  Pop $0
  Pop $SetupMutex
  ${If} $SetupMutex == 0
  ${OrIf} $0 == 183
    MessageBox MB_OK|MB_ICONSTOP "Another Crystal setup or uninstall is already running. Close it and try again." /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
!macroend

Function .onInit
  SetShellVarContext current
  SetRegView 64
  !insertmacro ClaimSetupMutex
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "Crystal Launcher requires 64-bit Windows." /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  ${IfNot} ${AtLeastWin10}
    MessageBox MB_OK|MB_ICONSTOP "Crystal Launcher requires Windows 10 or later." /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  ; NSIS discards an invalid /D= path before .onInit and may fall back to the
  ; registered folder. Honor the requested path or fail instead of upgrading a
  ; different installation. /D= is deliberately absent from NSIS's $CMDLINE.
  System::Call 'kernel32::GetCommandLineW() w.r0'
  ClearErrors
  ${GetOptionsS} "$0" "/D=" $1
  ${IfNot} ${Errors}
    ${GetRoot} "$1" $2
    StrCpy $3 "$1" 2
    ${If} $3 != "\\"
      StrCpy $3 "$1" 1 2
      ${If} $3 != "\"
      ${AndIf} $3 != "/"
        StrCpy $2 ""
      ${EndIf}
    ${EndIf}
    ${If} $2 == ""
      MessageBox MB_OK|MB_ICONSTOP "The installation folder must be an absolute directory path." /SD IDOK
      SetErrorLevel 1
      Quit
    ${EndIf}
    ; GetFullPathName can return empty for a destination that does not exist.
    ; The /D= convention already requires a full path; preserve that path here.
    StrCpy $INSTDIR $1
    Call CheckDestination
  ${EndIf}
  ClearErrors
FunctionEnd

Function RollbackInstall
  ${If} $Transaction != 1
    Return
  ${EndIf}
  ; Idempotent: explicit errors and NSIS's internal extraction abort both call us.
  StrCpy $Transaction 0
  StrCpy $RollbackFailed 0
  ${If} $RegistryInstalled == 1
    System::Call 'advapi32::RegRenameKey(p $RegistryHandle, w "$RegistryName", w "$RegistryNew") i.r0'
    ${If} $0 != 0
      StrCpy $RollbackFailed 1
    ${Else}
      StrCpy $RegistryStaged 1
    ${EndIf}
  ${EndIf}
  ${If} $RegistryBackedUp == 1
    System::Call 'advapi32::RegRenameKey(p $RegistryHandle, w "$RegistryBackup", w "$RegistryName") i.r0'
    ${If} $0 != 0
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${EndIf}
  ${If} $RegistryStaged == 1
    ClearErrors
    DeleteRegKey HKCU "$RegistryParent\$RegistryNew"
    ${If} ${Errors}
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${EndIf}
  ${If} $RegistryHandle != 0
    System::Call 'advapi32::RegCloseKey(p $RegistryHandle)'
    StrCpy $RegistryHandle 0
  ${EndIf}
  ${If} $ShortcutInstalled == 1
    ClearErrors
    Delete "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
    ${If} ${Errors}
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${EndIf}
  ${If} $ShortcutBackedUp == 1
    ClearErrors
    Rename "$ShortcutBackup" "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
    ${If} ${Errors}
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${ElseIf} $ShortcutBackup != ""
    Delete "$ShortcutBackup"
  ${EndIf}
  ${If} $ShortcutNew != ""
    Delete "$ShortcutNew"
  ${EndIf}
  ${If} $DesktopShortcutInstalled == 1
    ClearErrors
    Delete "$DESKTOP\${SHORTCUT_NAME}.lnk"
    ${If} ${Errors}
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${EndIf}
  ${If} $DesktopShortcutBackedUp == 1
    ClearErrors
    Rename "$DesktopShortcutBackup" "$DESKTOP\${SHORTCUT_NAME}.lnk"
    ${If} ${Errors}
      StrCpy $RollbackFailed 1
    ${EndIf}
  ${ElseIf} $DesktopShortcutBackup != ""
    Delete "$DesktopShortcutBackup"
  ${EndIf}
  ${If} $DesktopShortcutNew != ""
    Delete "$DesktopShortcutNew"
  ${EndIf}
  !insertmacro RestoreFile "crystal\pol.exe" 64
  !insertmacro RestoreFile "Uninstall.exe" 32
  !insertmacro RestoreFile "THIRD_PARTY_LICENSES.txt" 16
  !insertmacro RestoreFile "THIRD_PARTY.md" 8
  !insertmacro RestoreFile "LICENSE" 4
  !insertmacro RestoreFile "README.md" 2
  !insertmacro RestoreFile "Crystal-Launcher.exe" 1
  ; Never delete backups that could not be restored, or unexpected child files.
  ${If} $StageDir != ""
    SetOutPath "$TEMP"
    !insertmacro DeletePackage "$StageDir\new"
    RMDir "$StageDir\previous\crystal"
    RMDir "$StageDir\previous"
    RMDir "$StageDir"
  ${EndIf}
  ${If} $CreatedHostDirectory == 1
    RMDir "$INSTDIR\crystal"
  ${EndIf}
  ${If} $CreatedDestination == 1
    RMDir "$INSTDIR"
  ${EndIf}
  ${If} $RollbackFailed == 1
    MessageBox MB_OK|MB_ICONSTOP "Setup failed and could not restore every item. Close programs using Crystal before retrying. Saved package files remain in $StageDir\previous; prior shortcuts are $ShortcutBackup and $DesktopShortcutBackup, and the prior uninstall entry is HKCU\$RegistryParent\$RegistryBackup." /SD IDOK
  ${Else}
    MessageBox MB_OK|MB_ICONSTOP "Crystal could not be installed. The previous installation was preserved. Close Crystal, check the destination's permissions and free space, then try again." /SD IDOK
  ${EndIf}
  SetErrorLevel 1
FunctionEnd

Function .onInstFailed
  Call RollbackInstall
  SetErrorLevel 1
FunctionEnd

; Classifies the entry $2 in $INSTDIR. $R0: 0 unrecognised, 1 Crystal package
; file or data, 2 stale leftover (old self-update, setup staging or temp file).
; $R1 holds its attributes. Reparse points are never recognised.
Function ClassifyEntry
  StrCpy $R0 0
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR\$2") i.R1'
  ${If} $R1 == -1
    Return
  ${EndIf}
  IntOp $R2 $R1 & 0x410
  ${If} $2 == "crystal"
  ${OrIf} $2 == "iced-launcher"
    ${If} $R2 == 16
      StrCpy $R0 1
    ${EndIf}
    Return
  ${EndIf}
  ${If} $2 == "Crystal-Launcher.exe"
  ${OrIf} $2 == "README.md"
  ${OrIf} $2 == "LICENSE"
  ${OrIf} $2 == "THIRD_PARTY.md"
  ${OrIf} $2 == "THIRD_PARTY_LICENSES.txt"
  ${OrIf} $2 == "Uninstall.exe"
    ${If} $R2 == 0
      StrCpy $R0 1
    ${EndIf}
    Return
  ${EndIf}
  StrCpy $R3 $2 18
  ${If} $R3 == ".crystal-previous-"
    ${If} $R2 == 16
      StrCpy $R0 2
    ${EndIf}
    Return
  ${EndIf}
  ; Setup staging folders and temp files: ns?????.tmp.
  StrLen $R3 $2
  StrCpy $R4 $2 2
  StrCpy $R5 $2 "" -4
  ${If} $R3 == 11
  ${AndIf} $R4 == "ns"
  ${AndIf} $R5 == ".tmp"
    IntOp $R3 $R1 & 0x400
    ${If} $R3 == 0
      StrCpy $R0 2
    ${EndIf}
    Return
  ${EndIf}
  StrCpy $R4 $2 23
  ${If} $R4 == "Crystal-Launcher.exe~RF"
  ${AndIf} $R5 == ".tmp"
  ${AndIf} $R2 == 0
    StrCpy $R0 2
  ${EndIf}
FunctionEnd

; Removes the real directory $R6 and its contents. RMDir /r enters junctions
; and deletes their targets' contents, so links are unlinked here, never entered.
Function RemoveTree
  Push $R5
  Push $R7
  Push $R8
  Push $R9
  ClearErrors
  FindFirst $R7 $R8 "$R6\*.*"
  ${IfNot} ${Errors}
    ${DoWhile} $R8 != ""
      ${If} $R8 != "."
      ${AndIf} $R8 != ".."
        System::Call 'kernel32::GetFileAttributesW(w "$R6\$R8") i.R9'
        IntOp $R5 $R9 & 0x410
        ${If} $R9 == -1
        ${ElseIf} $R5 == 16
          Push $R6
          StrCpy $R6 "$R6\$R8"
          Call RemoveTree
          Pop $R6
        ${ElseIf} $R5 == 1040
          RMDir "$R6\$R8"
        ${Else}
          Delete "$R6\$R8"
        ${EndIf}
      ${EndIf}
      ClearErrors
      FindNext $R7 $R8
      ${If} ${Errors}
        ${Break}
      ${EndIf}
    ${Loop}
    FindClose $R7
  ${EndIf}
  RMDir "$R6"
  Pop $R9
  Pop $R8
  Pop $R7
  Pop $R5
FunctionEnd

Function CheckDestination
  ; Reinstall or upgrade in place into a folder holding only Crystal's own
  ; package files, retained data and stale setup/update leftovers.
  ReadRegStr $0 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $0 != ""
    StrCmp $0 "$INSTDIR" allowed
  ${EndIf}
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR") i.r0 ?e'
  Pop $3
  ${If} $0 == -1
    StrCpy $0 $3
    ${If} $0 == 2
    ${OrIf} $0 == 3
      Goto allowed
    ${EndIf}
    Goto occupied
  ${EndIf}
  IntOp $0 $0 & 0x10
  StrCmp $0 0 occupied
  ClearErrors
  FindFirst $1 $2 "$INSTDIR\*.*"
  IfErrors occupied
  inspect:
    StrCmp $2 "" empty
    StrCmp $2 "." next
    StrCmp $2 ".." next
    Call ClassifyEntry
    StrCmp $R0 0 0 next
    FindClose $1
    Goto occupied
  next:
    ClearErrors
    FindNext $1 $2
    IfErrors empty
    Goto inspect
  empty:
    FindClose $1
    Goto allowed
  occupied:
    MessageBox MB_OK|MB_ICONSTOP "Choose an empty folder, a folder containing only retained Crystal data, or Crystal's registered installation folder." /SD IDOK
    SetErrorLevel 1
    Abort
  allowed:
    SetErrorLevel 0
    ClearErrors
FunctionEnd

Section "Crystal Launcher"
  ; Silent installs also pass the directory-page validation.
  Call CheckDestination
  StrCpy $Transaction 1
  StrCpy $BackupFiles 0
  StrCpy $InstalledFiles 0
  StrCpy $RegistryHandle 0
  StrCpy $RegistryStaged 0
  StrCpy $RegistryExisted 0
  StrCpy $RegistryBackedUp 0
  StrCpy $RegistryInstalled 0
  StrCpy $ShortcutBackedUp 0
  StrCpy $ShortcutInstalled 0
  StrCpy $DesktopShortcutBackedUp 0
  StrCpy $DesktopShortcutInstalled 0
  StrCpy $CreatedDestination 0
  StrCpy $CreatedHostDirectory 0
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR") i.r0'
  ${If} $0 == -1
    StrCpy $CreatedDestination 1
  ${EndIf}
  ClearErrors
  CreateDirectory "$INSTDIR"
  IfErrors install_failed
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR\crystal") i.r0 ?e'
  Pop $1
  ${If} $0 == -1
    ${If} $1 != 2
    ${AndIf} $1 != 3
      Goto install_failed
    ${EndIf}
    StrCpy $CreatedHostDirectory 1
    ClearErrors
    CreateDirectory "$INSTDIR\crystal"
    IfErrors install_failed
  ${Else}
    ; The installed host must stay inside the installation, never a linked folder.
    IntOp $0 $0 & 0x410
    ${If} $0 != 16
      Goto install_failed
    ${EndIf}
  ${EndIf}
  ; Stage on the destination volume so commit/rollback use renames, not writes.
  GetTempFileName $StageDir "$INSTDIR"
  IfErrors install_failed
  Delete "$StageDir"
  IfErrors install_failed
  CreateDirectory "$StageDir\new\crystal"
  IfErrors install_failed
  CreateDirectory "$StageDir\previous\crystal"
  IfErrors install_failed
  SetOutPath "$StageDir\new"
  IfErrors install_failed
  SetOverwrite on
  ClearErrors
  File "${PACKAGE_DIR}\Crystal-Launcher.exe"
  File "${PACKAGE_DIR}\README.md"
  File "${PACKAGE_DIR}\LICENSE"
  File "${PACKAGE_DIR}\THIRD_PARTY.md"
  File "${PACKAGE_DIR}\THIRD_PARTY_LICENSES.txt"
  IfErrors install_failed
  SetOutPath "$StageDir\new\crystal"
  IfErrors install_failed
  File "${PACKAGE_DIR}\crystal\pol.exe"
  IfErrors install_failed
  WriteUninstaller "$StageDir\new\Uninstall.exe"
  IfErrors install_failed

  ; Prepare the shortcut beside its final path, preserving any existing link.
  SetOutPath "$INSTDIR"
  CreateDirectory "$SMPROGRAMS"
  IfErrors install_failed
  GetTempFileName $ShortcutNew "$SMPROGRAMS"
  IfErrors install_failed
  CreateShortcut "$ShortcutNew" "$INSTDIR\Crystal-Launcher.exe" "" "$INSTDIR\Crystal-Launcher.exe" 0
  IfErrors install_failed
  GetTempFileName $ShortcutBackup "$SMPROGRAMS"
  IfErrors install_failed
  Delete "$ShortcutBackup"
  IfErrors install_failed

  CreateDirectory "$DESKTOP"
  IfErrors install_failed
  GetTempFileName $DesktopShortcutNew "$DESKTOP"
  IfErrors install_failed
  CreateShortcut "$DesktopShortcutNew" "$INSTDIR\Crystal-Launcher.exe" "" "$INSTDIR\Crystal-Launcher.exe" 0
  IfErrors install_failed
  GetTempFileName $DesktopShortcutBackup "$DESKTOP"
  IfErrors install_failed
  Delete "$DesktopShortcutBackup"
  IfErrors install_failed

  ; Preserve the complete prior uninstall key, including extra values/subkeys.
  ; Only a fully written replacement is published under the real key name.
  ${GetParent} "${UNINSTALL_KEY}" $RegistryParent
  ${GetFileName} "${UNINSTALL_KEY}" $RegistryName
  ${GetFileName} "$StageDir" $0
  System::Call 'kernel32::GetCurrentProcessId() i.r1'
  StrCpy $RegistryNew "$RegistryName.pending-$1-$0"
  StrCpy $RegistryBackup "$RegistryName.previous-$1-$0"
  System::Call 'advapi32::RegCreateKeyExW(p 0x80000001, w "$RegistryParent", i 0, p 0, i 0, i 0x2011f, p 0, *p .r0, p 0) i.r1'
  ${If} $1 != 0
    Goto install_failed
  ${EndIf}
  StrCpy $RegistryHandle $0
  ; RegCopyTree also copies the old key's security descriptor. The newly
  ; created staging key therefore needs WRITE_DAC/WRITE_OWNER as well as values.
  System::Call 'advapi32::RegCreateKeyExW(p $RegistryHandle, w "$RegistryNew", i 0, p 0, i 0, i 0xf013f, p 0, *p .r0, *i .r2) i.r1'
  ${If} $1 != 0
    Goto install_failed
  ${EndIf}
  ${If} $2 != 1
    System::Call 'advapi32::RegCloseKey(p r0)'
    Goto install_failed
  ${EndIf}
  StrCpy $RegistryStaged 1
  System::Call 'advapi32::RegOpenKeyExW(p $RegistryHandle, w "$RegistryName", i 0, i 0x20119, *p .r2) i.r1'
  ${If} $1 == 0
    StrCpy $RegistryExisted 1
    System::Call 'advapi32::RegCopyTreeW(p r2, p 0, p r0) i.r1'
    System::Call 'advapi32::RegCloseKey(p r2)'
  ${ElseIf} $1 == 2
    StrCpy $1 0
  ${EndIf}
  System::Call 'advapi32::RegCloseKey(p r0)'
  ${If} $1 != 0
    Goto install_failed
  ${EndIf}
  !insertmacro StageReg Str "DisplayName" "Crystal Launcher"
  !insertmacro StageReg Str "DisplayVersion" "${APP_VERSION}"
  !insertmacro StageReg Str "Publisher" "Crystal Launcher"
  !insertmacro StageReg Str "InstallLocation" "$INSTDIR"
  !insertmacro StageReg Str "DisplayIcon" "$INSTDIR\Crystal-Launcher.exe"
  !insertmacro StageReg Str "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  !insertmacro StageReg Str "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
  !insertmacro StageReg Str "URLInfoAbout" "https://github.com/porisius/FFXI-Crystal-Launcher"
  !insertmacro StageReg DWORD "NoModify" 1
  !insertmacro StageReg DWORD "NoRepair" 1

  !insertmacro BackupFile "Crystal-Launcher.exe" 1
  !insertmacro BackupFile "README.md" 2
  !insertmacro BackupFile "LICENSE" 4
  !insertmacro BackupFile "THIRD_PARTY.md" 8
  !insertmacro BackupFile "THIRD_PARTY_LICENSES.txt" 16
  !insertmacro BackupFile "Uninstall.exe" 32
  !insertmacro BackupFile "crystal\pol.exe" 64
  !insertmacro InstallFile "Crystal-Launcher.exe" 1
  !insertmacro InstallFile "README.md" 2
  !insertmacro InstallFile "LICENSE" 4
  !insertmacro InstallFile "THIRD_PARTY.md" 8
  !insertmacro InstallFile "THIRD_PARTY_LICENSES.txt" 16
  !insertmacro InstallFile "Uninstall.exe" 32
  !insertmacro InstallFile "crystal\pol.exe" 64
  System::Call 'kernel32::GetFileAttributesW(w "$SMPROGRAMS\${SHORTCUT_NAME}.lnk") i.r0 ?e'
  Pop $1
  ${If} $0 != -1
    IntOp $0 $0 & 0x410
    ${If} $0 != 0
      Goto install_failed
    ${EndIf}
    ClearErrors
    Rename "$SMPROGRAMS\${SHORTCUT_NAME}.lnk" "$ShortcutBackup"
    IfErrors install_failed
    StrCpy $ShortcutBackedUp 1
  ${Else}
    StrCpy $0 $1
    ${If} $0 != 2
    ${AndIf} $0 != 3
      Goto install_failed
    ${EndIf}
  ${EndIf}
  ClearErrors
  Rename "$ShortcutNew" "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
  IfErrors install_failed
  StrCpy $ShortcutInstalled 1
  System::Call 'kernel32::GetFileAttributesW(w "$DESKTOP\${SHORTCUT_NAME}.lnk") i.r0 ?e'
  Pop $1
  ${If} $0 != -1
    IntOp $0 $0 & 0x410
    ${If} $0 != 0
      Goto install_failed
    ${EndIf}
    ClearErrors
    Rename "$DESKTOP\${SHORTCUT_NAME}.lnk" "$DesktopShortcutBackup"
    IfErrors install_failed
    StrCpy $DesktopShortcutBackedUp 1
  ${Else}
    StrCpy $0 $1
    ${If} $0 != 2
    ${AndIf} $0 != 3
      Goto install_failed
    ${EndIf}
  ${EndIf}
  ClearErrors
  Rename "$DesktopShortcutNew" "$DESKTOP\${SHORTCUT_NAME}.lnk"
  IfErrors install_failed
  StrCpy $DesktopShortcutInstalled 1
  ${If} $RegistryExisted == 1
    System::Call 'advapi32::RegRenameKey(p $RegistryHandle, w "$RegistryName", w "$RegistryBackup") i.r0'
    ${If} $0 != 0
      Goto install_failed
    ${EndIf}
    StrCpy $RegistryBackedUp 1
  ${EndIf}
  System::Call 'advapi32::RegRenameKey(p $RegistryHandle, w "$RegistryNew", w "$RegistryName") i.r0'
  ${If} $0 != 0
    Goto install_failed
  ${EndIf}
  StrCpy $RegistryInstalled 1
  StrCpy $RegistryStaged 0
  StrCpy $Transaction 0
  System::Call 'advapi32::RegCloseKey(p $RegistryHandle)'
  StrCpy $RegistryHandle 0
  ; The complete package, shortcut and registration are now committed.
  ClearErrors
  ${If} $RegistryBackedUp == 1
    DeleteRegKey HKCU "$RegistryParent\$RegistryBackup"
  ${EndIf}
  Delete "$ShortcutBackup"
  Delete "$DesktopShortcutBackup"
  !insertmacro DeletePackage "$StageDir\previous"
  !insertmacro DeletePackage "$StageDir\new"
  RMDir "$StageDir"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONEXCLAMATION "Crystal was installed, but some setup backups could not be removed. Package backups are in $StageDir; prior shortcuts are $ShortcutBackup and $DesktopShortcutBackup and the prior uninstall entry is HKCU\$RegistryParent\$RegistryBackup." /SD IDOK
  ${EndIf}
  ; Best effort: clear stale leftovers of earlier updates and setups, never
  ; this setup's own staging folder. Linked leftovers are never classified stale.
  ${GetFileName} "$StageDir" $3
  ClearErrors
  FindFirst $1 $2 "$INSTDIR\*.*"
  ${IfNot} ${Errors}
    ${DoWhile} $2 != ""
      ${If} $2 != $3
        Call ClassifyEntry
        ${If} $R0 == 2
          IntOp $R1 $R1 & 0x10
          ${If} $R1 == 0
            Delete "$INSTDIR\$2"
          ${Else}
            StrCpy $R6 "$INSTDIR\$2"
            Call RemoveTree
          ${EndIf}
        ${EndIf}
      ${EndIf}
      ClearErrors
      FindNext $1 $2
      ${If} ${Errors}
        ${Break}
      ${EndIf}
    ${Loop}
    FindClose $1
  ${EndIf}
  ClearErrors
  ; Refresh only our shortcut items and their folders after publishing them.
  ; SHCNF_PATHW | SHCNF_FLUSHNOWAIT keeps an unresponsive shell from blocking setup.
  System::Call 'shell32::SHChangeNotify(i 0x2000, i 0x2005, w "$DESKTOP\${SHORTCUT_NAME}.lnk", p 0)'
  System::Call 'shell32::SHChangeNotify(i 0x2000, i 0x2005, w "$SMPROGRAMS\${SHORTCUT_NAME}.lnk", p 0)'
  System::Call 'shell32::SHChangeNotify(i 0x1000, i 0x2005, w "$DESKTOP", p 0)'
  System::Call 'shell32::SHChangeNotify(i 0x1000, i 0x2005, w "$SMPROGRAMS", p 0)'
  SetErrorLevel 0
  Goto install_done
  install_failed:
    Call RollbackInstall
    SetErrorLevel 1
    Abort
  install_done:
SectionEnd

Function un.onInit
  SetShellVarContext current
  SetRegView 64
  !insertmacro ClaimSetupMutex
FunctionEnd

Section "Uninstall"
  ; Delete only the files this installer owns. Keep settings, games and unrelated files.
  ReadRegStr $0 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $0 == ""
  ${OrIf} $0 != $INSTDIR
    MessageBox MB_OK|MB_ICONSTOP "Crystal's registered installation could not be verified. Open the uninstaller from its original installation folder." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR\crystal") i.r0 ?e'
  Pop $1
  ${If} $0 != -1
    IntOp $0 $0 & 0x410
    ${If} $0 != 16
      MessageBox MB_OK|MB_ICONSTOP "Crystal's game-host folder is not a regular installation directory. Restore the original folder before uninstalling." /SD IDOK
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${ElseIf} $1 != 2
  ${AndIf} $1 != 3
    MessageBox MB_OK|MB_ICONSTOP "Crystal's game-host folder could not be checked. Close programs using this installation and try again." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  ClearErrors
  Delete "$INSTDIR\crystal\pol.exe"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "Close games using this launcher installation, then try uninstalling again." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  RMDir "$INSTDIR\crystal"
  ClearErrors
  IfFileExists "$INSTDIR\Crystal-Launcher.exe" 0 launcher_removed
    Delete "$INSTDIR\Crystal-Launcher.exe"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "Close Crystal and finish any games using this launcher installation, then try uninstalling again." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  launcher_removed:
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY.md"
  Delete "$INSTDIR\THIRD_PARTY_LICENSES.txt"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "Some Crystal files could not be removed. Close programs using them, then run this uninstaller again." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  ReadRegStr $0 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $0 == $INSTDIR
    Delete "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
    Delete "$DESKTOP\${SHORTCUT_NAME}.lnk"
    ; Keep the registration and helper until shortcut removal succeeds, so a
    ; sharing violation remains retryable instead of losing install identity.
    IfErrors uninstall_registration_failed
    DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ${EndIf}
  IfErrors uninstall_registration_failed
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  System::Call 'shell32::SHChangeNotify(i 0x1000, i 0x2005, w "$DESKTOP", p 0)'
  System::Call 'shell32::SHChangeNotify(i 0x1000, i 0x2005, w "$SMPROGRAMS", p 0)'
  SetErrorLevel 0
  Goto uninstall_done
  uninstall_registration_failed:
    MessageBox MB_OK|MB_ICONSTOP "Crystal's shortcut or uninstall registration could not be removed. Check your Windows account's permissions, then retry." /SD IDOK
    SetErrorLevel 1
    Abort
  uninstall_done:
SectionEnd
