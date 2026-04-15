fn main() {
    embuild::espidf::sysenv::output();

    if std::path::Path::new(".env").exists() {
        println!("cargo:rerun-if-changed=.env");
        for line in std::fs::read_to_string(".env").unwrap().lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                if !value.is_empty() {
                    println!("cargo:rustc-env={key}={value}");
                }
            }
        }
    }
}
