# /// script
# requires-python = ">=3.11"
# dependencies = ["pymupdf4llm", "pymupdf", "pillow"]
# ///
"""
Convert PDFs to markdown with ASCII art for figures.

Usage:
    uv run pdf_to_markdown.py --input-dir papers/ --output-dir papers/ [--ascii-width 60]
"""

import argparse
import io
import sys
from pathlib import Path

import fitz  # PyMuPDF
import pymupdf4llm
from PIL import Image

ASCII_CHARS = "@%#*+=-:. "


def image_to_ascii(img_bytes: bytes, width: int = 60) -> str:
    """Convert image bytes to ASCII art approximation."""
    try:
        img = Image.open(io.BytesIO(img_bytes))
        img = img.convert("L")  # Convert to grayscale

        # Calculate new dimensions maintaining aspect ratio
        aspect_ratio = img.height / img.width
        new_height = int(width * aspect_ratio * 0.5)  # 0.5 for character aspect ratio

        if new_height < 1:
            new_height = 1
        if new_height > 50:  # Cap height
            new_height = 50
            width = int(new_height / (aspect_ratio * 0.5))

        img = img.resize((width, new_height))

        # Map pixels to ASCII characters
        pixels = list(img.getdata())
        ascii_lines = []
        for i in range(0, len(pixels), width):
            row = pixels[i : i + width]
            line = "".join(
                ASCII_CHARS[min(p * len(ASCII_CHARS) // 256, len(ASCII_CHARS) - 1)]
                for p in row
            )
            ascii_lines.append(line)

        return "\n".join(ascii_lines)
    except Exception as e:
        return f"[Image conversion failed: {e}]"


def extract_figures_as_ascii(pdf_path: Path, ascii_width: int = 60) -> list[str]:
    """Extract all figures from PDF and convert to ASCII art."""
    figures = []

    try:
        doc = fitz.open(pdf_path)

        for page_num, page in enumerate(doc):
            images = page.get_images()

            for img_index, img in enumerate(images):
                try:
                    xref = img[0]
                    base_image = doc.extract_image(xref)

                    if base_image and base_image.get("image"):
                        ascii_art = image_to_ascii(
                            base_image["image"], width=ascii_width
                        )
                        figures.append(
                            {
                                "page": page_num + 1,
                                "index": img_index + 1,
                                "ascii": ascii_art,
                                "ext": base_image.get("ext", "unknown"),
                            }
                        )
                except Exception as e:
                    figures.append(
                        {
                            "page": page_num + 1,
                            "index": img_index + 1,
                            "ascii": f"[Failed to extract: {e}]",
                            "ext": "error",
                        }
                    )

        doc.close()
    except Exception as e:
        print(f"  Error extracting figures: {e}", file=sys.stderr)

    return figures


def convert_pdf_to_markdown(
    pdf_path: Path, output_path: Path, ascii_width: int = 60
) -> bool:
    """Convert a PDF to markdown with ASCII art figures."""
    try:
        # Get markdown text using pymupdf4llm
        md_text = pymupdf4llm.to_markdown(
            str(pdf_path),
            write_images=False,
            embed_images=False,
        )

        # Extract and convert figures to ASCII
        figures = extract_figures_as_ascii(pdf_path, ascii_width)

        # Append figures section if any were extracted
        if figures:
            md_text += "\n\n---\n\n## Figures (ASCII Approximation)\n\n"
            for fig in figures:
                md_text += f"### Figure {fig['page']}.{fig['index']}\n\n"
                md_text += f"```\n{fig['ascii']}\n```\n\n"

        # Write output
        output_path.write_text(md_text, encoding="utf-8")
        return True

    except Exception as e:
        print(f"  Error converting {pdf_path.name}: {e}", file=sys.stderr)
        return False


def convert_all(input_dir: Path, output_dir: Path, ascii_width: int = 60) -> dict:
    """Convert all PDFs in directory to markdown."""
    output_dir.mkdir(parents=True, exist_ok=True)

    stats = {
        "total": 0,
        "converted": 0,
        "failed": 0,
        "skipped": 0,
        "converted_files": [],
        "failed_files": [],
    }

    pdf_files = list(input_dir.glob("*.pdf"))
    stats["total"] = len(pdf_files)

    print(f"Converting {len(pdf_files)} PDFs to markdown...", file=sys.stderr)

    for pdf_path in pdf_files:
        output_path = output_dir / f"{pdf_path.stem}.md"

        if output_path.exists():
            stats["skipped"] += 1
            stats["converted_files"].append(str(output_path))
            continue

        print(f"  Converting: {pdf_path.name}", file=sys.stderr)

        if convert_pdf_to_markdown(pdf_path, output_path, ascii_width):
            stats["converted"] += 1
            stats["converted_files"].append(str(output_path))
        else:
            stats["failed"] += 1
            stats["failed_files"].append(str(pdf_path))

    return stats


def main():
    parser = argparse.ArgumentParser(
        description="Convert PDFs to markdown with ASCII art figures"
    )
    parser.add_argument(
        "--input-dir", type=Path, required=True, help="Directory containing PDFs"
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="Directory to save markdown files",
    )
    parser.add_argument(
        "--ascii-width",
        type=int,
        default=60,
        help="Width of ASCII art figures (default: 60)",
    )
    args = parser.parse_args()

    if not args.input_dir.exists():
        print(f"Error: Input directory does not exist: {args.input_dir}", file=sys.stderr)
        sys.exit(1)

    # Convert PDFs
    stats = convert_all(args.input_dir, args.output_dir, args.ascii_width)

    # Print summary
    print("", file=sys.stderr)
    print("Conversion Summary:", file=sys.stderr)
    print(f"  Total PDFs: {stats['total']}", file=sys.stderr)
    print(f"  Converted: {stats['converted']}", file=sys.stderr)
    print(f"  Already existed: {stats['skipped']}", file=sys.stderr)
    print(f"  Failed: {stats['failed']}", file=sys.stderr)


if __name__ == "__main__":
    main()
