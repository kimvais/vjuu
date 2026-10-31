# Manual test: opens vjuu while another app has focus, then types j, c, x, Ctrl+Enter, Ctrl+S.
# If the page had keyboard focus, the review comes back with a comment "x". It moves focus and sends real
# keystrokes (only while vjuu is in front), so don't type during the run.
# Usage: tests/keyboard_focus.ps1 -HomeHwnd (Get-Process explorer | Where-Object MainWindowHandle -ne 0)[0].MainWindowHandle
param([string]$Exe = (Join-Path $PSScriptRoot '..\target\release\vjuu.exe'), [string]$File = (Join-Path $PSScriptRoot '..\README.md'), [Parameter(Mandatory)][long]$HomeHwnd)
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Kb {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  public static void Force(IntPtr h) {
    uint pid; uint fg = GetWindowThreadProcessId(GetForegroundWindow(), out pid); uint me = GetCurrentThreadId();
    AttachThreadInput(me, fg, true); BringWindowToTop(h); SetForegroundWindow(h); AttachThreadInput(me, fg, false);
  }
}
'@
function Get-ForegroundName { $p = 0; [Kb]::GetWindowThreadProcessId([Kb]::GetForegroundWindow(), [ref]$p) | Out-Null; (Get-Process -Id $p).ProcessName }
$term = [Kb]::GetForegroundWindow()
$env:USERNAME = 'vjuu-focus-test'
& $Exe stop 2>$null
$out = Join-Path $env:TEMP 'vjuu-kb-out.txt'
foreach ($round in 'cold', 'resident') {
    if ($round -eq 'resident') { Start-Process $Exe -ArgumentList '--serve' -WindowStyle Hidden; Start-Sleep -Seconds 4 }
    [Kb]::Force([IntPtr]$HomeHwnd); Start-Sleep -Milliseconds 300
    $env:VJUU_COLD = if ($round -eq 'cold') { '1' } else { $null }
    $client = Start-Process $Exe -ArgumentList $File -PassThru -WindowStyle Hidden -RedirectStandardOutput $out
    Start-Sleep -Milliseconds 2000
    $fg = Get-ForegroundName
    if ($fg -ne 'vjuu') { "{0,-9} foreground is {1}; not typing" -f $round, $fg; Stop-Process -Id $client.Id -Force; continue }
    [System.Windows.Forms.SendKeys]::SendWait('j'); Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait('c'); Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait('x'); Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait('^{ENTER}'); Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait('^s')
    $exited = $client.WaitForExit(3000)
    if (-not $exited) { Stop-Process -Id $client.Id -Force }
    $result = (Get-Content $out -Raw -ErrorAction SilentlyContinue) -replace "`r?`n", ' | '
    "{0,-9} exited after keys: {1,-5} output: {2}" -f $round, $exited, $result
}
Remove-Item Env:VJUU_COLD -ErrorAction SilentlyContinue
& $Exe stop 2>$null
[Kb]::Force($term)
