# Real operating-system drag and drop onto the app window (Windows, for e2e).
#
# Modes:
#   files  — a small source window of this process starts a genuine OLE
#            DoDragDrop with a CF_HDROP file list (what Explorer provides),
#            the mouse is pressed on it, moved onto the app window at the
#            client point (physical pixels), held there (`HoldMs`, for the
#            highlight to be observed), and released: the app receives the
#            drop through its registered OLE drop target.
#   text   — the same with plain text (CF_UNICODETEXT) instead of files.
#   mouse  — no OLE source: a real press at (X, Y) in the app, moves to
#            (X2, Y2), release (an in-page drag such as moving selected text
#            inside the editor).
# The app window is made topmost for the duration so nothing else is under
# the pointer, and restored afterwards. Prints one JSON line with the result.
param(
  [Parameter(Mandatory = $true)][int]$AppPid,
  [Parameter(Mandatory = $true)][int]$X,
  [Parameter(Mandatory = $true)][int]$Y,
  [ValidateSet("files", "text", "mouse", "probe")][string]$Mode = "files",
  [string[]]$Paths = @(),
  [string]$Text = "",
  [int]$X2 = 0,
  [int]$Y2 = 0,
  [int]$HoldMs = 1500
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @"
using System;
using System.Diagnostics;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows.Forms;

public static class OsDrag {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
  [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] static extern void mouse_event(uint flags, int dx, int dy, uint data, UIntPtr extra);
  [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr hwnd, ref POINT p);
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
  [DllImport("user32.dll", SetLastError = true)] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr hwnd, int index);
  [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr hwnd);
  [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hwnd);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int cmd);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out int value, int size);
  [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(POINT p);
  [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr value);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L; public int T; public int R; public int B; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out RECT r);
  [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr hwnd);
  const uint LEFTDOWN = 0x2, LEFTUP = 0x4, MOVE = 0x1;
  static readonly IntPtr TOPMOST = new IntPtr(-1), NOTOPMOST = new IntPtr(-2);
  const uint NOMOVE = 0x2, NOSIZE = 0x1, SHOW = 0x40;

  static void Move(int x, int y) { SetCursorPos(x, y); mouse_event(MOVE, 0, 0, 0, UIntPtr.Zero); }

  static void Glide(POINT from, POINT to, int steps, int delay) {
    for (int i = 1; i <= steps; i++) {
      Move(from.X + (to.X - from.X) * i / steps, from.Y + (to.Y - from.Y) * i / steps);
      Thread.Sleep(delay);
    }
  }

  static void Hold(POINT at, int ms) {
    int until = Environment.TickCount + ms; int k = 0;
    while (Environment.TickCount < until) { Move(at.X + (k % 2), at.Y); k++; Thread.Sleep(40); }
    Move(at.X, at.Y);
  }

  public static string Run(int pid, int cx, int cy, string mode, string[] paths, string text, int cx2, int cy2, int holdMs) {
    try { SetProcessDpiAwarenessContext(new IntPtr(-4)); } catch { SetProcessDPIAware(); }
    IntPtr hwnd = Process.GetProcessById(pid).MainWindowHandle;
    if (hwnd == IntPtr.Zero) return "{\"ok\":false,\"error\":\"no main window\"}";
    POINT target = new POINT { X = cx, Y = cy };
    ClientToScreen(hwnd, ref target);
    POINT target2 = new POINT { X = cx2, Y = cy2 };
    ClientToScreen(hwnd, ref target2);
    // Raise the app until it is the window under the drop point; never drag
    // onto another application's window.
    bool raised = false; int raiseError = 0; IntPtr under = IntPtr.Zero; bool onApp = false;
    for (int attempt = 0; attempt < 15 && !onApp; attempt++) {
      if (IsIconic(hwnd)) ShowWindow(hwnd, 9);
      raised = SetWindowPos(hwnd, TOPMOST, 0, 0, 0, 0, NOMOVE | NOSIZE | SHOW);
      raiseError = Marshal.GetLastWin32Error();
      SetForegroundWindow(hwnd);
      BringWindowToTop(hwnd);
      Thread.Sleep(200);
      under = GetAncestor(WindowFromPoint(target), 2);
      onApp = under == hwnd;
    }
    POINT saved; GetCursorPos(out saved);
    if (!onApp && mode != "probe") {
      SetWindowPos(hwnd, NOTOPMOST, 0, 0, 0, 0, NOMOVE | NOSIZE | SHOW);
      RECT wr; GetWindowRect(hwnd, out wr); int cloaked = 0;
      try { DwmGetWindowAttribute(hwnd, 14, out cloaked, 4); } catch { }
      return "{\"ok\":false,\"error\":\"another window covers the drop point\",\"under\":" + under.ToInt64() + ",\"hwnd\":" + hwnd.ToInt64() + ",\"window\":[" + wr.L + "," + wr.T + "," + wr.R + "," + wr.B + "],\"target\":[" + target.X + "," + target.Y + "],\"visible\":" + (IsWindowVisible(hwnd) ? "true" : "false") + ",\"iconic\":" + (IsIconic(hwnd) ? "true" : "false") + ",\"cloaked\":" + cloaked + "}";
    }
    if (mode == "probe") {
      RECT w; GetWindowRect(hwnd, out w); RECT c; GetClientRect(hwnd, out c);
      POINT origin = new POINT { X = 0, Y = 0 }; ClientToScreen(hwnd, ref origin);
      string probe = "{\"ok\":true,\"mode\":\"probe\",\"raised\":" + (raised ? "true" : "false") + ",\"raiseError\":" + raiseError + ",\"exstyle\":" + GetWindowLong(hwnd, -20) + ",\"iconic\":" + (IsIconic(hwnd) ? "true" : "false") + ",\"hwnd\":" + hwnd.ToInt64() + ",\"under\":" + under.ToInt64() + ",\"window\":[" + w.L + "," + w.T + "," + w.R + "," + w.B + "],\"client\":[" + c.R + "," + c.B + "],\"origin\":[" + origin.X + "," + origin.Y + "],\"target\":[" + target.X + "," + target.Y + "],\"dpi\":" + GetDpiForWindow(hwnd) + "}";
      SetWindowPos(hwnd, NOTOPMOST, 0, 0, 0, 0, NOMOVE | NOSIZE | SHOW);
      return probe;
    }
    string result;
    try {
      if (mode == "mouse") {
        Move(target.X, target.Y); Thread.Sleep(150);
        mouse_event(LEFTDOWN, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(400);
        Glide(target, target2, 12, 30);
        Hold(target2, holdMs);
        mouse_event(LEFTUP, 0, 0, 0, UIntPtr.Zero);
        Thread.Sleep(300);
        result = "{\"ok\":true,\"mode\":\"mouse\",\"onApp\":" + (onApp ? "true" : "false") + "}";
      } else {
        // The drag source: a small window of this process, left of the target.
        Form source = new Form();
        source.FormBorderStyle = FormBorderStyle.None; source.TopMost = true; source.ShowInTaskbar = false;
        source.StartPosition = FormStartPosition.Manual; source.Size = new Size(90, 40);
        source.Location = new Point(Math.Max(0, target.X - 260), Math.Max(0, target.Y - 20));
        source.BackColor = Color.LightSteelBlue;
        source.Show(); Application.DoEvents(); Thread.Sleep(200);
        POINT start = new POINT { X = source.Left + 45, Y = source.Top + 20 };
        Move(start.X, start.Y); Thread.Sleep(150);
        mouse_event(LEFTDOWN, 0, 0, 0, UIntPtr.Zero); Application.DoEvents(); Thread.Sleep(150);
        DataObject data = new DataObject();
        if (mode == "files") {
          var list = new System.Collections.Specialized.StringCollection();
          foreach (string p in paths) list.Add(p);
          data.SetFileDropList(list);
        } else {
          data.SetText(text, TextDataFormat.UnicodeText);
        }
        Thread mover = new Thread(() => {
          Thread.Sleep(250);
          Glide(start, target, 16, 30);
          Hold(target, holdMs);
          mouse_event(LEFTUP, 0, 0, 0, UIntPtr.Zero);
        });
        mover.IsBackground = true;
        mover.Start();
        DragDropEffects effect = source.DoDragDrop(data, DragDropEffects.Copy | DragDropEffects.Move);
        mover.Join(holdMs + 5000);
        source.Close();
        result = "{\"ok\":true,\"mode\":\"" + mode + "\",\"effect\":\"" + effect + "\",\"onApp\":" + (onApp ? "true" : "false") + "}";
      }
    } finally {
      SetWindowPos(hwnd, NOTOPMOST, 0, 0, 0, 0, NOMOVE | NOSIZE | SHOW);
      SetCursorPos(saved.X, saved.Y);
    }
    return result;
  }
}
"@
[OsDrag]::Run($AppPid, $X, $Y, $Mode, $Paths, $Text, $X2, $Y2, $HoldMs)
