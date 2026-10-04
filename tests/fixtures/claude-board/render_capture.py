"""Render native terminal captures without inventing or compositing host UI.

Run through `uv run --with pyte --with pillow python render_capture.py <capture-dir>`.
PNG output is a rendering of captured cells; the .cast and .ansi remain canonical.
"""
from collections import namedtuple
import json
from pathlib import Path
import re
import sys
import unicodedata

from PIL import Image, ImageDraw, ImageFont
import pyte
from wcwidth import wcswidth

Base = pyte.screens.Char
DimChar = namedtuple("DimChar", list(Base._fields) + ["dim"], defaults=list(Base(" ")) + [False])


class Screen(pyte.Screen):
    @property
    def display(self):
        return ["".join(self.buffer[y][x].data for x in range(self.columns)) for y in range(self.lines)]

    @property
    def default_char(self):
        return DimChar(**super().default_char._asdict(), dim=False)

    def reset(self):
        super().reset()
        self.cursor.attrs = self.default_char

    def draw(self, data):
        # pyte's character-at-a-time writer stops at a ZWJ and drops the rest
        # of that draw. Keep graphemes together so native Unicode rows retain
        # their real cell positions in review images and text exports.
        clusters = []
        for char in data:
            previous = clusters[-1] if clusters else ""
            regional = lambda value: len(value) == 1 and 0x1F1E6 <= ord(value) <= 0x1F1FF
            joins = unicodedata.combining(char) or char in "\u200d\ufe0e\ufe0f" or 0x1F3FB <= ord(char) <= 0x1F3FF
            if previous and (joins or previous.endswith("\u200d") or regional(previous) and regional(char)):
                clusters[-1] += char
            else:
                clusters.append(char)
        for cluster in clusters:
            width = wcswidth(cluster)
            if len(cluster) == 1 or width not in (1, 2):
                super().draw(cluster)
                continue
            super().draw("界" if width == 2 else "X")
            x = max(0, self.cursor.x - width)
            self.buffer[self.cursor.y][x] = self.cursor.attrs._replace(data=unicodedata.normalize("NFC", cluster))

    def select_graphic_rendition(self, *attrs):
        super().select_graphic_rendition(*attrs)
        dim = getattr(self.cursor.attrs, "dim", False)
        index = 0
        while index < len(attrs):
            code = attrs[index]
            if code in (38, 48) and index + 1 < len(attrs):
                index += 5 if attrs[index + 1] == 2 else 3
                continue
            if code in (0, 22):
                dim = False
            elif code == 2:
                dim = True
            index += 1
        if not attrs:
            dim = False
        self.cursor.attrs = DimChar(**{**self.cursor.attrs._asdict(), "dim": dim})


def render(path, settings):
    screen = Screen(settings["columns"], settings["rows"])
    # xterm modifyOtherKeys controls are not SGR; pyte otherwise mistakes >4m
    # for underline and produces a screenshot unlike the actual terminal.
    captured = re.sub(rb"\x1b\[[><][0-9;]*m", b"", path.read_bytes())
    pyte.ByteStream(screen).feed(captured)
    path.with_suffix(".txt").write_text("\n".join(screen.display) + "\n")
    light = settings.get("theme") == "light"
    background, foreground = ("#ffffff", "#222222") if light else ("#181818", "#d5d5d5")
    palette = {"black": "#181818", "red": "#e17c79", "green": "#a6c990", "brown": "#e2c88a",
               "blue": "#89a9f8", "magenta": "#c497db", "cyan": "#84c5ce", "white": "#d5d5d5"}

    def color(value, default):
        if value == "default":
            return default
        if len(value) == 6 and all(char in "0123456789abcdefABCDEF" for char in value):
            return "#" + value
        return palette.get(value, default)

    font_path = "/System/Library/Fonts/Menlo.ttc"
    if not Path(font_path).exists():
        font_path = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
    font = ImageFont.truetype(font_path, 18)
    bold = ImageFont.truetype(font_path, 18, index=1) if font_path.endswith(".ttc") else font
    symbol_path = Path("/System/Library/Fonts/Apple Symbols.ttf")
    symbols = ImageFont.truetype(str(symbol_path), 18) if symbol_path.exists() else font
    unicode_path = Path("/System/Library/Fonts/Supplemental/Arial Unicode.ttf")
    unicode_font = ImageFont.truetype(str(unicode_path), 18) if unicode_path.exists() else font
    cell_width, line_height, padding = font.getlength("M"), 25, 20
    image = Image.new("RGB", (round(cell_width * settings["columns"]) + 2 * padding,
                             line_height * settings["rows"] + 2 * padding), background)
    draw = ImageDraw.Draw(image)
    for y in range(settings["rows"]):
        for x in range(settings["columns"]):
            cell = screen.buffer[y][x]
            fg, bg = color(cell.fg, foreground), color(cell.bg, background)
            if cell.reverse:
                fg, bg = bg, fg
            if getattr(cell, "dim", False):
                fg = "#" + "".join(f"{round(int(fg[i:i+2], 16) * .65 + int(bg[i:i+2], 16) * .35):02x}" for i in (1, 3, 5))
            xx, yy = padding + x * cell_width, padding + y * line_height
            if bg != background:
                draw.rectangle((xx, yy, xx + cell_width + .5, yy + line_height), fill=bg)
            if cell.data.strip():
                if cell.data == "⏸":
                    for offset in (.24, .59):
                        draw.rectangle((xx + cell_width * offset, yy + 5, xx + cell_width * (offset + .17), yy + 17), fill=fg)
                else:
                    chosen = symbols if cell.data in "⎿⏵⏺" else unicode_font if any(0x2E80 <= ord(char) <= 0xFFEF for char in cell.data) else bold if cell.bold else font
                    draw.text((xx, yy), cell.data, font=chosen, fill=fg)
                if cell.underscore:
                    draw.line((xx, yy + line_height - 3, xx + cell_width, yy + line_height - 3), fill=fg)
    image.save(path.with_suffix(".png"))


if __name__ == "__main__":
    root = Path(sys.argv[1])
    settings = json.loads((root / "capture.json").read_text())
    for path in root.glob("*.ansi"):
        render(path, settings)
