fn main() {
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let src = std::path::Path::new("partitions.csv");
        if src.exists() {
            let dst = std::path::Path::new(&out_dir).join("partitions.csv");
            if let Err(err) = std::fs::copy(src, &dst) {
                panic!("failed to copy partitions.csv into OUT_DIR: {err}");
            }
            println!("cargo:rerun-if-changed=partitions.csv");
        }
    }
    embuild::espidf::sysenv::output();
}
