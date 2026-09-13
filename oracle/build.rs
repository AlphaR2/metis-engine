use {
    codama::Codama,
    std::{env, fs, path::Path},
};

fn main() {
    println!("cargo:rerun-if-changed=src/");

    if let Err(error) = generate_idl() {
        println!("cargo:warning=Failed to generate Codama IDL: {error}");
    }
}

fn generate_idl() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")?;
    let crate_path = Path::new(&manifest_dir);
    let codama = Codama::load(crate_path)?;
    let idl_json = codama.get_json_idl()?;

    let parsed: serde_json::Value = serde_json::from_str(&idl_json)?;
    let mut formatted_json = serde_json::to_string_pretty(&parsed)?;
    formatted_json.push('\n');

    let idl_dir = crate_path
        .parent()
        .expect("oracle has a workspace parent")
        .join("idl");
    fs::create_dir_all(&idl_dir)?;
    fs::write(idl_dir.join("oracle.json"), formatted_json)?;
    Ok(())
}
