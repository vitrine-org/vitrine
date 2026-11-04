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
!define MUI_FINISHPAGE_RUN_TEXT "$(RUN_TEXT)"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
; The installer follows the Windows display language (English otherwise).
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Italian"
!insertmacro MUI_LANGUAGE "French"
!insertmacro MUI_LANGUAGE "German"
!insertmacro MUI_LANGUAGE "Spanish"
!insertmacro MUI_LANGUAGE "PortugueseBR"
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "Russian"
!insertmacro MUI_LANGUAGE "Japanese"
!insertmacro MUI_LANGUAGE "Korean"

LangString SEC_Autostart ${LANG_ENGLISH} "Start with Windows"
LangString SEC_Autostart ${LANG_ITALIAN} "Avvia con Windows"
LangString SEC_Autostart ${LANG_FRENCH} "Démarrer avec Windows"
LangString SEC_Autostart ${LANG_GERMAN} "Mit Windows starten"
LangString SEC_Autostart ${LANG_SPANISH} "Iniciar con Windows"
LangString SEC_Autostart ${LANG_PORTUGUESEBR} "Iniciar com o Windows"
LangString SEC_Autostart ${LANG_SIMPCHINESE} "随 Windows 启动"
LangString SEC_Autostart ${LANG_RUSSIAN} "Запускать вместе с Windows"
LangString SEC_Autostart ${LANG_JAPANESE} "Windows と同時に起動"
LangString SEC_Autostart ${LANG_KOREAN} "Windows 시작 시 실행"

LangString RUN_TEXT ${LANG_ENGLISH} "Start Vitrine"
LangString RUN_TEXT ${LANG_ITALIAN} "Avvia Vitrine"
LangString RUN_TEXT ${LANG_FRENCH} "Lancer Vitrine"
LangString RUN_TEXT ${LANG_GERMAN} "Vitrine starten"
LangString RUN_TEXT ${LANG_SPANISH} "Iniciar Vitrine"
LangString RUN_TEXT ${LANG_PORTUGUESEBR} "Iniciar o Vitrine"
LangString RUN_TEXT ${LANG_SIMPCHINESE} "启动 Vitrine"
LangString RUN_TEXT ${LANG_RUSSIAN} "Запустить Vitrine"
LangString RUN_TEXT ${LANG_JAPANESE} "Vitrine を起動"
LangString RUN_TEXT ${LANG_KOREAN} "Vitrine 시작"

LangString FILE_TYPE ${LANG_ENGLISH} "Vitrine widget package"
LangString FILE_TYPE ${LANG_ITALIAN} "Pacchetto di widget Vitrine"
LangString FILE_TYPE ${LANG_FRENCH} "Package de widgets Vitrine"
LangString FILE_TYPE ${LANG_GERMAN} "Vitrine-Widgetpaket"
LangString FILE_TYPE ${LANG_SPANISH} "Paquete de widgets de Vitrine"
LangString FILE_TYPE ${LANG_PORTUGUESEBR} "Pacote de widgets do Vitrine"
LangString FILE_TYPE ${LANG_SIMPCHINESE} "Vitrine 小组件包"
LangString FILE_TYPE ${LANG_RUSSIAN} "Пакет мини-приложений Vitrine"
LangString FILE_TYPE ${LANG_JAPANESE} "Vitrine ウィジェット パッケージ"
LangString FILE_TYPE ${LANG_KOREAN} "Vitrine 위젯 패키지"

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
  WriteRegStr HKCU "Software\Classes\Vitrine.Package" "" "$(FILE_TYPE)"
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
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://vitrine-org.github.io/"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0

  CreateShortCut "$SMPROGRAMS\Vitrine.lnk" "$INSTDIR\vitrine.exe"
SectionEnd

Section "$(SEC_Autostart)" SecAutostart
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Vitrine" '"$INSTDIR\vitrine.exe"'
SectionEnd

LangString DESC_Core ${LANG_ENGLISH} "Vitrine and the Essentials widget pack."
LangString DESC_Core ${LANG_ITALIAN} "Vitrine e il pacchetto di widget Essentials."
LangString DESC_Core ${LANG_FRENCH} "Vitrine et le package de widgets Essentials."
LangString DESC_Core ${LANG_GERMAN} "Vitrine und das Widgetpaket Essentials."
LangString DESC_Core ${LANG_SPANISH} "Vitrine y el paquete de widgets Essentials."
LangString DESC_Core ${LANG_PORTUGUESEBR} "O Vitrine e o pacote de widgets Essentials."
LangString DESC_Core ${LANG_SIMPCHINESE} "Vitrine 和 Essentials 小组件包。"
LangString DESC_Core ${LANG_RUSSIAN} "Vitrine и пакет мини-приложений Essentials."
LangString DESC_Core ${LANG_JAPANESE} "Vitrine と Essentials ウィジェット パッケージ。"
LangString DESC_Core ${LANG_KOREAN} "Vitrine 및 Essentials 위젯 패키지."
LangString DESC_Autostart ${LANG_ENGLISH} "Start Vitrine when you sign in."
LangString DESC_Autostart ${LANG_ITALIAN} "Avvia Vitrine all'accesso."
LangString DESC_Autostart ${LANG_FRENCH} "Démarrer Vitrine lorsque vous vous connectez."
LangString DESC_Autostart ${LANG_GERMAN} "Vitrine bei der Anmeldung starten."
LangString DESC_Autostart ${LANG_SPANISH} "Iniciar Vitrine al iniciar sesión."
LangString DESC_Autostart ${LANG_PORTUGUESEBR} "Iniciar o Vitrine quando você entrar."
LangString DESC_Autostart ${LANG_SIMPCHINESE} "登录时启动 Vitrine。"
LangString DESC_Autostart ${LANG_RUSSIAN} "Запускать Vitrine при входе в систему."
LangString DESC_Autostart ${LANG_JAPANESE} "サインイン時に Vitrine を起動します。"
LangString DESC_Autostart ${LANG_KOREAN} "로그인할 때 Vitrine을 시작합니다."
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore} $(DESC_Core)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAutostart} $(DESC_Autostart)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

LangString ASK_DATA ${LANG_ENGLISH} "Also remove your widgets, layout and settings (Vitrine 2 data)?"
LangString ASK_DATA ${LANG_ITALIAN} "Rimuovere anche widget, disposizione e impostazioni (dati di Vitrine 2)?"
LangString ASK_DATA ${LANG_FRENCH} "Supprimer également vos widgets, votre disposition et vos paramètres (données de Vitrine 2) ?"
LangString ASK_DATA ${LANG_GERMAN} "Auch Ihre Widgets, Ihr Layout und Ihre Einstellungen (Daten von Vitrine 2) entfernen?"
LangString ASK_DATA ${LANG_SPANISH} "¿Quitar también tus widgets, el diseño y la configuración (datos de Vitrine 2)?"
LangString ASK_DATA ${LANG_PORTUGUESEBR} "Remover também seus widgets, layout e configurações (dados do Vitrine 2)?"
LangString ASK_DATA ${LANG_SIMPCHINESE} "同时删除你的小组件、布局和设置（Vitrine 2 数据）吗？"
LangString ASK_DATA ${LANG_RUSSIAN} "Удалить также мини-приложения, макет и параметры (данные Vitrine 2)?"
LangString ASK_DATA ${LANG_JAPANESE} "ウィジェット、レイアウト、設定 (Vitrine 2 のデータ) も削除しますか?"
LangString ASK_DATA ${LANG_KOREAN} "위젯, 레이아웃 및 설정(Vitrine 2 데이터)도 제거하시겠습니까?"

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
