use thiserror::Error;

#[derive(Error, Debug)]
pub enum NautikError {
    #[error("Module error [{module}]: {message}")]
    Module {
        module: String,
        message: String,
    },

    #[error("Hardware profile error: {0}")]
    Hardware(String),

    #[error("Wayfinder indexing error: {0}")]
    Wayfinder(String),

    #[error("Lemonade bridge error: {0}")]
    Lemonade(String),

    #[error("Fleet coordination error: {0}")]
    Fleet(String),

    #[error("Storage/Database error: {0}")]
    Storage(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
