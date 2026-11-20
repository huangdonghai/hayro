import argparse
import json
import math
import os
import re
from pathlib import Path

import numpy as np
import pypdfium2 as pdfium
from PIL import Image

Image.MAX_IMAGE_PIXELS = None


def stable_id(path: Path, input_dir: Path) -> str:
    relative = path.relative_to(input_dir)
    return "__".join(relative.with_suffix("").parts)


def canvas_sample_scale(width: float, height: float) -> float:
    largest_side = max(width, height)
    area = width * height
    side_scale = 2048 / max(1.0, largest_side)
    area_scale = math.sqrt(4_000_000 / max(1.0, area))
    return min(1.0, max(0.25, min(side_scale, area_scale)))


def render_pdfium(
    pdf_path: Path,
    output_dir: Path,
    input_dir: Path,
    limit=None,
    scale_mode="native",
    render_scale=None,
):
    pdf = pdfium.PdfDocument(str(pdf_path))
    results = []
    stem = stable_id(pdf_path, input_dir)
    page_count = len(pdf) if limit is None else min(limit, len(pdf))
    try:
        pdf.init_forms()
    except Exception:
        pass
    for index in range(page_count):
        page = pdf[index]
        width, height = page.get_size()
        if render_scale is not None:
            scale = min(1.0, max(0.25, render_scale))
        else:
            scale = canvas_sample_scale(width, height) if scale_mode == "canvas" else 1.0
        bitmap = page.render(
            scale=scale,
            rotation=0,
            may_draw_forms=True,
            fill_color=(255, 255, 255, 255),
        )
        image = bitmap.to_pil().convert("RGB")
        expected_size = (math.ceil(width * scale), math.ceil(height * scale))
        if image.size != expected_size:
            image = image.resize(expected_size, Image.Resampling.LANCZOS)
        path = output_dir / f"{stem}_p{index}.png"
        image.save(path)
        results.append((index, width, height, path))
    pdf.close()
    return results


def compare_images(hayro_path: Path, pdfium_path: Path):
    with Image.open(hayro_path) as image:
        hayro = np.asarray(image.convert("RGB"), dtype=np.int16)
    with Image.open(pdfium_path) as image:
        pdfium = np.asarray(image.convert("RGB"), dtype=np.int16)
    size_mismatch = hayro.shape != pdfium.shape
    if size_mismatch:
        height = min(hayro.shape[0], pdfium.shape[0])
        width = min(hayro.shape[1], pdfium.shape[1])
        hayro = hayro[:height, :width]
        pdfium = pdfium[:height, :width]
    diff = np.abs(hayro - pdfium)
    per_pixel = diff.max(axis=2)
    total = per_pixel.size
    return {
        "size_mismatch": size_mismatch,
        "width": int(hayro.shape[1]),
        "height": int(hayro.shape[0]),
        "mse": float(np.mean(np.square(diff, dtype=np.float64))),
        "mae": float(np.mean(diff)),
        "changed_pixels_over_3": int(np.count_nonzero(per_pixel > 3)),
        "changed_pixels_over_32": int(np.count_nonzero(per_pixel > 32)),
        "max_difference": int(per_pixel.max()),
        "total_pixels": int(total),
    }


def parse_page_paths(values):
    pages = {}
    pattern = re.compile(r"^(.*)_p(\d+)$")
    for value in values:
        match = pattern.match(value.stem)
        if not match:
            continue
        stem, index = match.group(1), int(match.group(2))
        pages.setdefault(stem, {})[index] = value
    return pages


