//! R518: Generate TypeScript types from the real pc-openapi DTO registry.
//! Run with: `cargo run -p pc-typescript-gen --example gen_types > types.ts`
//! Then verify with `tsc --noEmit --strict types.ts`.

use pc_openapi::{register_core_dtos, OpenApiRegistry};
use pc_typescript_gen::generate_typescript_types;

fn main() {
    let mut reg = OpenApiRegistry::builder();
    register_core_dtos(&mut reg);
    let spec = reg.build();
    let ts = generate_typescript_types(&spec);
    print!("{ts}");
}
