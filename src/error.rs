//! Errores internos del decodificador. La API pública los traduce a `OpusError` (`lib.rs`).

use core::fmt;

/// Error interno de cualquier capa del decodificador. Los mensajes de `Display` se conservan en
/// inglés, como en la versión original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Frecuencia de salida no admitida (las admitidas son 8, 12, 16, 24 y 48 kHz).
    InvalidSampleRate(u32),
    /// Número de canales no admitido (1 o 2).
    InvalidChannels(u8),
    /// Paquete mayor que el máximo admitido: `max` bytes, recibidos `got`.
    PacketTooLarge { max: usize, got: usize },
    /// El búfer de salida no basta: hacían falta `needed` muestras y tiene `got`.
    OutputTooSmall { needed: usize, got: usize },
    /// Paquete mal formado o incoherente.
    BadPacket,
    /// Estado interno no previsto (por ejemplo, una configuración que no se decodifica).
    NotImplemented,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidSampleRate(fs) => write!(f, "invalid Opus output sample rate: {fs}"),
            Error::InvalidChannels(ch) => write!(f, "invalid Opus channel count: {ch}"),
            Error::PacketTooLarge { max, got } => {
                write!(f, "Opus packet too large (max {max}, got {got})")
            }
            Error::OutputTooSmall { needed, got } => {
                write!(f, "output buffer too small: needed {needed}, got {got}")
            }
            Error::BadPacket => write!(f, "invalid Opus packet"),
            Error::NotImplemented => write!(f, "not implemented"),
        }
    }
}

impl std::error::Error for Error {}
