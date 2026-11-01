Add-Type -AssemblyName System.Windows.Forms, System.Drawing

$proc = Get-Process modern-sshdeck -ErrorAction SilentlyContinue |
  Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1

if (-not $proc) { throw "modern-sshdeck window not found" }

# 用 Win32 API 获取窗口客户区矩形（不含标题栏阴影）
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Cap {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
}
"@

$h = $proc.MainWindowHandle
[Win32Cap]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
[Win32Cap]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 800

$r = New-Object Win32Cap+RECT
[Win32Cap]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left
$hh = $r.Bottom - $r.Top
Write-Output "window rect: ${w}x${hh} at ($($r.Left),$($r.Top))"

$bmp = New-Object System.Drawing.Bitmap($w, $hh)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$out = $args[0]
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved: $out"
