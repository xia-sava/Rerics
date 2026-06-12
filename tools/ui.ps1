<#
.SYNOPSIS
  実行中の Rerics ウィンドウを操作・キャプチャする開発用ヘルパ（Claude の自走検証用）。

.DESCRIPTION
  プロセスの MainWindowHandle で対象ウィンドウを特定し、最小化→復元で確実に前面化してから、
  任意のキー送出（SendKeys 構文）と PNG キャプチャを行う。

  - PrintWindow は winsafe の窓だと黒画像になるため、前面化＋画面領域 CopyFromScreen 方式を採用。
  - IME 変換は SendKeys では再現できない（生キー素通り）。日本語入力の検証は人手が必要。

.EXAMPLE
  pwsh -File tools/ui.ps1                         # 撮るだけ -> target/shot.png
  pwsh -File tools/ui.ps1 -Keys "{DOWN}{ENTER}"   # キー送出してから撮る
  pwsh -File tools/ui.ps1 -Keys "%{F4}" -NoShot   # Alt+F4 だけ送る（撮らない）
#>
param(
    [string]$Process = "rerics",
    [string]$Keys = "",
    [string]$Out = "",
    [switch]$NoShot,
    [int]$DelayMs = 500
)

$ErrorActionPreference = "Stop"
if (-not $Out) { $Out = Join-Path $PSScriptRoot "..\target\shot.png" }

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class RericsUi {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[RericsUi]::SetProcessDPIAware() | Out-Null

$h = (Get-Process -Name $Process -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne 0 } |
        Select-Object -First 1).MainWindowHandle
if (-not $h) { Write-Error "プロセス '$Process' のウィンドウが見つかりません（起動してる？）"; exit 1 }

# 最小化→復元で確実に前面化（SetForegroundWindow は背景プロセスからは効かないため）
[RericsUi]::ShowWindow($h, 6) | Out-Null   # SW_MINIMIZE
Start-Sleep -Milliseconds 250
[RericsUi]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
Start-Sleep -Milliseconds $DelayMs

if ($Keys) {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.SendKeys]::SendWait($Keys)
    Start-Sleep -Milliseconds $DelayMs
}

if (-not $NoShot) {
    # キー送出でフォーカスが移ることがあるので、撮る直前に再度前面化
    [RericsUi]::ShowWindow($h, 6) | Out-Null
    Start-Sleep -Milliseconds 200
    [RericsUi]::ShowWindow($h, 9) | Out-Null
    Start-Sleep -Milliseconds 400

    $r = New-Object RericsUi+RECT
    [RericsUi]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left
    $ht = $r.Bottom - $r.Top
    Add-Type -AssemblyName System.Drawing
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Output "SHOT $Out ($($w)x$($ht))"
} else {
    Write-Output "KEYS sent: $Keys"
}
