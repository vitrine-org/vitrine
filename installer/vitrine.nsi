; Vitrine 2 installer: per-user, no admin rights.
;   makensis /DVERSION=2.0.0 /DSOURCE_DIR=..\dist\vitrine /DOUTPUT_DIR=..\dist installer\vitrine.nsi
; SOURCE_DIR holds vitrine.exe, LICENSE and packs\*.vitrine.

Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !define VERSION "2.0.0"
!endif
!ifndef SOURCE_DIR
  !define SOURCE_DIR "..\dist\vitrine"
!endif
!ifndef OUTPUT_DIR
  !define OUTPUT_DIR "..\dist"
!endif

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Vitrine"

Name "Vitrine"
OutFile "${OUTPUT_DIR}\VitrineSetup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\Vitrine"
InstallDirRegKey HKCU "Software\Vitrine" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ManifestDPIAware true

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Vitrine"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}.0"
VIAddVersionKey "FileDescription" "Vitrine installer"
VIAddVersionKey "LegalCopyright" "MIT License"

!define MUI_ICON "..\assets\icon\vitrine.ico"
!define MUI_UNICON "..\assets\icon\vitrine.ico"
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\vitrine.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start Vitrine"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Italian"

!macro StopVitrine
  ; Ask a running Vitrine 2 to quit, then make sure no instance (v1 or v2) is left.
  IfFileExists "$INSTDIR\vitrine.exe" 0 +3
    nsExec::Exec '"$INSTDIR\vitrine.exe" --quit'
    Sleep 800
  nsExec::Exec 'taskkill /F /IM vitrine.exe'
!macroend

Section "Vitrine" SecCore
  SectionIn RO
  !insertmacro StopVitrine

  ; Upgrading from Vitrine 1 (.NET): its files are not used any more.
  ; User data (themes, config.json) in %APPDATA%\Vitrine is left untouched.
  IfFileExists "$INSTDIR\Vitrine.Engine.dll" 0 +3
    RMDir /r "$INSTDIR"
    CreateDirectory "$INSTDIR"

  SetOutPath "$INSTDIR"
  File "${SOURCE_DIR}\vitrine.exe"
  File "${SOURCE_DIR}\LICENSE"
  SetOutPath "$INSTDIR\packs"
  File /nonfatal "${SOURCE_DIR}\packs\*.vitrine"
  SetOutPath "$INSTDIR"

  WriteRegStr HKCU "Software\Vitrine" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Vitrine" "Version" "${VERSION}"

  ; Double-click a .vitrine package to install it.
  WriteRegStr HKCU "Software\Classes\.vitrine" "" "Vitrine.Package"
  WriteRegStr HKCU "Software\Classes\.vitrine" "Content Type" "application/x-vitrine"
  WriteRegStr HKCU "Software\Classes\Vitrine.Package" "" "Vitrine widget package"
  WriteRegStr HKCU "Software\Classes\Vitrine.Package\DefaultIcon" "" '"$INSTDIR\vitrine.exe",0'
  WriteRegStr HKCU "Software\Classes\Vitrine.Package\shell\open\command" "" '"$INSTDIR\vitrine.exe" "%1"'
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'

  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "Vitrine"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" '"$INSTDIR\vitrine.exe",0'
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Vitrine"
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://vitrine-widgets.github.io/"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0

  CreateShortCut "$SMPROGRAMS\Vitrine.lnk" "$INSTDIR\vitrine.exe"
SectionEnd

Section "Start with Windows" SecAutostart
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Vitrine" '"$INSTDIR\vitrine.exe"'
SectionEnd

LangString DESC_Core ${LANG_ENGLISH} "Vitrine and the Essentials widget pack."
LangString DESC_Core ${LANG_ITALIAN} "Vitrine e il pacchetto di widget Essentials."
LangString DESC_Autostart ${LANG_ENGLISH} "Start Vitrine when you sign in."
LangString DESC_Autostart ${LANG_ITALIAN} "Avvia Vitrine all'accesso."
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore} $(DESC_Core)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAutostart} $(DESC_Autostart)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

LangString ASK_DATA ${LANG_ENGLISH} "Also remove your widgets, layout and settings (Vitrine 2 data)?"
LangString ASK_DATA ${LANG_ITALIAN} "Rimuovere anche widget, disposizione e impostazioni (dati di Vitrine 2)?"

Section "Uninstall"
  !insertmacro StopVitrine
  Delete "$INSTDIR\vitrine.exe"
  Delete "$INSTDIR\LICENSE"
  RMDir /r "$INSTDIR\packs"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Vitrine.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Vitrine"
  DeleteRegKey HKCU "Software\Classes\Vitrine.Package"
  ReadRegStr $0 HKCU "Software\Classes\.vitrine" ""
  StrCmp $0 "Vitrine.Package" 0 +2
    DeleteRegKey HKCU "Software\Classes\.vitrine"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\Vitrine"

  ; Only Vitrine 2 data; Vitrine 1 files (config.json, themes\) are kept.
  MessageBox MB_YESNO|MB_ICONQUESTION $(ASK_DATA) /SD IDNO IDNO done
    RMDir /r "$APPDATA\Vitrine\packages"
    RMDir /r "$APPDATA\Vitrine\storage"
    RMDir /r "$APPDATA\Vitrine\logs"
    Delete "$APPDATA\Vitrine\layout.json"
    Delete "$APPDATA\Vitrine\settings.json"
    RMDir "$APPDATA\Vitrine"
  done:
SectionEnd
