; Tauri v2 NSIS installer hooks (bundle.windows.nsis.installerHooks in
; tauri.conf.json). These macros are spliced directly into the generated
; installer.nsi, so plain NSIS built-ins (ExecWait, DetailPrint, $INSTDIR)
; are already available - no extra plugin needed. nsExec (used below) is
; also already available - it's one of NSIS's own bundled stock plugins
; (ships with every NSIS install, not a Tauri-specific or extra download).
;
; Real friction this fixes: after a plain install, Windows Firewall blocks
; incoming P2P connections until someone manually runs an elevated
; New-NetFirewallRule. The NSIS installer already elevates for install
; (and uninstall), so adding/removing the rule here means nobody has to.
;
; ${MAINBINARYNAME} is defined earlier in installer.nsi from the bundle's
; main binary name - not hardcoded here, so this keeps working if the
; product/binary name ever changes.
;
; Round 18: nsExec::ExecToLog, not plain ExecWait, for every netsh call
; below. Real click-through found this the hard way: ExecWait launches
; netsh.exe as a normal child process, which on Windows means a visible
; console window flashing on screen during install/uninstall - the same
; class of bug round 13 already fixed for the app's own Run-flow
; subprocesses (CREATE_NO_WINDOW), just hit here at install time instead.
; nsExec runs the child hidden and pipes its stdout/stderr into the
; installer's own detail log (so `DetailPrint`-style visibility into what
; ran is unchanged) instead of a flashing window. Its exit code lands on
; the stack either way - popped and discarded below, same as ExecWait's
; return value always was: fire-and-forget, doesn't abort install if
; netsh fails (e.g. firewall service disabled) - a missing rule just means
; the original manual-workaround friction returns, not a broken install.

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Adding Windows Firewall rules for LocalSync..."
  nsExec::ExecToLog '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="LocalSync" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes protocol=TCP'
  Pop $0
  nsExec::ExecToLog '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="LocalSync" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes protocol=UDP'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing Windows Firewall rules for LocalSync..."
  nsExec::ExecToLog '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="LocalSync"'
  Pop $0
!macroend
