"""截取「歌词自动对齐工具」窗口客户区，保存到指定文件。纯标准库 + ctypes GDI。"""
import ctypes
import struct
import sys
from ctypes import wintypes

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
user32.SetProcessDPIAware()

ENUM = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)


def find_window():
    found = []

    def cb(hwnd, _):
        if not user32.IsWindowVisible(hwnd):
            return True
        n = user32.GetWindowTextLengthW(hwnd)
        if n == 0:
            return True
        buf = ctypes.create_unicode_buffer(n + 1)
        user32.GetWindowTextW(hwnd, buf, n + 1)
        if "歌词自动对齐" in buf.value:
            found.append((hwnd, buf.value))
        return True

    user32.EnumWindows(ENUM(cb), 0)
    return found


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "shot.bmp"
    wins = find_window()
    if not wins:
        print("窗口未找到")
        return 1
    hwnd = wins[0][0]
    rect = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(rect))
    w, h = rect.right, rect.bottom
    if w <= 0 or h <= 0:
        print("客户区尺寸异常")
        return 1

    origin = wintypes.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(origin))

    hdc = user32.GetDC(hwnd)
    memdc = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, w, h)
    gdi32.SelectObject(memdc, bmp)
    gdi32.BitBlt(memdc, 0, 0, w, h, hdc, 0, 0, 0x00CC0020)

    # BITMAPINFOHEADER
    header = struct.pack(
        "<IiiHHIIiiII",
        40, w, -h, 1, 32, 0, 0, 0, 0, 0, 0,
    )
    buf = ctypes.create_string_buffer(w * h * 4)
    gdi32.GetDIBits(memdc, bmp, 0, h, buf, header, 0)

    with open(out, "wb") as f:
        f.write(b"BM")
        f.write(struct.pack("<IHHI", 14 + len(header) + len(buf), 0, 0, 14 + len(header)))
        f.write(header)
        f.write(buf.raw)

    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(memdc)
    user32.ReleaseDC(hwnd, hdc)
    print("已保存", out, w, h)
    return 0


if __name__ == "__main__":
    sys.exit(main())
