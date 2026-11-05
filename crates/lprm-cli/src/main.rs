use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "lprm", version, about = "Local Pull Request Manager")]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("lprm {}", lprm_core::VERSION);
}
