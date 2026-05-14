#[derive(Debug)]
pub enum AppError {
    IoError(String),
    DecodeError(String),
    KeyNotFound(String),
    InternalError(String),
}
