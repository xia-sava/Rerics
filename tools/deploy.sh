#!/usr/bin/env bash
# ============================================================
#  Rerics デプロイ (git-bash 側)
#  稼働中インスタンスを graceful 終了 → release ビルド →
#  デプロイ先へ rerics.exe をコピー → 起動。
#
#  デプロイ先はリポジトリに焼かない。以下の順で解決する:
#    1. 環境変数 RERICS_DEPLOY_DIR
#    2. gitignored な tools/../.claude/deploy.local.sh
#       （中で `export RERICS_DEPLOY_DIR="C:/app/Rerics"` のように設定）
#    3. どちらも無ければエラーで停止
#
#  例: ./tools/dev.sh deploy   （dev.sh から deploy サブコマンドで呼ぶ）
# ============================================================
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # tools/
root="$(cd "$here/.." && pwd)"                          # リポジトリ root
cd "$root"

# --- デプロイ先の解決 ---
if [[ -z "${RERICS_DEPLOY_DIR:-}" && -f .claude/deploy.local.sh ]]; then
  # shellcheck disable=SC1091
  source .claude/deploy.local.sh
fi
if [[ -z "${RERICS_DEPLOY_DIR:-}" ]]; then
  echo "[deploy] RERICS_DEPLOY_DIR が未設定。環境変数で渡すか、" 1>&2
  echo "         .claude/deploy.local.sh に次を書く:" 1>&2
  echo '           export RERICS_DEPLOY_DIR="C:/app/Rerics"' 1>&2
  exit 1
fi

# 末尾スラッシュを落とし、Windows パス（バックスラッシュ）へ正規化
deploy_dir="${RERICS_DEPLOY_DIR%[/\\]}"
deploy_win="${deploy_dir//\//\\}"
target_exe="${deploy_win}\\rerics.exe"

echo "[deploy] 対象: ${target_exe}"

# --- 1. デプロイ先で動いているインスタンスだけを graceful close ---
#     taskkill /IM はパスで絞れないので PowerShell で実行パス一致を特定する。
#     CloseMainWindow(=WM_CLOSE) で正常終了させ state.toml を保存させる。
#     一定時間残ったら Kill にフォールバック。dev/e2e の別インスタンスは path が
#     違うので巻き込まない。
powershell.exe -NoProfile -Command "
  \$target = '${target_exe}'
  \$procs = @(Get-Process rerics -ErrorAction SilentlyContinue | Where-Object { \$_.Path -ieq \$target })
  if (\$procs.Count -gt 0) {
    foreach (\$p in \$procs) { \$null = \$p.CloseMainWindow() }
    for (\$i = 0; \$i -lt 50; \$i++) {
      if (-not (\$procs | Where-Object { -not \$_.HasExited })) { break }
      Start-Sleep -Milliseconds 100
    }
    \$procs | Where-Object { -not \$_.HasExited } | ForEach-Object { \$_.Kill() }
    Write-Host '[deploy] 稼働インスタンスを終了'
  } else {
    Write-Host '[deploy] 稼働インスタンスなし'
  }
"

# --- 2. release ビルド（MSVC 環境は dev.sh 経由）---
echo "[deploy] release ビルド"
"${here}/dev.sh" build --release

# --- 3. exe をコピー（デプロイするのは exe のみ）---
src_exe="target/release/rerics.exe"
if [[ ! -f "${src_exe}" ]]; then
  echo "[deploy] ${src_exe} が見つからない（ビルド失敗？）" 1>&2
  exit 1
fi
mkdir -p "${deploy_dir}"
cp -f "${src_exe}" "${deploy_dir}/rerics.exe"
echo "[deploy] コピー完了: ${deploy_dir}/rerics.exe"

# --- 4. 起動（窓が開く）---
#     git bash から継いだ HOME・MSYS 系変数・PATH 追加分を落とし、ショートカット起動と
#     同じクリーンな環境で起動する。HOME が残ると Cygwin 系の子プロセス（cygterm/zsh 等）
#     がホームディレクトリを誤認する。
powershell.exe -NoProfile -Command "
  foreach (\$name in @('HOME','SHELL','TERM','HOSTNAME','MSYSTEM','MSYSTEM_PREFIX',
                       'MSYSTEM_CARCH','MSYSTEM_CHOST','MINGW_PREFIX','MINGW_CHOST',
                       'MINGW_PACKAGE_PREFIX','ORIGINAL_PATH','ORIGINAL_TEMP','ORIGINAL_TMP',
                       'EXEPATH','PLINK_PROTOCOL','SHLVL','PS1','OLDPWD')) {
    Remove-Item \"Env:\$name\" -ErrorAction SilentlyContinue
  }
  \$env:PATH = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
               [Environment]::GetEnvironmentVariable('Path', 'User')
  Start-Process -FilePath '${target_exe}'
"
echo "[deploy] 起動した"
