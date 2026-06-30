use std::{collections::HashMap, env, path::PathBuf};

// Bake .env values into the binary as compile-time env (so server-side
// std::env! / option_env! lookups work even when the process env is sparse).
// Mirrors OrderTracker's build.rs.
fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let env_path = PathBuf::from(&manifest_dir).join(".env");

    println!("cargo:rerun-if-changed={}", env_path.display());

    let mut vars: HashMap<String, String> = HashMap::new();
    if env_path.exists() {
        for item in dotenvy::from_path_iter(&env_path).expect("Failed to read .env file") {
            let (key, val) = item.expect("Failed to parse .env entry");
            vars.insert(key, val);
        }
    } else {
        eprintln!("Warning: .env not found at {}", env_path.display());
    }

    for (key, val) in vars {
        println!("cargo:rustc-env={}={}", key, val);
    }
}
