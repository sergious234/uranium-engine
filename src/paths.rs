use std::path::PathBuf;

fn base_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("uranium-engine")
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("uranium-engine")
}

pub fn data_dir() -> PathBuf {
    base_dir()
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("uranium-engine")
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn db_file() -> PathBuf {
    data_dir().join("data.db")
}

pub fn instances_dir() -> PathBuf {
    data_dir().join("instances")
}

pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::create_dir_all(data_dir())?;
    std::fs::create_dir_all(cache_dir())?;
    std::fs::create_dir_all(instances_dir())?;
    Ok(())
}
