#![forbid(unsafe_code)]

use std::path::PathBuf;

use hiroute_application_api::generated_contract_files;

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("contracts/cli"));
    if arguments.next().is_some() {
        eprintln!("usage: generate-cli-contract [output-directory]");
        std::process::exit(2);
    }
    std::fs::create_dir_all(&output).expect("create contract output directory");
    for file in generated_contract_files() {
        std::fs::write(output.join(file.relative_path), file.contents)
            .expect("write generated CLI contract");
    }
}
