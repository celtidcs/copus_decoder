//! Ayudantes compartidos por varias pruebas de integración (`mod common;`).
//!
//! Cada prueba se compila como un crate aparte y usa solo una parte de este módulo, así que el
//! resto le parece código muerto. Por eso se permite `dead_code` aquí, y solo aquí.
#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Datos de un vector que usan las pruebas y las mediciones de rendimiento.
#[derive(Debug, Clone)]
pub struct VectorSpec {
    /// Ruta relativa del flujo `.bit`.
    pub bit: String,
    /// Frecuencia de salida con la que se decodifica.
    pub sample_rate: u32,
    /// Canales de salida con los que se decodifica.
    pub channels: usize,
}

/// Carpeta de los vectores, relativa a la raíz del crate. Es la que crean
/// `documentacion/infraestructura/preparar-entorno.ps1` y `.sh`. La misma constante está en
/// `src/test_vectors.rs`, para las pruebas unitarias; las dos deben coincidir.
const DEFAULT_VECTORS_DIR: &str = "material_secundario/vectores/opus_newvectors";
/// Variable de entorno que, si está definida, sustituye a la carpeta por defecto.
const VECTORS_DIR_ENV: &str = "OPUS_TESTVECTORS_DIR";
/// Bytes de cada campo de la cabecera de un registro de `opus_demo`: longitud y `final_range`,
/// cada uno un `u32` big-endian.
const FIELD_BYTES: usize = 4;

/// Un registro de un flujo `.bit` de `opus_demo`.
pub struct OpusDemoRecord {
    /// Carga del paquete; `None` si el registro marca un paquete perdido (longitud 0).
    pub packet: Option<Vec<u8>>,
    /// Estado final del decodificador de rango que obtuvo el decodificador de referencia.
    pub expected_final_range: u32,
}

/// Carpeta de los vectores: `OPUS_TESTVECTORS_DIR` si está definida y, si no, la carpeta por
/// defecto dentro del crate.
pub fn vectors_dir() -> PathBuf {
    std::env::var_os(VECTORS_DIR_ENV).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_VECTORS_DIR),
        PathBuf::from,
    )
}

/// Lee del manifiesto de los vectores de RFC la descripción de `vector_name` (por ejemplo,
/// `testvector07`).
pub fn load_vector_spec(vector_name: &str) -> io::Result<VectorSpec> {
    let manifest = fs::read_to_string(vectors_dir().join("vectors.txt"))?;
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut fields = line.split_whitespace();
        let name = fields.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing vector name in manifest",
            )
        })?;
        if name != vector_name {
            continue;
        }

        let bit = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing .bit path"))?
            .to_string();
        let _dec = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing .dec path"))?;
        let sample_rate = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing sample rate"))?
            .parse::<u32>()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let channels = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing channel count"))?
            .parse::<usize>()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        return Ok(VectorSpec {
            bit,
            sample_rate,
            channels,
        });
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("vector not found in manifest: {vector_name}"),
    ))
}

/// Paquetes del flujo `.bit` del vector oficial `vector_name` (por ejemplo, `testvector07`),
/// sin los registros de pérdida.
pub fn load_packets(vector_name: &str) -> io::Result<Vec<Vec<u8>>> {
    let spec = load_vector_spec(vector_name)?;
    let packets = read_opus_demo_stream(&vectors_dir().join(spec.bit))?;
    Ok(packets.into_iter().flatten().collect())
}

/// Lee un flujo `.bit` de `opus_demo`: registros de longitud (`u32` BE), `final_range` (`u32` BE)
/// y la carga.
///
/// Devuelve `UnexpectedEof` si el archivo acaba a mitad de un registro.
pub fn read_opus_demo_records(path: &Path) -> io::Result<Vec<OpusDemoRecord>> {
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

/// Lee solo las cargas de un flujo `.bit`; `None` marca un paquete perdido.
pub fn read_opus_demo_stream(path: &Path) -> io::Result<Vec<Option<Vec<u8>>>> {
    Ok(read_opus_demo_records(path)?
        .into_iter()
        .map(|r| r.packet)
        .collect())
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

/// Construye un paquete de multiflujo con los `sub_packets` en orden de flujo: todos menos el
/// último con el prefijo de tamaño autodelimitado.
pub fn build_multistream_packet(sub_packets: &[&[u8]]) -> Vec<u8> {
    let mut packet = Vec::new();
    for (stream_idx, sub_packet) in sub_packets.iter().enumerate() {
        if stream_idx + 1 != sub_packets.len() {
            packet.extend(encode_self_delimited_size(sub_packet.len()));
        }
        packet.extend_from_slice(sub_packet);
    }
    packet
}

/// Codifica la longitud `len` de un subpaquete en el campo autodelimitado, de uno o dos bytes.
fn encode_self_delimited_size(len: usize) -> Vec<u8> {
    if len < 252 {
        vec![u8::try_from(len).expect("short self-delimited size fits in u8")]
    } else {
        let remainder = len - 252;
        vec![
            u8::try_from(252 + (remainder & 0x03))
                .expect("self-delimited prefix first byte fits in u8"),
            u8::try_from(remainder >> 2).expect("self-delimited prefix second byte fits in u8"),
        ]
    }
}
