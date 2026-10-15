param([string]$cmd, [string]$arg = "", [int]$hold = 80)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class K {
  [StructLayout(LayoutKind.Sequential)] public struct KI { public ushort vk; public ushort scan; public uint flags; public uint time; public IntPtr extra; }
  [StructLayout(LayoutKind.Explicit, Size=40)] public struct IN { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KI ki; }
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, IN[] i, int size);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static void Key(ushort scan, bool up, bool ext) {
    var i = new IN[1]; i[0].type = 1; i[0].ki.scan = scan; i[0].ki.flags = 0x0008u | (up ? 0x0002u : 0u) | (ext ? 0x0001u : 0u);
    SendInput(1, i, Marshal.SizeOf(typeof(IN)));
  }
}
"@
$p = Get-Process eldenring -ErrorAction SilentlyContinue | Select-Object -First 1
function Focus {
  if (-not $p) { throw "game not running" }
  for ($i = 0; $i -lt 5; $i++) {
    [K]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [K]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
    [K]::ShowWindow($p.MainWindowHandle, 9) | Out-Null; [K]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    Start-Sleep -Milliseconds 300
    if ([K]::GetForegroundWindow() -eq $p.MainWindowHandle) { return }
  }
  throw "could not bring the game to the front"
}
$scans = @{ "esc"=1; "1"=2; "2"=3; "3"=4; "4"=5; "q"=0x10; "w"=0x11; "e"=0x12; "r"=0x13; "f"=0x21; "a"=0x1e; "s"=0x1f; "d"=0x20; "enter"=0x1c; "space"=0x39; "up"=0xc8; "down"=0xd0; "left"=0xcb; "right"=0xcd; "ctrl"=0x1d; "shift"=0x2a; "g"=0x22; "m"=0x32; "i"=0x17; "x"=0x2d; "c"=0x2e; "tab"=0x0f; "f2"=0x3c; "f3"=0x3d; "f4"=0x3e }
switch ($cmd) {
  "shot" {
    Focus
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $bmp.Save($arg, [System.Drawing.Imaging.ImageFormat]::Png)
    "saved $arg $($b.Width)x$($b.Height)"
  }
  "keys" {
    Focus
    foreach ($k in $arg.Split(",")) {
      if ($k -match "^wait(\d+)$") { Start-Sleep -Milliseconds ([int]$Matches[1]); continue }
      if ($k -eq "lclick") { [K]::mouse_event(2,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds $hold; [K]::mouse_event(4,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 150; continue }
      $parts = $k.Split("+")
      foreach ($q in $parts) { $s = $scans[$q]; [K]::Key([uint16]($s -band 0x7f), $false, ($s -ge 0x80)) }
      Start-Sleep -Milliseconds $hold
      [array]::Reverse($parts)
      foreach ($q in $parts) { $s = $scans[$q]; [K]::Key([uint16]($s -band 0x7f), $true, ($s -ge 0x80)) }
      Start-Sleep -Milliseconds 120
    }
    "sent $arg"
  }
  "alive" { if ($p) { "running pid $($p.Id) responding $($p.Responding)" } else { "not running" } }
}
