//! Acceso a los vectores oficiales de RFC 8251 desde las pruebas unitarias del crate.
//!
//! Las pruebas de integración tienen su propia copia en `tests/common/mod.rs`, porque no
//! pueden ver los módulos `#[cfg(test)]` de la biblioteca. Las dos deben coincidir.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Carpeta de los vectores, relativa a la raíz del crate. Es la que crean
/// `documentacion/infraestructura/preparar-entorno.ps1` y `.sh`.
const DEFAULT_VECTORS_DIR: &str = "material_secundario/vectores/opus_newvectors";
/// Variable de entorno que, si está definida, sustituye a la carpeta por defecto.
const VECTORS_DIR_ENV: &str = "OPUS_TESTVECTORS_DIR";
/// Bytes de cada campo de la cabecera de un registro de `opus_demo`: longitud y `final_range`,
/// cada uno un `u32` big-endian.
const FIELD_BYTES: usize = 4;

/// Un registro de un flujo `.bit` de `opus_demo`.
pub(crate) struct OpusDemoRecord {
    /// Carga del paquete; `None` si el registro marca un paquete perdido (longitud 0).
    pub(crate) packet: Option<Vec<u8>>,
    /// Estado final del decodificador de rango que obtuvo el decodificador de referencia.
    pub(crate) expected_final_range: u32,
}

/// Carpeta de los vectores: `OPUS_TESTVECTORS_DIR` si está definida y, si no, la carpeta por
/// defecto dentro del crate.
pub(crate) fn vectors_dir() -> PathBuf {
    std::env::var_os(VECTORS_DIR_ENV).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_VECTORS_DIR),
        PathBuf::from,
    )
}

/// Lee un flujo `.bit` de `opus_demo`: registros de longitud (`u32` BE), `final_range` (`u32` BE)
/// y la carga.
///
/// Devuelve `UnexpectedEof` si el archivo acaba a mitad de un registro.
pub(crate) fn read_opus_demo_records(path: &Path) -> io::Result<Vec<OpusDemoRecord>> {
    let bytes = fs::read(path)?;
    let mut records = Vec::new();
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        let packet_len = take_u32_be(&mut rest, "truncated packet_len")? as usize;
        let expected_final_range = take_u32_be(&mut rest, "truncated final_range")?;
        if rest.len() < packet_len {
            return Err(truncated("truncated packet"));
        }
        let (payload, tail) = rest.split_at(packet_len);
        rest = tail;
        records.push(OpusDemoRecord {
            packet: (packet_len > 0).then(|| payload.to_vec()),
            expected_final_range,
        });
    }
    Ok(records)
}

fn take_u32_be(rest: &mut &[u8], what: &'static str) -> io::Result<u32> {
    let (field, tail) = rest
        .split_first_chunk::<FIELD_BYTES>()
        .ok_or_else(|| truncated(what))?;
    *rest = tail;
    Ok(u32::from_be_bytes(*field))
}

fn truncated(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, what)
}
