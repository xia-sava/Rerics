#!/usr/bin/env bash
# 画面に出ないデスクトップでビルド・テスト・アプリ起動を行なう。
#
#   ./tools/offscreen.sh dev <cargo の引数...>    MSVC 環境つき cargo（tools/dev.bat 経由）
#   ./tools/offscreen.sh run <program> [args...]  任意のプログラム
#
# 例:
#   ./tools/offscreen.sh dev test -p rerics --features debug-server --test debug_server
#   ./tools/offscreen.sh run target/debug/rerics.exe --debug-server=8731 --debug-visible &
#
# 窓の行き先は rundesk.py が作る別デスクトップ。止めるときは、このスクリプトが
# 起こした python を落とせば Job Object で子まで道連れになる。

set -u

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root" || exit 1

rundesk () {
  MSYS_NO_PATHCONV=1 python tools/rundesk.py "$@"
}

case "${1:-}" in
  dev)
    shift
    [ $# -ge 1 ] || { echo "usage: $0 dev <cargo args...>" >&2; exit 2; }
    rundesk cmd.exe /c ".\\tools\\dev.bat $*"
    ;;
  run)
    shift
    [ $# -ge 1 ] || { echo "usage: $0 run <program> [args...]" >&2; exit 2; }
    prog="$1"; shift
    # CreateProcess はコマンドラインを Windows の流儀で解くので、手元のファイルは絶対パスへ直す。
    if [ -e "$prog" ]; then
      prog="$(cd "$(dirname "$prog")" && pwd -W)/$(basename "$prog")"
    fi
    rundesk "$prog" "$@"
    ;;
  *)
    echo "usage: $0 {dev <cargo args...>|run <program> [args...]}" >&2
    exit 2
    ;;
esac
