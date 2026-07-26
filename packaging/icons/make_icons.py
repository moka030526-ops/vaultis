#!/usr/bin/env python3
"""Generate the vaultis desktop-shortcut icons: a little vault, drawn locked
(read-only) and unlocked (edit).

Outputs, next to this script:
    vaultis-locked.png    / .ico   -> read-only shortcut
    vaultis-unlocked.png  / .ico   -> edit (--write) shortcut

Pure Pillow, no other tools needed. Run:  python3 make_icons.py
Everything is drawn supersampled and downscaled, so the edges stay smooth.
"""

import io
import struct
from pathlib import Path
from PIL import Image, ImageDraw

SS = 1024                     # supersampled canvas; final PNG is SS//2
HERE = Path(__file__).resolve().parent

# Same vault for both icons (clearly one app); only the padlock state + accent
# colour change, so "locked vs unlocked" is the message.
BODY_DARK = (28, 41, 61)      # cabinet edge
BODY      = (61, 90, 128)     # cabinet face (steel blue)
DOOR      = (50, 74, 106)     # inset door
DOOR_EDGE = (35, 54, 80)
METAL     = (205, 215, 228)   # dial / bolts (light steel)
METAL_DK  = (138, 158, 186)
PLATE     = (243, 247, 252)   # halo behind the padlock emblem

LOCK_BLUE  = (38, 120, 184)   # closed padlock (read-only = secure)
LOCK_AMBER = (226, 146, 45)   # open padlock   (edit = caution)


def rrect(d, box, r, **kw):
    d.rounded_rectangle(box, radius=r, **kw)


