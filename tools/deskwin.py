"""List the top-level windows that live on a given desktop.

    python tools/deskwin.py [--desktop NAME] [--proc SUBSTR] [--titled]

引数を省くと、今このスクリプトが乗っているデスクトップ（＝利用者が見ている方）を見る。
窓が本当に向こう側に居るのか、こちら側へ漏れていないのかを実測で確かめるためのもの。

窓はデスクトップごとに分かれているので、`EnumDesktopWindows` は指定した
デスクトップの窓しか返さない。`--desktop` を変えて2回叩けば、どちらの世界に
居るかが並べて分かる。
"""
import argparse
import ctypes
import ctypes.wintypes as w
import sys
import time

# 窓の題名には cp932 で表せない字が普通に混じる。既定の出力符号化のままだと落ちる。
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.stderr.reconfigure(encoding="utf-8", errors="replace")

u32 = ctypes.WinDLL("user32", use_last_error=True)
k32 = ctypes.WinDLL("kernel32", use_last_error=True)

DESKTOP_READOBJECTS = 0x0001
DESKTOP_ENUMERATE = 0x0040
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000

WNDENUMPROC = ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)

u32.OpenDesktopW.restype = w.HANDLE
u32.OpenDesktopW.argtypes = [w.LPCWSTR, w.DWORD, w.BOOL, w.DWORD]
u32.GetThreadDesktop.restype = w.HANDLE
u32.GetThreadDesktop.argtypes = [w.DWORD]
u32.EnumDesktopWindows.argtypes = [w.HANDLE, WNDENUMPROC, w.LPARAM]
u32.GetForegroundWindow.restype = w.HWND
u32.GetWindowTextLengthW.argtypes = [w.HWND]
u32.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, ctypes.c_int]
u32.IsWindowVisible.argtypes = [w.HWND]
u32.GetWindowThreadProcessId.argtypes = [w.HWND, ctypes.POINTER(w.DWORD)]
k32.OpenProcess.restype = w.HANDLE
k32.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]


def process_name(pid):
    h = k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return "?"
    try:
        buf = ctypes.create_unicode_buffer(32768)
        size = w.DWORD(len(buf))
        if k32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size)):
            return buf.value.rsplit("\\", 1)[-1]
        return "?"
    finally:
        k32.CloseHandle(h)


def window_title(hwnd):
    n = u32.GetWindowTextLengthW(hwnd)
    if n <= 0:
        return ""
    buf = ctypes.create_unicode_buffer(n + 1)
    u32.GetWindowTextW(hwnd, buf, n + 1)
    return buf.value


def foreground():
    hwnd = u32.GetForegroundWindow()
    if not hwnd:
        return (0, 0, "", "(前面窓なし)")
    pid = w.DWORD()
    u32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return (hwnd, pid.value, process_name(pid.value), window_title(hwnd))


def count_windows(hdesk, proc_key):
    n = [0]

    @WNDENUMPROC
    def tally(hwnd, _lparam):
        pid = w.DWORD()
        u32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if proc_key in process_name(pid.value).lower():
            n[0] += 1
        return True

    u32.EnumDesktopWindows(hdesk, tally, 0)
    return n[0]


def watch(seconds, proc_key, interval=0.2):
    """利用者が見ているデスクトップを見張り、変わった時だけ書き出す。

    何も起きなければ最初の1行しか出ない。それが「画面を触っていない」証拠になる。
    """
    hdesk = u32.GetThreadDesktop(k32.GetCurrentThreadId())
    end = time.monotonic() + seconds
    last = None
    changes = 0
    while time.monotonic() < end:
        cur = (foreground(), count_windows(hdesk, proc_key))
        if cur != last:
            (hwnd, pid, name, title), n = cur
            if last is not None:
                changes += 1
            print("%s  fg=%-24s pid=%-6d %s窓=%d  %s"
                  % (time.strftime("%H:%M:%S"), name, pid, proc_key, n, title),
                  flush=True)
            last = cur
        time.sleep(interval)
    print("watch: %.0f 秒で変化 %d 回" % (seconds, changes), flush=True)
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--desktop", help="見るデスクトップ名（省略＝今このスレッドのもの）")
    ap.add_argument("--proc", help="実行ファイル名にこの文字列を含むものだけ出す")
    ap.add_argument("--titled", action="store_true", help="題名のある窓だけ出す")
    ap.add_argument("--watch", type=float, metavar="SEC",
                    help="この秒数だけ前面の窓と窓数を見張り、変わった時だけ出す")
    args = ap.parse_args()

    if args.watch:
        return watch(args.watch, (args.proc or "rerics").lower())

    if args.desktop:
        hdesk = u32.OpenDesktopW(args.desktop, 0, False,
                                 DESKTOP_READOBJECTS | DESKTOP_ENUMERATE)
        if not hdesk:
            sys.stderr.write("deskwin: %s を開けなかった err=%d\n"
                             % (args.desktop, ctypes.get_last_error()))
            return 3
        where = args.desktop
    else:
        hdesk = u32.GetThreadDesktop(k32.GetCurrentThreadId())
        where = "(このスレッドのデスクトップ)"

    rows = []

    @WNDENUMPROC
    def collect(hwnd, _lparam):
        pid = w.DWORD()
        u32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        rows.append((bool(u32.IsWindowVisible(hwnd)), pid.value,
                     process_name(pid.value), window_title(hwnd)))
        return True

    u32.EnumDesktopWindows(hdesk, collect, 0)

    if args.proc:
        key = args.proc.lower()
        rows = [r for r in rows if key in r[2].lower()]
    if args.titled:
        rows = [r for r in rows if r[3]]

    print("desktop: %s  windows: %d" % (where, len(rows)))
    for visible, pid, name, title in rows:
        print("  %s  pid=%-6d %-24s %s" % ("visible" if visible else "hidden ",
                                           pid, name, title))
    return 0


if __name__ == "__main__":
    sys.exit(main())
