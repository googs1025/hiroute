#![forbid(unsafe_code)]

use std::path::PathBuf;

use hiroute_product_e2e::generated_product_contract_files;

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("e2e/product/schema"));
    if arguments.next().is_some() {
        eprintln!("usage: generate-product-contract [output-directory]");
        std::process::exit(2);
    }
    std::fs::create_dir_all(&output).expect("create Product E2E schema output directory");
    for file in generated_product_contract_files() {
        std::fs::write(output.join(file.relative_path), file.contents)
            .expect("write generated Product E2E contract");
    }
}
