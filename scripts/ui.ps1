param([string]$Action = "capture", [string]$Out = "shot.png", [string]$X = "", [string]$Y = "", [string]$Text = "")

Add-Type -AssemblyName System.Windows.Forms, System.Drawing

$src = @'
using System;
using System.Runtime.InteropServices;
public class W32 {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)]
    public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int c);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, int e);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref POINT p);
}
'@
Add-Type -TypeDefinition $src -ErrorAction SilentlyContinue

$proc = Get-Process modern-sshdeck -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $proc) { Write-Output "NO_WINDOW"; exit 1 }
$h = $proc.MainWindowHandle
[W32]::ShowWindow($h, 9) | Out-Null
[W32]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 600

$r = New-Object W32+RECT
[W32]::GetWindowRect($h, [ref]$r) | Out-Null

if ($Action -eq "capture") {
    $w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
    $bmp = New-Object System.Drawing.Bitmap($w, $hh)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Output "captured:$Out"
    exit 0
}

if ($Action -eq "click" -or $Action -eq "clicktype") {
    $cx = [int]$X; $cy = [int]$Y
    $p = New-Object W32+POINT
    $p.X = $cx; $p.Y = $cy
    [W32]::ClientToScreen($h, [ref]$p) | Out-Null
    [W32]::SetCursorPos($p.X, $p.Y) | Out-Null
    Start-Sleep -Milliseconds 150
    [W32]::mouse_event(0x0002, 0, 0, 0, 0)
    [W32]::mouse_event(0x0004, 0, 0, 0, 0)
    Start-Sleep -Milliseconds 300

    if ($Action -eq "clicktype" -and $Text -ne "") {
        if ($Text.StartsWith("!")) {
            [System.Windows.Forms.SendKeys]::SendWait("^a")
            Start-Sleep -Milliseconds 120
            [System.Windows.Forms.SendKeys]::SendWait($Text.Substring(1))
        } else {
            [System.Windows.Forms.SendKeys]::SendWait($Text)
        }
        Start-Sleep -Milliseconds 200
    }
    Write-Output "clicked:$cx,$cy"
    exit 0
}

Write-Output "unknown:$Action"
