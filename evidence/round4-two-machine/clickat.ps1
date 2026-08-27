param(
  [Parameter(Mandatory=$true)][string]$TitleMatch,
  [Parameter(Mandatory=$true)][int]$RelX,   # x within the captured window image (screenshot space)
  [Parameter(Mandatory=$true)][int]$RelY    # y within the captured window image (screenshot space)
)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Click {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, IntPtr dwExtraInfo);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@

$MOUSEEVENTF_LEFTDOWN = 0x0002
$MOUSEEVENTF_LEFTUP = 0x0004

$proc = Get-Process | Where-Object { $_.MainWindowTitle -like "*$TitleMatch*" } | Select-Object -First 1
if (-not $proc) { Write-Error "No window matching '$TitleMatch' found"; exit 1 }
$hwnd = $proc.MainWindowHandle
[Win32Click]::ShowWindow($hwnd, 9) | Out-Null
[Win32Click]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 400

$rect = New-Object Win32Click+RECT
[Win32Click]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
$screenX = $rect.Left + $RelX
$screenY = $rect.Top + $RelY
Write-Host "Window at ($($rect.Left),$($rect.Top)) - clicking screen ($screenX,$screenY)"

[Win32Click]::SetCursorPos($screenX, $screenY) | Out-Null
Start-Sleep -Milliseconds 150
[Win32Click]::mouse_event($MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [IntPtr]::Zero)
Start-Sleep -Milliseconds 80
[Win32Click]::mouse_event($MOUSEEVENTF_LEFTUP, 0, 0, 0, [IntPtr]::Zero)
Write-Host "Clicked."
