#!/usr/bin/env bash
# ============================================================
#  Rerics ビルド用ラッパ (git-bash 側)
#  dev.bat を MSVC 環境込みで呼ぶ。cmd へのクォート崩れと
#  MSYS のパス変換を吸収する。
#  例: ./tools/dev.sh build / ./tools/dev.sh run / ./tools/dev.sh test
#      ./tools/dev.sh deploy （release ビルドしてデプロイ先へ差し替え起動）
# ============================================================
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # tools/
root="$(cd "$here/.." && pwd)"                          # リポジトリ root
cd "$root"

# deploy はビルド以外の処理（プロセス終了・コピー・起動）を含むので別スクリプトへ委譲。
if [[ "${1:-}" == "deploy" ]]; then
  shift
  exec "$here/deploy.sh" "$@"
fi

MSYS_NO_PATHCONV=1 exec cmd.exe /C ".\tools\dev.bat $*"
