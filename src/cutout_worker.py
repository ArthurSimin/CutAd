"""Stream PNG frames through one locally loaded U²-Net session."""

import io
import os
import struct
import sys

from PIL import Image, ImageChops
from rembg import new_session, remove


def read_png(stream):
    signature = stream.read(8)
    if not signature:
        return None
    if signature != b"\x89PNG\r\n\x1a\n":
        raise ValueError("Invalid PNG frame in cutout input")

    encoded = io.BytesIO(signature)
    encoded.seek(0, io.SEEK_END)
    while True:
        header = stream.read(8)
        if len(header) != 8:
            raise EOFError("Incomplete PNG frame in cutout input")
        length, kind = struct.unpack(">I4s", header)
        payload = stream.read(length + 4)
        if len(payload) != length + 4:
            raise EOFError("Incomplete PNG chunk in cutout input")
        encoded.write(header)
        encoded.write(payload)
        if kind == b"IEND":
            encoded.seek(0)
            return encoded


def main():
    model = os.path.abspath(sys.argv[1])
    os.environ["U2NET_HOME"] = os.path.dirname(model)
    os.environ["REMBG_HOME"] = os.path.dirname(model)
    session = new_session("u2net_custom", model_path=model,
                          providers=["CPUExecutionProvider"])
    expected = int(sys.argv[2])
    subjects_only = len(sys.argv) > 3 and sys.argv[3] == "subjects"
    for index in range(expected):
        encoded = read_png(sys.stdin.buffer)
        if encoded is None:
            raise EOFError(f"Cutout read {index} frames, expected {expected}")
        with Image.open(encoded) as decoded:
            frame = decoded.convert("RGBA")
        mask = remove(frame.convert("RGB"), session=session, only_mask=True)
        mask = ImageChops.multiply(mask, frame.getchannel("A"))
        if subjects_only:
            width, height = mask.size
            bounds = mask.point(lambda pixel: 255 if pixel >= 160 else 0).getbbox()
            if bounds is None:
                print(f"{width} {height}", flush=True)
            else:
                left, top, right, bottom = bounds
                x = (left + right) / (2 * width)
                y = (top + bottom) / (2 * height)
                print(f"{width} {height} {x:.6f} {y:.6f}", flush=True)
        else:
            frame.putalpha(mask)
            frame.save(sys.stdout.buffer, format="PNG", compress_level=1)
            sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()
