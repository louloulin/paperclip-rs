use std::{env, fs, path::PathBuf};

fn main() {
    let migrations_dir = PathBuf::from("migrations/drizzle");
    println!("cargo:rerun-if-changed={}", migrations_dir.display());

    let mut files = fs::read_dir(&migrations_dir)
        .expect("read migrations directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect::<Vec<_>>();
    files.sort();

    let entries = files
        .iter()
        .map(|path| {
            let name = path.file_name().expect("migration filename").to_string_lossy();
            format!(
                "MigrationSource {{ name: {name:?}, sql: include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/migrations/drizzle/{name}\")) }},"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let generated = format!("&[\n{entries}\n]");

    let output = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("drizzle_migrations.rs");
    fs::write(output, generated).expect("write generated migration list");
}
