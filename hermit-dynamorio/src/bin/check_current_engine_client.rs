//! Finite read-only ABI/source guard for the real current native client.
fn main() {
    match hermit_dynamorio::check_current_engine_client() {
        Ok(imports) => {
            println!(
                "current shared-engine client: {} actual callbacks defined",
                imports.len()
            );
            for symbol in imports {
                println!("{symbol}");
            }
        }
        Err(error) => {
            eprintln!("current shared-engine client refused: {error:#}");
            std::process::exit(1);
        }
    }
}
