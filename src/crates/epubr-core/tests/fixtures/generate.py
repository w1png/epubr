#!/usr/bin/env python3
"""Regenerate the synthetic, selectable-text PDF fixtures with Python 3."""

from pathlib import Path
import zlib


HERE = Path(__file__).resolve().parent


def literal(value):
    encoded = value.encode("cp1252")
    return b"(" + encoded.replace(b"\\", b"\\\\").replace(b"(", b"\\(").replace(b")", b"\\)") + b")"


def stream(contents, attributes=b""):
    return (
        b"<< /Length " + str(len(contents)).encode() + b" " + attributes + b" >>\n"
        b"stream\n" + contents + b"\nendstream"
    )


def text(x, y, value, size=12, bold=False):
    font = "F2" if bold else "F1"
    prefix = f"BT /{font} {size} Tf {x} {y} Td ".encode()
    return prefix + literal(value) + b" Tj ET\n"


def image():
    width, height = 96, 48
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            if (8 <= x < 32 or 64 <= x < 88) and 10 <= y < 38:
                color = (42, 96, 154)
            elif (32 <= x < 64 and 23 <= y <= 25) or (
                55 <= x < 64 and abs(y - 24) <= 64 - x
            ):
                color = (30, 30, 30)
            else:
                color = (244, 247, 251)
            pixels.extend(color)
    return stream(
        zlib.compress(pixels),
        b"/Type /XObject /Subtype /Image /Width 96 /Height 48 "
        b"/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
    )


class Pdf:
    def __init__(self):
        self.objects = []

    def add(self, contents=b""):
        self.objects.append(contents)
        return len(self.objects)

    def set(self, number, contents):
        self.objects[number - 1] = contents

    def write(self, filename, root, info):
        output = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
        offsets = [0]
        for number, contents in enumerate(self.objects, start=1):
            offsets.append(len(output))
            output.extend(f"{number} 0 obj\n".encode() + contents + b"\nendobj\n")
        xref = len(output)
        output.extend(f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode())
        for offset in offsets[1:]:
            output.extend(f"{offset:010d} 00000 n \n".encode())
        output.extend(
            f"trailer\n<< /Size {len(offsets)} /Root {root} 0 R /Info {info} 0 R >>"
            f"\nstartxref\n{xref}\n%%EOF\n".encode()
        )
        (HERE / filename).write_bytes(output)


def make_pdf(filename, title, pages, *, figure=False, chapter_outline=False):
    pdf = Pdf()
    root = pdf.add()
    tree = pdf.add()
    regular = pdf.add(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
    )
    bold = pdf.add(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
    )
    picture = pdf.add(image()) if figure else None
    resources = f"/Font << /F1 {regular} 0 R /F2 {bold} 0 R >>"
    if picture:
        resources += f" /XObject << /Diagram {picture} 0 R >>"

    children = []
    for contents in pages:
        content = pdf.add(stream(contents))
        children.append(pdf.add(
            f"<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 432 576] "
            f"/Resources << {resources} >> /Contents {content} 0 R >>".encode()
        ))
    kids = " ".join(f"{child} 0 R" for child in children)
    pdf.set(tree, f"<< /Type /Pages /Count {len(children)} /Kids [{kids}] >>".encode())

    outline_ref = ""
    if chapter_outline:
        outlines = pdf.add()
        first = pdf.add()
        subsection = pdf.add()
        second = pdf.add()
        pdf.set(outlines, f"<< /Type /Outlines /First {first} 0 R /Last {second} 0 R /Count 3 >>".encode())
        pdf.set(first, (
            f"<< /Title (1 First Chapter) /Parent {outlines} 0 R /Next {second} 0 R "
            f"/First {subsection} 0 R /Last {subsection} 0 R /Count 1 /Dest [{children[0]} 0 R /XYZ null 492 null] >>"
        ).encode())
        pdf.set(subsection, (
            f"<< /Title (1.1 A Subsection) /Parent {first} 0 R /Dest [{children[0]} 0 R /XYZ null 394 null] >>"
        ).encode())
        pdf.set(second, (
            f"<< /Title (2 Second Chapter) /Parent {outlines} 0 R /Prev {first} 0 R "
            f"/Dest [{children[1]} 0 R /Fit] >>"
        ).encode())
        outline_ref = f" /Outlines {outlines} 0 R"
    pdf.set(root, f"<< /Type /Catalog /Pages {tree} 0 R /Lang (en){outline_ref} >>".encode())
    info = pdf.add(b"<< /Title " + literal(title) + b" /Author (Fixture Author) >>")
    pdf.write(filename, root, info)


def main():
    first = (
        text(48, 520, "Technical Notes", 24, bold=True)
        + text(48, 478, "1 First Chapter", 19, bold=True)
        + text(48, 449, "First page opening.")
        + text(48, 429, "The café costs €5. The symbols × and · stay intact.")
        + text(48, 380, "1.1 A Subsection", 15, bold=True)
        + text(48, 355, "Read the engineer-")
        + text(48, 340, "ing notes on private-key encryption.")
        + b"q 192 0 0 96 48 213 cm /Diagram Do Q\n"
        + text(48, 197, "Figure 1. A test diagram.", 10)
    )
    second = (
        text(48, 520, "2 Second Chapter", 19, bold=True)
        + text(48, 491, "Second page closing.")
    )
    make_pdf("basic.pdf", "Technical Notes", [first, second], figure=True, chapter_outline=True)

    notes = (
        text(48, 520, "Notes in Context", 24, bold=True)
        + b"BT /F1 12 Tf 48 475 Td (The claim has a source) Tj "
        b"/F1 7 Tf 5 Ts (1) Tj 0 Ts /F1 12 Tf (.) Tj ET\n"
        + text(48, 447, "Unrelated body text.")
        + b"0.5 w 48 85 m 160 85 l S\n"
        + text(48, 68, "1 This source note belongs to the claim.", 9)
    )
    make_pdf("notes.pdf", "Notes in Context", [notes])

    math = (
        text(48, 520, "Boolean Expressions", 24, bold=True)
        + text(48, 475, "The following formula negates only A.")
        + text(176, 403, "A + B = C", 18)
        + b"0.85 w 176 418 m 188 418 l S\n"
        + text(48, 356, "The symbols × and · remain distinct.")
    )
    make_pdf("math.pdf", "Boolean Expressions", [math])
    make_pdf("blank.pdf", "Blank Fixture", [b""])


if __name__ == "__main__":
    main()