def draw_vault(locked: bool) -> Image.Image:
    img = Image.new("RGBA", (SS, SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    s = SS / 1024.0  # scale helper so all numbers read as /1024

    def u(x):  # scale a 0..1024 design unit to the canvas
        return x * s

    # --- cabinet -----------------------------------------------------------
    rrect(d, [u(96), u(96), u(928), u(928)], u(120),
          fill=BODY, outline=BODY_DARK, width=int(u(14)))
    # inset door
    rrect(d, [u(168), u(168), u(856), u(856)], u(96),
          fill=DOOR, outline=DOOR_EDGE, width=int(u(10)))

    # corner bolts
    for bx, by in [(250, 250), (774, 250), (250, 774), (774, 774)]:
        rb = u(26)
        d.ellipse([u(bx) - rb, u(by) - rb, u(bx) + rb, u(by) + rb],
                  fill=METAL_DK, outline=BODY_DARK, width=int(u(4)))

    # --- combination wheel (centre) ---------------------------------------
    cx, cy = u(512), u(470)
    ring_r = u(176)
    d.ellipse([cx - ring_r, cy - ring_r, cx + ring_r, cy + ring_r],
              outline=METAL, width=int(u(34)))
    # spokes + knobs (the safe-dial handle)
    import math
    for k in range(6):
        a = math.radians(k * 60)
        ex, ey = cx + math.cos(a) * ring_r, cy + math.sin(a) * ring_r
        d.line([cx, cy, ex, ey], fill=METAL, width=int(u(26)))
        kr = u(30)
        d.ellipse([ex - kr, ey - kr, ex + kr, ey + kr], fill=METAL)
    hub = u(54)
    d.ellipse([cx - hub, cy - hub, cx + hub, cy + hub],
              fill=METAL_DK, outline=BODY_DARK, width=int(u(6)))

    # --- padlock emblem (bottom-right), locked or open --------------------
    paste_padlock(img, locked)
    return img


def paste_padlock(base: Image.Image, locked: bool):
    s = SS / 1024.0

    def u(x):
        return x * s

    # circular plate so the emblem pops off the dial
    d = ImageDraw.Draw(base)
    pcx, pcy, pr = u(720), u(720), u(212)
    d.ellipse([pcx - pr, pcy - pr, pcx + pr, pcy + pr],
              fill=PLATE, outline=(210, 220, 232), width=int(u(8)))

    color = LOCK_BLUE if locked else LOCK_AMBER

    # lock body
    bw, bh = u(196), u(150)
    bx0, by0 = pcx - bw / 2, pcy - bh / 2 + u(34)
    rrect(d, [bx0, by0, bx0 + bw, by0 + bh], u(34), fill=color)
    # keyhole
    kh = u(20)
    d.ellipse([pcx - kh, by0 + u(40) - kh, pcx + kh, by0 + u(40) + kh],
              fill=PLATE)
    d.polygon([(pcx - u(12), by0 + u(48)), (pcx + u(12), by0 + u(48)),
               (pcx + u(20), by0 + u(108)), (pcx - u(20), by0 + u(108))],
              fill=PLATE)

    # shackle: drawn on its own layer so the "open" one can be rotated
    th = int(u(34))
    sh = Image.new("RGBA", base.size, (0, 0, 0, 0))
    sd = ImageDraw.Draw(sh)
    # outer arc (top half) + two legs down to the body top
    ax0, ay0, ax1, ay1 = pcx - u(70), by0 - u(150), pcx + u(70), by0 - u(10)
    sd.arc([ax0, ay0, ax1, ay1], 180, 360, fill=color, width=th)
    leg_top = (ay0 + ay1) / 2
    sd.line([ax0 + th / 2, leg_top, ax0 + th / 2, by0 + u(6)], fill=color, width=th)
    sd.line([ax1 - th / 2, leg_top, ax1 - th / 2, by0 + u(6)], fill=color, width=th)
    if not locked:
        # swing the shackle open about the right leg's base
        pivot = (ax1 - th / 2, by0 + u(6))
        sh = sh.rotate(38, resample=Image.BICUBIC, center=pivot)
    base.alpha_composite(sh)


def dib_frame(img: Image.Image) -> bytes:
    """One icon frame in BMP/DIB form: BITMAPINFOHEADER + bottom-up BGRA + AND mask.

    This is the format Windows expects for icon sizes BELOW 256. Pillow's ICO writer
    emits PNG for *every* size unless told otherwise, and Explorer will not decode a
    PNG-compressed entry at 16/32/48 px — it falls back to a generic icon, which is
    exactly the size a shortcut is drawn at. See save_ico().
    """
    w, h = img.size
    px = img.convert("RGBA").load()

    # BITMAPINFOHEADER. Height is DOUBLED because the DIB notionally stacks the colour
    # (XOR) bitmap and the mask (AND) bitmap; biSizeImage=0 is allowed for BI_RGB.
    header = struct.pack(
        "<IiiHHIIiiII",
        40,        # biSize
        w,         # biWidth
        h * 2,     # biHeight  (XOR + AND)
        1,         # biPlanes
        32,        # biBitCount
        0,         # biCompression = BI_RGB
        0,         # biSizeImage
        0, 0,      # biXPelsPerMeter, biYPelsPerMeter
        0, 0,      # biClrUsed, biClrImportant
    )

    # XOR bitmap: BGRA, rows BOTTOM-UP. At 32bpp each row is already 4-byte aligned.
    xor = bytearray()
    for y in range(h - 1, -1, -1):
        for x in range(w):
            r, g, b, a = px[x, y]
            xor += bytes((b, g, r, a))

    # AND mask: 1bpp, bottom-up, each row padded to a 4-byte boundary. Left all-zero
    # ("opaque"): 32bpp icons carry real transparency in the alpha channel above, and
    # Windows uses that. The mask must still be PRESENT and correctly sized.
    row_bytes = ((w + 31) // 32) * 4
    and_mask = bytes(row_bytes * h)

    return header + bytes(xor) + and_mask


def save_ico(src: Image.Image, path: Path, sizes: list[int]) -> None:
    """Write a multi-size .ico by hand, so each frame is in the format Windows wants.

    256 px goes in PNG-compressed (the Vista+ convention, and what keeps the file
    small); everything smaller goes in as BMP/DIB. Written manually because Pillow's
    `sizes=` shortcut produces an all-PNG file, and its `bitmap_format="bmp"` produces
    an all-BMP one — neither is the mixed layout Windows actually expects.
    """
    frames = []
    for s in sorted(sizes, reverse=True):
        frame = src.resize((s, s), Image.LANCZOS)
        if s >= 256:
            buf = io.BytesIO()
            frame.save(buf, format="PNG")
            frames.append((s, buf.getvalue()))
        else:
            frames.append((s, dib_frame(frame)))

    # ICONDIR, then one 16-byte ICONDIRENTRY per frame, then the frame data.
    offset = 6 + 16 * len(frames)
    directory = b""
    for s, blob in frames:
        directory += struct.pack(
            "<BBBBHHII",
            s if s < 256 else 0,   # bWidth  (0 means 256)
            s if s < 256 else 0,   # bHeight (0 means 256)
            0,                     # bColorCount (0 = truecolour)
            0,                     # bReserved
            1,                     # wPlanes
            32,                    # wBitCount
            len(blob),             # dwBytesInRes
            offset,                # dwImageOffset
        )
        offset += len(blob)

    with open(path, "wb") as fh:
        fh.write(struct.pack("<HHH", 0, 1, len(frames)))  # reserved=0, type=1 (icon)
        fh.write(directory)
        for _, blob in frames:
            fh.write(blob)


def main():
    for locked, name in [(True, "vaultis-locked"), (False, "vaultis-unlocked")]:
        big = draw_vault(locked)
        png = big.resize((SS // 2, SS // 2), Image.LANCZOS)          # 512px
        png.save(HERE / f"{name}.png")
        ico_base = big.resize((256, 256), Image.LANCZOS)
        save_ico(ico_base, HERE / f"{name}.ico", [256, 128, 64, 48, 32, 16])
        print("wrote", name + ".png", "and", name + ".ico")


if __name__ == "__main__":
    main()
