param([Parameter(Mandatory=$true)][string]$TitleMatch)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Focus {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
}
"@

$proc = Get-Process | Where-Object { $_.MainWindowTitle -like "*$TitleMatch*" } | Select-Object -First 1
if (-not $proc) { Write-Error "No window matching '$TitleMatch' found"; exit 1 }
$hwnd = $proc.MainWindowHandle
[Win32Focus]::ShowWindow($hwnd, 9) | Out-Null
[Win32Focus]::SetForegroundWindow($hwnd) | Out-Null
Write-Host "Focused: $($proc.MainWindowTitle) (PID $($proc.Id))"
