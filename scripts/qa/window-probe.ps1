# QA probe of the real Forgebox.exe window (M2-E1-T7, ADR-0035). Dev script, not product code:
# the Win32 calls below live here, never in src-tauri.
#
#   -Action Launch       starts -Exe (with -ExeArgs) after the probe is compiled, then WaitVisible
#                        on it: the polling is already running when the process is created.
#   -Action WaitVisible  polls every ~1 ms until the "Forgebox" top-level window of -ProcessId
#                        is visible, then captures it at once with PrintWindow(PW_RENDERFULLCONTENT)
#                        (-Png) and reports pixel stats of that first visible frame (and of one
#                        100 ms later): content_ratio 0 means a blank frame, mode_luma tells which
#                        (0 black = unpainted transparent window, 255 white = unpainted WebView2,
#                        else an empty canvas).
#   -Action Motion       moves (-Mode drag) or resizes (-Mode resize) the window with SetWindowPos
#                        every -IntervalMs for -DurationMs, like the modal move/size loop does, and
#                        reports the latency of each call (the app's UI thread answers
#                        WM_WINDOWPOSCHANGING/CHANGED synchronously) and the DWM composition
#                        counters over the phase (DwmGetCompositionTimingInfo). -Mode idle measures
#                        the same counters without motion.
#   -Action Info         visibility, iconic/zoomed state, rect and whether it is the foreground window.
#   -Action Capture      PrintWindow of the window now (-Png) with the same pixel stats.
#
# Output: one JSON object on stdout. Run with `powershell -NoProfile -File scripts/qa/window-probe.ps1`.
param(
    [int]$ProcessId = 0,
    [ValidateSet('Launch', 'WaitVisible', 'Motion', 'Info', 'Capture')][string]$Action = 'Info',
    [string]$Exe = '',
    [string]$ExeArgs = '',
    [ValidateSet('drag', 'resize', 'idle')][string]$Mode = 'drag',
    [int]$DurationMs = 3000,
    [int]$IntervalMs = 16,
    [int]$TimeoutMs = 30000,
    [string]$Png = ''
)
$ErrorActionPreference = 'Stop'

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;

