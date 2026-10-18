!macro NSIS_HOOK_PREINSTALL
  ; Stop the previous service before replacing its executable. Ordinary STOP
  ; deliberately leaves the TV alone, including during an updater install.
  ${If} ${FileExists} "$INSTDIR\Manage-Service.ps1"
    nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\Manage-Service.ps1" -Action Stop -InstallDirectory "$INSTDIR"'
    Pop $0
    ${If} $0 != 0
      Abort "LTAC power service could not be stopped."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\Manage-Service.ps1" -Action Install -InstallDirectory "$INSTDIR"'
  Pop $0
  ${If} $0 != 0
    Abort "LTAC power service installation failed. Run the installer again."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\Manage-Service.ps1" -Action Remove -InstallDirectory "$INSTDIR"'
  Pop $0
  ${If} $0 != 0
    Abort "LTAC power service could not be removed."
  ${EndIf}
!macroend
