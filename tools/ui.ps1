<#
.SYNOPSIS
  実行中の Rerics ウィンドウを操作・キャプチャする開発用ヘルパ（Claude の自走検証用）。

.DESCRIPTION
  プロセスの MainWindowHandle で対象ウィンドウを特定し、最小化→復元で確実に前面化してから、
  任意のキー送出（SendKeys 構文）と PNG キャプチャを行う。

  - PrintWindow は winsafe の窓だと黒画像になるため、前面化＋画面領域 CopyFromScreen 方式を採用。
  - IME 変換は SendKeys では再現できない（生キー素通り）。日本語入力の検証は人手が必要。
  - debug-server で観測・撮影できないモーダル（registry 非登録の OS レベル窓）は、本ツールの
    -Foreground で「前面に出た別窓」を直接撮る。詳細は rerics-e2e-verify skill を参照。

.PARAMETER Keys
  前面化後に送るキー（SendKeys 構文）。別窓を開くトリガ等。

.PARAMETER PostKeys
  Keys 送出後にもう一段送るキー。開いた窓のフォーカス済みコントロールを動かす用途
  （例：一覧の選択を矢印で動かす）。

.PARAMETER Foreground
  MainWindowHandle ではなく GetForegroundWindow（＝直前に開いた別窓モーダル）を撮る。

.PARAMETER Close
  撮影後に Esc を送って前面窓を閉じ、メイン窓を最小化する（作業画面に残さない）。

.EXAMPLE
  pwsh -File tools/ui.ps1                         # 撮るだけ -> target/shot.png
  pwsh -File tools/ui.ps1 -Keys "{DOWN}{ENTER}"   # キー送出してから撮る
  pwsh -File tools/ui.ps1 -Keys "%{F4}" -NoShot   # Alt+F4 だけ送る（撮らない）
  pwsh -File tools/ui.ps1 -Keys "%xs" -PostKeys "{DOWN}" -Foreground -Close
                                                  # メニュー等で別窓を開き→前面窓を撮り→閉じる
#>
param(
    [string]$Process = "rerics",
    [string]$Keys = "",
    [string]$PostKeys = "",
    [string]$Out = "",
    [switch]$Foreground,
    [switch]$NoShot,
    [switch]$NoFront,
    [switch]$NoMinimize,
    [switch]$Close,
    [int]$DelayMs = 300
)

$ErrorActionPreference = "Stop"
if (-not $Out) { $Out = Join-Path $PSScriptRoot "..\target\shot.png" }

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class RericsUi {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
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

# 最小化→復元で確実に前面化（SetForegroundWindow は背景プロセスからは効かないため）。
# -NoFront 時は前面化しない（既に開いているモーダルのフォーカスを奪わないため）。
if (-not $NoFront) {
    [RericsUi]::ShowWindow($h, 6) | Out-Null   # SW_MINIMIZE
    Start-Sleep -Milliseconds 250
    [RericsUi]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
    Start-Sleep -Milliseconds $DelayMs
}

if ($Keys -or $PostKeys) {
    Add-Type -AssemblyName System.Windows.Forms
}
if ($Keys) {
    [System.Windows.Forms.SendKeys]::SendWait($Keys)
    Start-Sleep -Milliseconds $DelayMs
}
# 開いた窓のフォーカス済みコントロールへの追撃キー（一覧移動など）。
if ($PostKeys) {
    [System.Windows.Forms.SendKeys]::SendWait($PostKeys)
    Start-Sleep -Milliseconds $DelayMs
}

if (-not $NoShot) {
    # 前面窓（別窓モーダル）を撮るときは再前面化しない（メイン窓を出すとモーダルが隠れるため）。
    if (-not $Foreground -and -not $NoFront) {
        # キー送出でフォーカスが移ることがあるので、撮る直前に再度前面化。
        [RericsUi]::ShowWindow($h, 6) | Out-Null
        Start-Sleep -Milliseconds 150
        [RericsUi]::ShowWindow($h, 9) | Out-Null
        Start-Sleep -Milliseconds 250
    }

    $target = if ($Foreground) { [RericsUi]::GetForegroundWindow() } else { $h }
    $r = New-Object RericsUi+RECT
    [RericsUi]::GetWindowRect($target, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left
    $ht = $r.Bottom - $r.Top
    Add-Type -AssemblyName System.Drawing
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()

    # 後始末：-Close は前面窓を Esc で閉じる。いずれもメイン窓を最小化して作業画面に残さない。
    if ($Close) {
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.SendKeys]::SendWait("{ESC}")
        Start-Sleep -Milliseconds 200
    }
    if (-not $NoFront -and -not $NoMinimize) {
        [RericsUi]::ShowWindow($h, 6) | Out-Null   # SW_MINIMIZE
    }
    Write-Output "SHOT $Out ($($w)x$($ht))"
} else {
    Write-Output "KEYS sent: $Keys $PostKeys"
}
