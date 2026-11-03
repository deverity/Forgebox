# Captures the notification area of the taskbar (and the overflow flyout, where Windows 10
# puts new icons) with PrintWindow(PW_RENDERFULLCONTENT), for the tray screenshots of M2-E1-T15
# (docs/07 section 7). CopyFromScreen does not work on an RDP session; PrintWindow asks the shell
# window itself to render. Optionally also captures an open popup menu (class #32768) of a
# process, for the tray menu.
#
#   powershell -NoProfile -File scripts/qa/capture-tray.ps1 -Out qa-out/tray
#   powershell -NoProfile -File scripts/qa/capture-tray.ps1 -Out qa-out/tray -MenuOfProcess <pid> -OpenMenu
#
# -OpenMenu posts a right click to the tray icon's own window (tray-icon's `tray_icon_app`
# class, callback message 6002) so the menu opens without moving the mouse; close it with Esc or
# by ending the process. Test tool only: the app never does this.
#
# Writes <Out>-taskbar.png (the whole taskbar), <Out>-notify.png (TrayNotifyWnd),
# <Out>-overflow.png (NotifyIconOverflowWindow, when it renders) and <Out>-menu.png.
# Output: one JSON line per file {ok, png, width, height} or {ok:false, error}.
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$MenuOfProcess = 0,
    [switch]$OpenMenu
)
$ErrorActionPreference = 'Stop'

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;

public static class ForgeboxTrayCapture {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int max);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);

    static IntPtr WindowOf(uint pid, string cls, bool visibleOnly) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            if (visibleOnly && !IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            if (p != pid) return true;
            var sb = new StringBuilder(64); GetClassName(h, sb, 64);
            if (sb.ToString() == cls) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // A right click on the icon, as the shell reports it to tray-icon's window.
    public static bool RightClick(uint pid) {
        IntPtr h = WindowOf(pid, "tray_icon_app", false);
        if (h == IntPtr.Zero) return false;
        PostMessage(h, 6002, IntPtr.Zero, (IntPtr)0x0204);
        return PostMessage(h, 6002, IntPtr.Zero, (IntPtr)0x0205);
    }

    public static IntPtr Taskbar() { return FindWindow("Shell_TrayWnd", null); }
    public static IntPtr Notify(IntPtr taskbar) { return FindWindowEx(taskbar, IntPtr.Zero, "TrayNotifyWnd", null); }
    public static IntPtr Overflow() { return FindWindow("NotifyIconOverflowWindow", null); }

    public static IntPtr PopupMenuOf(uint pid) { return WindowOf(pid, "#32768", true); }

    // Renders `h` with PrintWindow; crops to `clip` (screen coordinates) when it is inside.
    public static string Capture(IntPtr h, IntPtr clip, string png) {
        RECT r; if (!GetWindowRect(h, out r)) return null;
        int w = r.Right - r.Left, ht = r.Bottom - r.Top;
        if (w <= 0 || ht <= 0) return null;
        using (var bmp = new Bitmap(w, ht, PixelFormat.Format32bppArgb)) {
            using (var g = Graphics.FromImage(bmp)) {
                IntPtr hdc = g.GetHdc();
                bool ok = PrintWindow(h, hdc, 2);
                g.ReleaseHdc(hdc);
                if (!ok) return null;
            }
            Rectangle area = new Rectangle(0, 0, w, ht);
            if (clip != IntPtr.Zero) {
                RECT c; if (GetWindowRect(clip, out c)) {
                    area = Rectangle.Intersect(area, new Rectangle(c.Left - r.Left, c.Top - r.Top, c.Right - c.Left, c.Bottom - c.Top));
                }
            }
            using (var part = bmp.Clone(area, PixelFormat.Format32bppArgb)) {
                part.Save(png, ImageFormat.Png);
            }
            return area.Width + "," + area.Height;
        }
    }
}
'@

function Report([string]$Png, $Result) {
    if ($null -eq $Result) {
        Write-Output (([ordered]@{ ok = $false; png = $Png; error = 'not rendered' }) | ConvertTo-Json -Compress)
    } else {
        $wh = $Result.Split(',')
        Write-Output (([ordered]@{ ok = $true; png = $Png; width = [int]$wh[0]; height = [int]$wh[1] }) | ConvertTo-Json -Compress)
    }
}

$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$base = [System.IO.Path]::GetFullPath($Out)

$taskbar = [ForgeboxTrayCapture]::Taskbar()
if ($taskbar -eq [IntPtr]::Zero) {
    Write-Output (([ordered]@{ ok = $false; error = 'no Shell_TrayWnd' }) | ConvertTo-Json -Compress)
    exit 1
}
Report "$base-taskbar.png" ([ForgeboxTrayCapture]::Capture($taskbar, [IntPtr]::Zero, "$base-taskbar.png"))
$notify = [ForgeboxTrayCapture]::Notify($taskbar)
Report "$base-notify.png" ([ForgeboxTrayCapture]::Capture($taskbar, $notify, "$base-notify.png"))
$overflow = [ForgeboxTrayCapture]::Overflow()
if ($overflow -ne [IntPtr]::Zero) {
    Report "$base-overflow.png" ([ForgeboxTrayCapture]::Capture($overflow, [IntPtr]::Zero, "$base-overflow.png"))
}
if ($MenuOfProcess -ne 0) {
    if ($OpenMenu) {
        if (-not [ForgeboxTrayCapture]::RightClick([uint32]$MenuOfProcess)) {
            Write-Output (([ordered]@{ ok = $false; error = 'no tray icon window in that process' }) | ConvertTo-Json -Compress)
            exit 1
        }
        Start-Sleep -Milliseconds 800
    }
    $menu = [ForgeboxTrayCapture]::PopupMenuOf([uint32]$MenuOfProcess)
    if ($menu -eq [IntPtr]::Zero) {
        Write-Output (([ordered]@{ ok = $false; png = "$base-menu.png"; error = 'no open popup menu' }) | ConvertTo-Json -Compress)
    } else {
        Report "$base-menu.png" ([ForgeboxTrayCapture]::Capture($menu, [IntPtr]::Zero, "$base-menu.png"))
    }
}
exit 0
