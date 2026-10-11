; Hooks into Tauri's NSIS installer (tauri.conf.json, bundle.windows.nsis.installerHooks).

; An update starts while the app that launched it is still exiting: Tauri's
; updater opens the installer and only then exits the app, and with the
; model's threads (llama.cpp, OpenMP) the exit can take a few seconds. The
; installer's own check found it still running, failed to stop it and
; aborted ("could not close Nepheral", 0.5.2 -> 0.5.3). So an update first
; waits, up to 30 s, until the program file can be opened for writing:
; Windows refuses that while the program runs.
!macro NSIS_HOOK_PREINSTALL
  ${If} $UpdateMode = 1
  ${AndIf} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $R9 0
    nepheral_wait_exit:
      ClearErrors
      FileOpen $R8 "$INSTDIR\${MAINBINARYNAME}.exe" a
      ${IfNot} ${Errors}
        FileClose $R8
        Goto nepheral_exited
      ${EndIf}
      IntOp $R9 $R9 + 1
      IntCmp $R9 60 nepheral_exited
      Sleep 500
      Goto nepheral_wait_exit
    nepheral_exited:
  ${EndIf}
!macroend
