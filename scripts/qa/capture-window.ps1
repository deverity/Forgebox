# Captures the native frame (titlebar, material, borders) of a Forgebox window with
# PrintWindow(PW_RENDERFULLCONTENT) into a PNG (ADR-0036). CopyFromScreen does not work on an RDP
# session; PrintWindow asks the window itself to render, so it works there and when covered.
#
#   powershell -NoProfile -File scripts/qa/capture-window.ps1 -ProcessId <pid> -Png out.png
#   powershell -NoProfile -File scripts/qa/capture-window.ps1 -Title Forgebox -Png out.png
#
# Output: one JSON line {ok, png, width, height, mean_luma, content_ratio} or {ok:false, error}.
# Exit 0 = a non-black frame was written; 1 = no window / capture failed; 2 = the frame is black
# or blank (a locked session or a disconnected display makes DWM stop composing: unlock and retry).
param(
    [int]$ProcessId = 0,
    [string]$Title = 'Forgebox',
    [Parameter(Mandatory = $true)][string]$Png,
    [int]$TimeoutMs = 15000
)
$ErrorActionPreference = 'Stop'

function Fail([int]$Code, [string]$Message) {
    $json = [ordered]@{ ok = $false; error = $Message } | ConvertTo-Json -Compress
    Write-Output $json
    exit $Code
}

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;

public static class ForgeboxCapture {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT rect, int size);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);

    public static IntPtr Find(uint pid, string title) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            if (!IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            if (pid != 0 && p != pid) return true;
            var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
            if (sb.ToString() == title) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // Returns "w,h,mean_luma,content_ratio" or null when PrintWindow failed.
    // GetWindowRect includes the invisible resize borders of Win10 (about 8 px left/right/bottom,
    // painted black by PrintWindow); DWMWA_EXTENDED_FRAME_BOUNDS (9) is the visible frame, so the
    // bitmap is cropped to it.
    public static string Capture(IntPtr h, string png) {
        RECT r; GetWindowRect(h, out r);
        int fw = Math.Max(1, r.Right - r.Left), fh = Math.Max(1, r.Bottom - r.Top);
        int cx = 0, cy = 0, w = fw, ht = fh;
        RECT v;
        if (DwmGetWindowAttribute(h, 9, out v, Marshal.SizeOf(typeof(RECT))) == 0) {
            int x0 = Math.Max(0, v.Left - r.Left), y0 = Math.Max(0, v.Top - r.Top);
            int x1 = Math.Min(fw, v.Right - r.Left), y1 = Math.Min(fh, v.Bottom - r.Top);
            if (x1 - x0 > 0 && y1 - y0 > 0) { cx = x0; cy = y0; w = x1 - x0; ht = y1 - y0; }
        }
        using (var full = new Bitmap(fw, fh, PixelFormat.Format32bppArgb)) {
            using (var g = Graphics.FromImage(full)) {
                IntPtr hdc = g.GetHdc();
                bool ok = PrintWindow(h, hdc, 2);
                g.ReleaseHdc(hdc);
                if (!ok) return null;
            }
            using (var bmp = full.Clone(new Rectangle(cx, cy, w, ht), PixelFormat.Format32bppArgb)) {
                var data = bmp.LockBits(new Rectangle(0, 0, w, ht), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
                byte[] px = new byte[data.Stride * ht];
                Marshal.Copy(data.Scan0, px, 0, px.Length); bmp.UnlockBits(data);
                int inset = Math.Min(10, Math.Min(w, ht) / 4);
                double sum = 0; long n = 0, bright = 0;
                for (int y = inset; y < ht - inset; y += 2) for (int x = inset; x < w - inset; x += 2) {
                    int i = y * data.Stride + x * 4;
                    double lum = 0.2126 * px[i + 2] + 0.7152 * px[i + 1] + 0.0722 * px[i];
                    sum += lum; n++; if (lum >= 10) bright++;
                }
                n = Math.Max(1, n);
                bmp.Save(png, ImageFormat.Png);
                return String.Format(System.Globalization.CultureInfo.InvariantCulture,
                    "{0},{1},{2:F1},{3:F4}", w, ht, sum / n, (double)bright / n);
            }
        }
    }
}
'@

$dir = Split-Path -Parent ([System.IO.Path]::GetFullPath($Png))
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$full = [System.IO.Path]::GetFullPath($Png)

$deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
$hwnd = [IntPtr]::Zero
while ([DateTime]::UtcNow -lt $deadline) {
    $hwnd = [ForgeboxCapture]::Find([uint32]$ProcessId, $Title)
    if ($hwnd -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 100
}
if ($hwnd -eq [IntPtr]::Zero) { Fail 1 "no visible window titled '$Title' (pid $ProcessId) within $TimeoutMs ms" }

$stats = [ForgeboxCapture]::Capture($hwnd, $full)
if ($null -eq $stats) { Fail 1 'PrintWindow failed for the window' }
$p = $stats.Split(',')
$result = [ordered]@{
    ok = $true; png = $full; width = [int]$p[0]; height = [int]$p[1]
    mean_luma = [double]::Parse($p[2], [Globalization.CultureInfo]::InvariantCulture)
    content_ratio = [double]::Parse($p[3], [Globalization.CultureInfo]::InvariantCulture)
}
# Black: nothing but unpainted/transparent pixels (a locked or composition-less session).
if ($result.mean_luma -lt 2 -or $result.content_ratio -lt 0.01) {
    Remove-Item -LiteralPath $full -ErrorAction SilentlyContinue
    # LogonUI in THIS session only words the hint; PrintWindow itself works on a locked session.
    $mine = (Get-Process -Id $PID).SessionId
    $locked = @(Get-Process -Name LogonUI -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $mine }).Count -gt 0
    $hint = if ($locked) { 'this session is locked (LogonUI is running in it): unlock and run again' } else { 'the window painted nothing (still loading, or the display is off)' }
    Fail 2 "the captured frame is black (mean luma $($result.mean_luma)): $hint"
}
Write-Output ($result | ConvertTo-Json -Compress)
exit 0
