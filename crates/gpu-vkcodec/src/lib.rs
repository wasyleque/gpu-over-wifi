//! Interpreter kodeka struktur Vulkana sterowany metadanymi (`gpu_proto::meta`).
//! Klient koduje struktury z pamięci aplikacji; serwer odtwarza je w arenie z poprawnymi wskaźnikami.

pub mod codec;
pub mod len;

use gpu_wire::WireError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    Wire(WireError),
    /// Nieobsługiwane wyrażenie długości.
    BadLen(&'static str),
    /// Nieobsługiwany rodzaj członka (pole bitowe, typ zewnętrzny, wskaźnik na wskaźnik innego typu...).
    Unsupported(String),
    UnknownStype(u32),
    UnknownStruct(String),
    /// Przekroczony limit (rozmiar areny, głębokość, długość łańcucha, liczba elementów).
    Limit(&'static str),
    /// Uchwyt, którego tłumacz nie zna.
    Handle(u64),
}

impl From<WireError> for CodecError {
    fn from(e: WireError) -> Self {
        CodecError::Wire(e)
    }
}
