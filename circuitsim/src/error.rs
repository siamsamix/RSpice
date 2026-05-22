use thiserror::Error;

#[derive(Debug, Error)]
pub enum SimError {
    #[error("netlist parse error: {0}")]
    Parse(String),

    #[error("circuit error: {0}")]
    Circuit(String),

    #[error("analysis error: {0}")]
    Analysis(String),

    #[error("linear algebra error: {0}")]
    Algebra(String),
}

pub type Result<T> = std::result::Result<T, SimError>;
