//! What stops a read of the vendored C source.

/// A place in a C source file the reader could not make sense of. Every
/// variant names the file and the line, so a report of one points at the
/// source rather than at the reader.
#[derive(Debug, thiserror::Error)]
pub enum CError {
    #[error("{file}:{line}: unterminated {what}")]
    Unterminated {
        file: String,
        line: u32,
        what: &'static str,
    },

    #[error("{file}:{line}: {text:?} is not an integer this reader can evaluate")]
    BadNumber {
        file: String,
        line: u32,
        text: String,
    },

    #[error("{file}:{line}: {name} is not a known constant")]
    UnknownSymbol {
        file: String,
        line: u32,
        name: String,
    },

    #[error("{file}:{line}: expected {want}, found {found}")]
    Expected {
        file: String,
        line: u32,
        want: &'static str,
        found: String,
    },

    #[error(
        "{file}:{line}: {name} has two definitions in the source, and no preprocessor here to choose"
    )]
    Ambiguous {
        file: String,
        line: u32,
        name: String,
    },

    #[error("{file}: no array named {name} with an initializer")]
    NoArray { file: String, name: String },

    #[error("{file}: {name} declares {declared} entries and initializes {actual}")]
    TooManyEntries {
        file: String,
        name: String,
        declared: i64,
        actual: usize,
    },

    #[error("{file}: struct {name} has fields {actual:?}, not {expected:?}")]
    StructShape {
        file: String,
        name: String,
        expected: Vec<String>,
        actual: Vec<String>,
    },

    #[error("{file}: no struct named {name}")]
    NoStruct { file: String, name: String },

    #[error("reading {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("writing {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}
