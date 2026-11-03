//! Run with: cargo run -p asusio-core --example lighting_preview -- /path/to/lighting.json
use asusio_core::lighting::{self, Request, Store};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open(std::env::args_os().nth(1).map(Into::into))?;
    let plan = store.plan(&Request::new(lighting::ALL, [128, 192, 255]))?;
    for target in &plan {
        println!("{}: {}", target.alias(), lighting::hex(target.output()));
    }
    // Applications may explicitly call lighting::execute(plan) to apply it.
    // This example deliberately only previews and performs no color writes.
    Ok(())
}