def write_report(
    rows,
    report_path: Path,
    input_dir: Path,
    hayro_dir: Path,
    pdfium_dir: Path,
    source_name="Hayro",
):
    rows.sort(key=lambda row: (-row["mae"], row["file"]))
    source_folder = hayro_dir.name if hayro_dir != report_path.parent else "hayro"
    pdfium_folder = pdfium_dir.name if pdfium_dir != report_path.parent else "pdfium"
    payload = []
    for row in rows:
        payload.append(
            {
                **row,
                "hayro_url": f"{source_folder}/{row['hayro_image']}",
                "pdfium_url": f"{pdfium_folder}/{row['pdfium_image']}",
            }
        )
    html = """<!doctype html><html><meta charset="utf-8"><title>Hayro vs PDFium</title>
<style>body{font:14px system-ui;margin:20px;background:#111;color:#eee}table{border-collapse:collapse;width:100%}th,td{padding:6px;border-bottom:1px solid #333;text-align:left;vertical-align:top}th{position:sticky;top:0;background:#222}.bad{color:#ff8a80}.warn{color:#ffd180}.ok{color:#a5d6a7}img{width:240px;image-rendering:auto}input{width:100%;padding:8px;margin-bottom:10px;background:#222;color:#eee;border:1px solid #444}a{color:#80d8ff}</style>
<h1>Hayro vs PDFium</h1><input id="q" placeholder="Filter by file name"><table><thead><tr><th>File / page</th><th>MAE</th><th>MSE</th><th>&gt;3 px</th><th>&gt;32 px</th><th>Max</th><th>Size</th><th>Hayro</th><th>PDFium</th></tr></thead><tbody id="body"></tbody></table>
<script>const rows=__ROWS__;function fmt(n){return n.toFixed(2)}function cell(v){return `<span class="${v===0?'ok':v<1?'warn':'bad'}">${fmt(v)}</span>`}function draw(q=''){const b=document.getElementById('body');b.innerHTML=rows.filter(r=>r.file.toLowerCase().includes(q.toLowerCase())).map(r=>`<tr><td>${r.file}<br>page ${r.page}</td><td>${cell(r.mae)}</td><td>${fmt(r.mse)}</td><td>${r.changed_pixels_over_3}/${r.total_pixels}</td><td>${r.changed_pixels_over_32}</td><td>${r.max_difference}</td><td>${r.width}x${r.height}${r.size_mismatch?' MISMATCH':''}</td><td><img loading="lazy" src="${r.hayro_url}"></td><td><img loading="lazy" src="${r.pdfium_url}"></td></tr>`).join('')}document.getElementById('q').addEventListener('input',e=>draw(e.target.value));draw()</script></html>"""
    html = html.replace("__ROWS__", json.dumps(payload)).replace(
        "Hayro", source_name
    )
    report_path.write_text(html, encoding="utf-8")


def rewrite_report(report_path: Path):
    text = report_path.read_text(encoding="utf-8")
    rows = json.loads(re.search(r"const rows=(\[.*?\]);function", text).group(1))
    for row in rows:
        row.pop("hayro_url", None)
        row.pop("pdfium_url", None)
    write_report(rows, report_path, report_path.parent, report_path.parent, report_path.parent)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("input_dir")
    parser.add_argument("hayro_dir")
    parser.add_argument("output_dir")
    parser.add_argument("--limit", type=int, default=1)
    parser.add_argument("--max-files", type=int)
    parser.add_argument("--rewrite-report", action="store_true")
    parser.add_argument("--scale-mode", choices=("native", "canvas"), default="native")
    parser.add_argument("--render-scale", type=float)
    parser.add_argument("--source-name", default="Hayro")
    args = parser.parse_args()

    if args.rewrite_report:
        rewrite_report(Path(args.output_dir) / "report.html")
        print(Path(args.output_dir) / "report.html")
        return

    input_dir = Path(args.input_dir)
    hayro_dir = Path(args.hayro_dir)
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    pdfium_dir = output_dir / "pdfium"
    pdfium_dir.mkdir(parents=True, exist_ok=True)

    hayro_pages = parse_page_paths(hayro_dir.glob("*.png"))
    pdf_paths = sorted(input_dir.rglob("*.pdf"))
    if args.max_files is not None:
        pdf_paths = pdf_paths[: args.max_files]

    rows = []
    failures = []
    for pdf_index, pdf_path in enumerate(pdf_paths, 1):
        stem = stable_id(pdf_path, input_dir)
        hayro_pages_for_file = hayro_pages.get(stem, {})
        if not hayro_pages_for_file:
            continue
        try:
            rendered = render_pdfium(
                pdf_path,
                pdfium_dir,
                input_dir,
                args.limit,
                args.scale_mode,
                args.render_scale,
            )
        except Exception as error:
            failures.append({"file": stem, "error": str(error)})
            continue
        for index, width, height, pdfium_path in rendered:
            hayro_path = hayro_pages_for_file.get(index)
            if hayro_path is None:
                continue
            metrics = compare_images(hayro_path, pdfium_path)
            rows.append(
                {
                    "file": stem,
                    "page": index,
                    "hayro_image": hayro_path.name,
                    "pdfium_image": pdfium_path.name,
                    **metrics,
                }
            )
        if pdf_index % 50 == 0:
            print(f"processed {pdf_index}/{len(pdf_paths)} files", flush=True)

    report_path = output_dir / "report.html"
    write_report(
        rows,
        report_path,
        input_dir,
        hayro_dir,
        pdfium_dir,
        args.source_name,
    )
    summary = {
        "files": len(pdf_paths),
        "compared_pages": len(rows),
        "failures": failures,
        "average_mae": float(np.mean([row["mae"] for row in rows])) if rows else 0.0,
        "pages_mae_over_3": sum(row["mae"] > 3 for row in rows),
        "pages_mae_over_10": sum(row["mae"] > 10 for row in rows),
        "pages_with_size_mismatch": sum(row["size_mismatch"] for row in rows),
        "report": str(report_path),
    }
    (output_dir / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
