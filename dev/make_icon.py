# SPDX-License-Identifier: GPL-3.0-or-later
"""Draws the Open3D Launcher icon: assets/open3d/icon.ico (16-256 px), icon-256.png and icon-64.png (tray).

Run with the Python that ships with 3ds Max (it has PySide6):
    python -I dev\\make_icon.py
"""

import struct
import sys
from pathlib import Path

from PySide6.QtCore import QBuffer, QByteArray, QIODevice, QPointF, QRectF, Qt
from PySide6.QtGui import QColor, QGuiApplication, QImage, QPainter, QPainterPath, QPolygonF

ASSETS = Path(__file__).resolve().parent.parent / "assets" / "open3d"
SIZES = (16, 24, 32, 48, 64, 128, 256)

BACKGROUND = QColor("#24262b")
TOP = QColor("#ffb061")
LEFT = QColor("#ff8c1a")
RIGHT = QColor("#c9660a")


def draw(size):
    img = QImage(size, size, QImage.Format_ARGB32)
    img.fill(Qt.transparent)
    p = QPainter(img)
    p.setRenderHint(QPainter.Antialiasing)
    s = float(size)

    # Rounded square background.
    bg = QPainterPath()
    bg.addRoundedRect(QRectF(0, 0, s, s), s * 0.22, s * 0.22)
    p.fillPath(bg, BACKGROUND)

    # Isometric cube: centre c, half width w, edge height h.
    cx, cy = s * 0.5, s * 0.52
    w = s * 0.30
    h = s * 0.34
    top = QPointF(cx, cy - h)
    upper_left = QPointF(cx - w, cy - h / 2)
    upper_right = QPointF(cx + w, cy - h / 2)
    centre = QPointF(cx, cy)
    lower_left = QPointF(cx - w, cy + h / 2)
    lower_right = QPointF(cx + w, cy + h / 2)
    bottom = QPointF(cx, cy + h)

    p.setPen(Qt.NoPen)
    for colour, pts in (
        (TOP, (top, upper_right, centre, upper_left)),
        (LEFT, (upper_left, centre, bottom, lower_left)),
        (RIGHT, (centre, upper_right, lower_right, bottom)),
    ):
        p.setBrush(colour)
        p.drawPolygon(QPolygonF(list(pts)))
    p.end()
    return img


def png_bytes(img):
    data = QByteArray()
    buf = QBuffer(data)
    buf.open(QIODevice.WriteOnly)
    img.save(buf, "PNG")
    buf.close()
    return bytes(data)


def write_ico(path, images):
    """ICO with PNG-compressed entries (supported by Windows Vista and later)."""
    pngs = [png_bytes(img) for img in images]
    header = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    entries = b""
    for img, data in zip(images, pngs):
        side = img.width()
        dim = 0 if side >= 256 else side  # 0 means 256 in the ICO format
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    path.write_bytes(header + entries + b"".join(pngs))


def main():
    _app = QGuiApplication.instance() or QGuiApplication(sys.argv)
    ASSETS.mkdir(exist_ok=True)
    images = [draw(s) for s in SIZES]
    write_ico(ASSETS / "icon.ico", images)
    images[-1].save(str(ASSETS / "icon-256.png"), "PNG")
    images[SIZES.index(64)].save(str(ASSETS / "icon-64.png"), "PNG")
    print("wrote icon.ico, icon-256.png and icon-64.png in", ASSETS)


if __name__ == "__main__":
    main()
