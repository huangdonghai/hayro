use hayro_syntax::Pdf;
use std::env;
use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <input-folder>", args[0]);
        std::process::exit(2);
    }
    let input = PathBuf::from(&args[1]);
    let mut paths: Vec<PathBuf> = WalkDir::new(&input)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path().to_path_buf())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        })
        .collect();
    paths.sort();

    for path in paths {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                println!("READ_ERROR\t{}\t{}", path.display(), err);
                continue;
            }
        };
        match Pdf::new(bytes) {
            Ok(pdf) => println!("OK\t{}\t{}", path.display(), pdf.pages().len()),
            Err(err) => println!("HAYRO_FAIL\t{}\t{:?}", path.display(), err),
        }
    }
}
