use opalc::dummy_compile;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = if let Some(path) = args.get(1) {
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("error: could not read `{path}`: {e}");
            std::process::exit(1);
        })
    } else {
        eprintln!("usage: opal-cli <file.opal>");
        std::process::exit(1);
    };

    dummy_compile(&source);
}
