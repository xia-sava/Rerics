#!/usr/bin/env bash
# ============================================================
#  Rerics ビルド用ラッパ (git-bash 側)
#  dev.bat を MSVC 環境込みで呼ぶ。cmd へのクォート崩れと
#  MSYS のパス変換を吸収する。
#  例: ./tools/dev.sh build / ./tools/dev.sh run / ./tools/dev.sh test
# ============================================================
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # tools/
root="$(cd "$here/.." && pwd)"                          # リポジトリ root
cd "$root"
MSYS_NO_PATHCONV=1 exec cmd.exe /C ".\tools\dev.bat $*"
