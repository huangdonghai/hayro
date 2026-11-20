import argparse
from pathlib import Path

import pypdfium2 as pdfium


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("input_dir")
    parser.add_argument("hayro_load_tsv")
    args = parser.parse_args()

    input_dir = Path(args.input_dir)
    failures = []
    for line in Path(args.hayro_load_tsv).read_text(encoding="utf-8-sig").splitlines():
        if line.startswith("HAYRO_FAIL\t"):
            _, path, reason = line.split("\t", 2)
            failures.append((Path(path), reason))

    for path, reason in failures:
        label = path.relative_to(input_dir).as_posix()
        try:
            pdf = pdfium.PdfDocument(str(path))
            page_count = len(pdf)
            rendered = 0
            render_error = ""
            try:
                pdf.init_forms()
            except Exception:
                pass
            try:
                for index in range(page_count):
                    pdf[index].render(scale=1).to_pil().convert("RGB")
                    rendered += 1
            except Exception as error:
                render_error = f" render_error={error}"
            pdf.close()
            print(
                f"PDFIUM_OK\t{label}\tpages={page_count}\trendered={rendered}"
                f"{render_error}\thayro={reason}"
            )
        except Exception as error:
            print(f"PDFIUM_FAIL\t{label}\t{error}\thayro={reason}")


if __name__ == "__main__":
    main()
