param(
  [Parameter(Mandatory=$true)][string]$TitleMatch,
  [Parameter(Mandatory=$true)][string]$Text,
  [switch]$Raw
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Type {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
}
"@

$proc = Get-Process | Where-Object { $_.MainWindowTitle -like "*$TitleMatch*" } | Select-Object -First 1
if (-not $proc) { Write-Error "No window matching '$TitleMatch' found"; exit 1 }
$hwnd = $proc.MainWindowHandle
[Win32Type]::ShowWindow($hwnd, 9) | Out-Null
[Win32Type]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 400

if ($Raw) {
  $escaped = $Text
} else {
  # Escape SendKeys special characters for literal text
  $escaped = $Text -replace '([+^%~(){}])', '{$1}'
}
[System.Windows.Forms.SendKeys]::SendWait($escaped)
Write-Host "Typed into: $($proc.MainWindowTitle)"