public static class ForgeboxWindowProbe {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential, Pack = 1)] public struct RATIO { public uint Num, Den; }
    [StructLayout(LayoutKind.Sequential, Pack = 1)]
    public struct DWM_TIMING_INFO {
        public uint cbSize; public RATIO rateRefresh; public ulong qpcRefreshPeriod; public RATIO rateCompose;
        public ulong qpcVBlank; public ulong cRefresh; public uint cDXRefresh; public ulong qpcCompose; public ulong cFrame;
        public uint cDXPresent; public ulong cRefreshFrame; public ulong cFrameSubmitted; public uint cDXPresentSubmitted;
        public ulong cFrameConfirmed; public uint cDXPresentConfirmed; public ulong cRefreshConfirmed; public uint cDXRefreshConfirmed;
        public ulong cFramesLate; public uint cFramesOutstanding; public ulong cFrameDisplayed; public ulong qpcFrameDisplayed;
        public ulong cRefreshFrameDisplayed; public ulong cFrameComplete; public ulong qpcFrameComplete; public ulong cFramePending;
        public ulong qpcFramePending; public ulong cFramesDisplayed; public ulong cFramesComplete; public ulong cFramesPending;
        public ulong cFramesAvailable; public ulong cFramesDropped; public ulong cFramesMissed; public ulong cRefreshNextDisplayed;
        public ulong cRefreshNextPresented; public ulong cRefreshesDisplayed; public ulong cRefreshesPresented; public ulong cRefreshStarted;
        public ulong cPixelsReceived; public ulong cPixelsDrawn; public ulong cBuffersEmpty;
    }
    public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("dwmapi.dll")] static extern int DwmGetCompositionTimingInfo(IntPtr hwnd, ref DWM_TIMING_INFO info);
    const uint SWP_NOZORDER = 0x0004, SWP_NOACTIVATE = 0x0010, SWP_NOSIZE = 0x0001, SWP_NOMOVE = 0x0002;
    const uint PW_RENDERFULLCONTENT = 2;

    public static IntPtr Find(uint pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p != pid) return true;
            var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
            if (sb.ToString() == "Forgebox") { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static string WaitVisible(uint pid, int timeoutMs, string png) {
        var sw = Stopwatch.StartNew();
        IntPtr h = IntPtr.Zero; long foundAt = -1; int polls = 0;
        while (sw.ElapsedMilliseconds < timeoutMs) {
            if (h == IntPtr.Zero) { h = Find(pid); if (h != IntPtr.Zero) foundAt = sw.ElapsedMilliseconds; }
            if (h != IntPtr.Zero && IsWindowVisible(h)) break;
            polls++;
            System.Threading.Thread.Sleep(1);
        }
        if (h == IntPtr.Zero || !IsWindowVisible(h)) return "{\"visible\":false,\"window_found\":" + (h != IntPtr.Zero ? "true" : "false") + "}";
        long visibleAt = sw.ElapsedMilliseconds;
        long visibleUnix = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        string stats = Capture(h, png);
        // A second look 100 ms later tells a one-frame lag of the compositor from a blank page.
        System.Threading.Thread.Sleep(100);
        string later = Capture(h, png.Length > 0 ? png.Replace(".png", "-100ms.png") : "");
        return "{\"visible\":true,\"window_found_ms\":" + foundAt + ",\"visible_ms\":" + visibleAt + ",\"visible_unix_ms\":" + visibleUnix + ",\"hidden_polls\":" + polls + "," + stats + ",\"after_100ms\":{" + later + "}}";
    }

    public static string Capture(IntPtr h, string png) {
        RECT r; GetWindowRect(h, out r);
        int w = Math.Max(1, r.Right - r.Left), ht = Math.Max(1, r.Bottom - r.Top);
        using (var bmp = new Bitmap(w, ht, PixelFormat.Format32bppArgb)) {
            using (var g = Graphics.FromImage(bmp)) {
                IntPtr hdc = g.GetHdc();
                bool ok = PrintWindow(h, hdc, PW_RENDERFULLCONTENT);
                g.ReleaseHdc(hdc);
                if (!ok) return "\"capture\":false";
            }
            // Stats of the client area (10 px inset: the invisible resize border of an
            // undecorated window is transparent, i.e. black, in a capture). mode_luma is the
            // background; content_ratio counts pixels far from it (text, controls): 0 means a
            // blank frame — black (unpainted transparent window), white (unpainted WebView2) or
            // an empty canvas (page painted before the shell).
            var data = bmp.LockBits(new Rectangle(0, 0, w, ht), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
            int stride = data.Stride; byte[] px = new byte[stride * ht];
            Marshal.Copy(data.Scan0, px, 0, px.Length); bmp.UnlockBits(data);
            int inset = 10; var hist = new long[256]; long n = 0; double sum = 0;
            var lumas = new List<byte>();
            for (int y = inset; y < ht - inset; y += 2) for (int x = inset; x < w - inset; x += 2) {
                int i = y * stride + x * 4;
                double lum = 0.2126 * px[i + 2] + 0.7152 * px[i + 1] + 0.0722 * px[i];
                byte l = (byte)Math.Min(255, Math.Max(0, Math.Round(lum)));
                hist[l]++; lumas.Add(l); sum += lum; n++;
            }
            int mode = 0; for (int k = 1; k < 256; k++) if (hist[k] > hist[mode]) mode = k;
            long content = 0, white = 0, dark = 0;
            foreach (byte l in lumas) { if (Math.Abs(l - mode) > 60) content++; if (l >= 254) white++; if (l < 10) dark++; }
            if (png.Length > 0) bmp.Save(png, ImageFormat.Png);
            n = Math.Max(1, n);
            return String.Format(System.Globalization.CultureInfo.InvariantCulture,
                "\"capture\":true,\"width\":{0},\"height\":{1},\"mean_luma\":{2:F1},\"mode_luma\":{3},\"content_ratio\":{4:F4},\"white_ratio\":{5:F4},\"dark_ratio\":{6:F4}",
                w, ht, sum / n, mode, (double)content / n, (double)white / n, (double)dark / n);
        }
    }

    static DWM_TIMING_INFO Timing() {
        var t = new DWM_TIMING_INFO(); t.cbSize = (uint)Marshal.SizeOf(typeof(DWM_TIMING_INFO));
        int hr = DwmGetCompositionTimingInfo(IntPtr.Zero, ref t);
        if (hr != 0) t.cbSize = 0;
        return t;
    }

    static string Pct(List<double> v) {
        if (v.Count == 0) return "null";
        v.Sort();
        Func<double, double> q = p => v[Math.Min(v.Count - 1, (int)Math.Floor(p * (v.Count - 1) + 0.5))];
        return String.Format(System.Globalization.CultureInfo.InvariantCulture,
            "{{\"n\":{0},\"p50\":{1:F2},\"p95\":{2:F2},\"p99\":{3:F2},\"max\":{4:F2}", v.Count, q(0.5), q(0.95), q(0.99), v[v.Count - 1]) + "}";
    }

    public static string Motion(uint pid, string mode, int durationMs, int intervalMs) {
        IntPtr h = Find(pid);
        if (h == IntPtr.Zero) return "{\"error\":\"window not found\"}";
        RECT r0; GetWindowRect(h, out r0);
        int w0 = r0.Right - r0.Left, h0 = r0.Bottom - r0.Top;
        var calls = new List<double>();
        var t0 = Timing();
        var sw = Stopwatch.StartNew();
        double freq = Stopwatch.Frequency;
        int i = 0;
        while (sw.ElapsedMilliseconds < durationMs) {
            double phase = (sw.ElapsedMilliseconds / 1000.0) * 2 * Math.PI * 0.5;
            long c0 = Stopwatch.GetTimestamp();
            if (mode == "drag") {
                int dx = (int)(120 * Math.Sin(phase)), dy = (int)(60 * Math.Cos(phase));
                SetWindowPos(h, IntPtr.Zero, r0.Left + dx, r0.Top + dy, 0, 0, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE);
            } else if (mode == "resize") {
                int dw = (int)(160 * (1 + Math.Sin(phase)) / 2), dh = (int)(100 * (1 + Math.Cos(phase)) / 2);
                SetWindowPos(h, IntPtr.Zero, 0, 0, w0 - 160 + dw, h0 - 100 + dh, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE);
            }
            if (mode != "idle") calls.Add((Stopwatch.GetTimestamp() - c0) * 1000.0 / freq);
            i++;
            long next = (long)i * intervalMs;
            while (sw.ElapsedMilliseconds < next) System.Threading.Thread.Sleep(1);
        }
        double elapsed = sw.Elapsed.TotalSeconds;
        var t1 = Timing();
        SetWindowPos(h, IntPtr.Zero, r0.Left, r0.Top, w0, h0, SWP_NOZORDER | SWP_NOACTIVATE);
        string dwm = (t0.cbSize == 0 || t1.cbSize == 0) ? "null" : String.Format(System.Globalization.CultureInfo.InvariantCulture,
            "{{\"refresh_hz\":{0:F1},\"refreshes\":{1},\"frames\":{2},\"frames_per_s\":{3:F1},\"frames_displayed\":{4},\"frames_dropped\":{5},\"frames_missed\":{6},\"frames_late\":{7}}}",
            t1.rateRefresh.Den == 0 ? 0 : (double)t1.rateRefresh.Num / t1.rateRefresh.Den,
            t1.cRefresh - t0.cRefresh, t1.cFrame - t0.cFrame, (t1.cFrame - t0.cFrame) / elapsed,
            t1.cFramesDisplayed - t0.cFramesDisplayed, t1.cFramesDropped - t0.cFramesDropped,
            t1.cFramesMissed - t0.cFramesMissed, t1.cFramesLate - t0.cFramesLate);
        return String.Format(System.Globalization.CultureInfo.InvariantCulture,
            "{{\"mode\":\"{0}\",\"seconds\":{1:F2},\"steps\":{2},\"setwindowpos_ms\":{3},\"dwm\":{4}}}", mode, elapsed, i, Pct(calls), dwm);
    }

    public static string Info(uint pid) {
        IntPtr h = Find(pid);
        if (h == IntPtr.Zero) return "{\"window_found\":false}";
        RECT r; GetWindowRect(h, out r);
        return String.Format("{{\"window_found\":true,\"visible\":{0},\"minimized\":{1},\"maximized\":{2},\"foreground\":{3},\"rect\":[{4},{5},{6},{7}]}}",
            IsWindowVisible(h) ? "true" : "false", IsIconic(h) ? "true" : "false", IsZoomed(h) ? "true" : "false",
            GetForegroundWindow() == h ? "true" : "false", r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top);
    }
}
'@

switch ($Action) {
    'Launch' {
        $started = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $p = if ($ExeArgs) { Start-Process -FilePath $Exe -ArgumentList $ExeArgs -PassThru } else { Start-Process -FilePath $Exe -PassThru }
        $result = [ForgeboxWindowProbe]::WaitVisible([uint32]$p.Id, $TimeoutMs, $Png)
        '{"pid":' + $p.Id + ',"launched_unix_ms":' + $started + ',' + $result.Substring(1)
    }
    'WaitVisible' { [ForgeboxWindowProbe]::WaitVisible([uint32]$ProcessId, $TimeoutMs, $Png) }
    'Motion' { [ForgeboxWindowProbe]::Motion([uint32]$ProcessId, $Mode, $DurationMs, $IntervalMs) }
    'Info' { [ForgeboxWindowProbe]::Info([uint32]$ProcessId) }
    'Capture' { '{' + [ForgeboxWindowProbe]::Capture([ForgeboxWindowProbe]::Find([uint32]$ProcessId), $Png) + '}' }
}
