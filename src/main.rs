mod ui;

use std::error::Error;

fn handle(input: &str) -> String {
    format!("User message: {}", input)
}

fn main() -> Result<(), Box<dyn Error>> {
    ui::run(handle)
}
