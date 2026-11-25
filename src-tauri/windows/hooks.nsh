; Hooks for the NSIS installer (bundle > windows > nsis > installerHooks in tauri.conf.json).
;
; InstaADE was called Instabox until v0.1.6. The installer finds an earlier install by the
; product name (its uninstall key and folder), so installing InstaADE would leave Instabox
; installed next to it, with its own Start menu entry and start at login. In the release build
; only (identifier sh.instabox.bridge), this hook first uninstalls Instabox, silently, and moves
; its start at login over to InstaADE. Both apps keep their data in the identifier's folders,
; which the silent uninstall never deletes. Without an Instabox install it does nothing, so
; updates and reinstalls of InstaADE are not affected.

!define INSTABOX_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Instabox"
!define INSTABOX_RUNKEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define INSTABOX_APPROVEDKEY "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"

!macro NSIS_HOOK_PREINSTALL
  !if "${BUNDLEID}" == "sh.instabox.bridge"
    Push $0
    Push $1
    Push $2
    Push $3
    Push $4
    ; The Instabox install: its uninstall key, written by an installer of this app (its
    ; publisher and executable), and its uninstaller.
    ReadRegStr $0 HKCU "${INSTABOX_UNINSTKEY}" "MainBinaryName"
    ReadRegStr $3 HKCU "${INSTABOX_UNINSTKEY}" "Publisher"
    ReadRegStr $1 HKCU "${INSTABOX_UNINSTKEY}" "InstallLocation"
    ; The key holds the folder in quotes.
    StrCpy $2 $1 1
    ${If} $2 == '"'
      StrCpy $1 $1 -1 1
    ${EndIf}
    ${If} $0 == "${MAINBINARYNAME}.exe"
    ${AndIf} $3 == "${MANUFACTURER}"
    ${AndIf} $1 != ""
    ${AndIf} ${FileExists} "$1\uninstall.exe"
      ; $2: Instabox starts at login. Its Run entry is there, and Task Manager did not turn it
      ; off, which stores the time in the last 8 bytes of its StartupApproved entry.
      StrCpy $2 0
      ClearErrors
      ReadRegStr $3 HKCU "${INSTABOX_RUNKEY}" "Instabox"
      ${IfNot} ${Errors}
        StrCpy $2 1
        nsExec::ExecToStack '"$SYSDIR\reg.exe" query "HKCU\${INSTABOX_APPROVEDKEY}" /v Instabox'
        Pop $3
        Pop $4
        ${If} $3 == 0
          ${StrLoc} $3 $4 "0000000000000000$\r" ">"
          ${If} $3 == ""
            ${StrLoc} $3 $4 "0000000000000000$\n" ">"
          ${EndIf}
          ${If} $3 == ""
            StrCpy $2 0
          ${EndIf}
        ${EndIf}
      ${EndIf}
      ; Silent, so the uninstaller shows no confirm page and never deletes the app data (only
      ; that page's check box does). Without /UPDATE, so it removes its shortcuts, its
      ; uninstall key and its Run entry. It closes a running Instabox by its path first.
      ClearErrors
      ExecWait '"$1\uninstall.exe" /S _?=$1' $3
      ${IfNot} ${Errors}
      ${AndIf} $3 == 0
      ${AndIfNot} ${FileExists} "$1\${MAINBINARYNAME}.exe"
        ; Run in place (_?=), the uninstaller cannot delete itself or its folder.
        Delete "$1\uninstall.exe"
        RMDir "$1"
        ; What only Instabox used: its install folder record and its Task Manager entry.
        DeleteRegKey HKCU "${MANUKEY}\Instabox"
        DeleteRegValue HKCU "${INSTABOX_APPROVEDKEY}" "Instabox"
        ; InstaADE starts at login when Instabox did. The app finds its own Run entry, named
        ; after the product, and keeps it.
        ${If} $2 = 1
          ClearErrors
          ReadRegStr $3 HKCU "${INSTABOX_RUNKEY}" "${PRODUCTNAME}"
          ${If} ${Errors}
            WriteRegStr HKCU "${INSTABOX_RUNKEY}" "${PRODUCTNAME}" '"$INSTDIR\${MAINBINARYNAME}.exe" --at-login'
          ${EndIf}
        ${EndIf}
      ${EndIf}
      ; The install goes on in $INSTDIR, which the old folder may have been.
      SetOutPath $INSTDIR
    ${EndIf}
    Pop $4
    Pop $3
    Pop $2
    Pop $1
    Pop $0
  !endif
!macroend
