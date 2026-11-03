# Captures the notification area of the taskbar (and the overflow flyout, where Windows 10
# puts new icons) with PrintWindow(PW_RENDERFULLCONTENT), for the tray screenshots of M2-E1-T15
# (docs/07 section 7). CopyFromScreen does not work on an RDP session; PrintWindow asks the shell
# window itself to render. Optionally also captures the open popup menus (class #32768) of a
# process, for the tray menu.
#
#   powershell -NoProfile -File scripts/qa/capture-tray.ps1 -Out qa-out/tray
#   powershell -NoProfile -File scripts/qa/capture-tray.ps1 -Out qa-out/tray -MenuOfProcess <pid> -OpenMenu
#   powershell -NoProfile -File scripts/qa/capture-tray.ps1 -Out qa-out/tray-services -MenuOfProcess <pid> -OpenMenu -Submenu 3
#
# -OpenMenu posts a right click to the tray icon's own window (tray-icon's `tray_icon_app`
# class, callback message 6002) so the menu opens without moving the mouse. tray-icon 0.25 drops
# the click when Shell_NotifyIconGetRect fails, which happens on Windows 10 while the icon sits
# in the hidden overflow area; when no menu opens, the script shows the overflow flyout (BM_CLICK
# on the taskbar chevron) and posts the click again. -Submenu <n> then selects the item at
# position <n> of the root menu (separators count: 0 = context header, 3 = Gerenciar servicos,
# 4 = Sites, 5 = PHP CLI) and opens its submenu. The menus are closed (Esc) after the capture
# unless -KeepMenu. Test tool only: the app never does this.
#
# Writes <Out>-taskbar.png (the whole taskbar), <Out>-notify.png (TrayNotifyWnd),
# <Out>-overflow.png (NotifyIconOverflowWindow, when it renders) and <Out>-menu.png (every open
# popup level of the process, composed at their screen positions).
# Output: one JSON line per file {ok, png, width, height} or {ok:false, error}.
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$MenuOfProcess = 0,
    [switch]$OpenMenu,
    [int]$Submenu = -1,
    [switch]$KeepMenu
)
$ErrorActionPreference = 'Stop'

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
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
    [DllImport("user32.dll")] static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);

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

    // Every visible popup menu window of `pid` (the root menu and its open submenus).
    public static List<IntPtr> PopupMenusOf(uint pid) {
        var all = new List<IntPtr>();
        EnumWindows((h, l) => {
            if (!IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            if (p != pid) return true;
            var sb = new StringBuilder(64); GetClassName(h, sb, 64);
            if (sb.ToString() == "#32768") all.Add(h);
            return true;
        }, IntPtr.Zero);
        return all;
    }

    // Shows the overflow flyout (the chevron button of the notification area).
    public static bool ShowOverflow() {
        IntPtr notify = Notify(Taskbar());
        if (notify == IntPtr.Zero) return false;
        IntPtr chevron = FindWindowEx(notify, IntPtr.Zero, "Button", null);
        if (chevron == IntPtr.Zero) return false;
        return PostMessage(chevron, 0x00F5, IntPtr.Zero, IntPtr.Zero); // BM_CLICK
    }

    public static bool OverflowVisible() {
        IntPtr h = Overflow();
        return h != IntPtr.Zero && IsWindowVisible(h);
    }

    // Selects item `index` of the popup menu window `menu` (MN_SELECTITEM) and opens its
    // submenu (VK_RIGHT through the menu's modal loop).
    public static void OpenSubmenu(IntPtr menu, int index) {
        SendMessage(menu, 0x01E5, (IntPtr)index, IntPtr.Zero);
        PostMessage(menu, 0x0100, (IntPtr)0x27, IntPtr.Zero);
    }

    // One Esc per open level.
    public static void CloseMenus(uint pid) {
        foreach (var m in PopupMenusOf(pid)) PostMessage(m, 0x0100, (IntPtr)0x1B, IntPtr.Zero);
    }

    // PrintWindow of each window in `hs`, composed on one bitmap at their screen positions.
    public static string CaptureMany(List<IntPtr> hs, string png) {
        var wins = new List<IntPtr>();
        var rects = new List<RECT>();
        int left = int.MaxValue, top = int.MaxValue, right = int.MinValue, bottom = int.MinValue;
        foreach (var h in hs) {
            RECT r; if (!GetWindowRect(h, out r)) continue;
            if (r.Right <= r.Left || r.Bottom <= r.Top) continue;
            wins.Add(h); rects.Add(r);
            left = Math.Min(left, r.Left); top = Math.Min(top, r.Top);
            right = Math.Max(right, r.Right); bottom = Math.Max(bottom, r.Bottom);
        }
        if (wins.Count == 0) return null;
        using (var canvas = new Bitmap(right - left, bottom - top, PixelFormat.Format32bppArgb)) {
            using (var cg = Graphics.FromImage(canvas)) {
                cg.Clear(Color.Transparent);
                // EnumWindows lists top-most first: draw in reverse so submenus end up on top.
                for (int i = wins.Count - 1; i >= 0; i--) {
                    RECT r = rects[i];
                    using (var bmp = new Bitmap(r.Right - r.Left, r.Bottom - r.Top, PixelFormat.Format32bppArgb)) {
                        using (var g = Graphics.FromImage(bmp)) {
                            IntPtr hdc = g.GetHdc();
                            bool ok = PrintWindow(wins[i], hdc, 2);
                            g.ReleaseHdc(hdc);
                            if (!ok) return null;
                        }
                        cg.DrawImageUnscaled(bmp, r.Left - left, r.Top - top);
                    }
                }
            }
            canvas.Save(png, ImageFormat.Png);
            return canvas.Width + "," + canvas.Height;
        }
    }

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
    $procId = [uint32]$MenuOfProcess
    if ($OpenMenu) {
        if (-not [ForgeboxTrayCapture]::RightClick($procId)) {
            Write-Output (([ordered]@{ ok = $false; error = 'no tray icon window in that process' }) | ConvertTo-Json -Compress)
            exit 1
        }
        Start-Sleep -Milliseconds 800
        if ([ForgeboxTrayCapture]::PopupMenuOf($procId) -eq [IntPtr]::Zero) {
            # The icon is in the overflow area and tray-icon dropped the click: show the
            # flyout and click again.
            if (-not [ForgeboxTrayCapture]::OverflowVisible()) {
                [void][ForgeboxTrayCapture]::ShowOverflow()
                Start-Sleep -Milliseconds 700
            }
            [void][ForgeboxTrayCapture]::RightClick($procId)
            Start-Sleep -Milliseconds 800
        }
    }
    $menu = [ForgeboxTrayCapture]::PopupMenuOf($procId)
    if ($menu -eq [IntPtr]::Zero) {
        Write-Output (([ordered]@{ ok = $false; png = "$base-menu.png"; error = 'no open popup menu' }) | ConvertTo-Json -Compress)
    } else {
        if ($Submenu -ge 0) {
            [ForgeboxTrayCapture]::OpenSubmenu($menu, $Submenu)
            Start-Sleep -Milliseconds 600
        }
        $menus = [ForgeboxTrayCapture]::PopupMenusOf($procId)
        Report "$base-menu.png" ([ForgeboxTrayCapture]::CaptureMany($menus, "$base-menu.png"))
        if (-not $KeepMenu) { [ForgeboxTrayCapture]::CloseMenus($procId) }
    }
}
exit 0
