; Tauri v2 NSIS installer hooks (bundle.windows.nsis.installerHooks in
; tauri.conf.json). These macros are spliced directly into the generated
; installer.nsi, so plain NSIS built-ins (ExecWait, DetailPrint, $INSTDIR)
; are already available - no extra plugin needed.
;
; Real friction this fixes: after a plain install, Windows Firewall blocks
; incoming P2P connections until someone manually runs an elevated
; New-NetFirewallRule. The NSIS installer already elevates for install
; (and uninstall), so adding/removing the rule here means nobody has to.
;
; ${MAINBINARYNAME} is defined earlier in installer.nsi from the bundle's
; main binary name - not hardcoded here, so this keeps working if the
; product/binary name ever changes.

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Adding Windows Firewall rules for LocalSync..."
  ; ponytail: fire-and-forget, doesn't abort install if netsh fails (e.g.
  ; firewall service disabled) - a missing rule just means the original
  ; manual-workaround friction returns, not a broken install.
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="LocalSync" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes protocol=TCP'
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="LocalSync" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes protocol=UDP'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing Windows Firewall rules for LocalSync..."
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="LocalSync"'
!macroend
